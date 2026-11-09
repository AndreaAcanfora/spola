//! The promise of Spola: a program can be checkpointed at any instruction and resumed from the bytes, and it
//! behaves exactly as if it had never stopped.

use spola::{Data, ErrorKind, Outcome, Status, Vm, compile};

const PROGRAMS: &[&str] = &[
    // closures, captured loop variables, maps and strings, all alive across effects
    r#"
    fn make_counter(start) {
        let n = start
        return fn(step) { n += step; return n }
    }
    let c = make_counter(await input(100))
    let seen = {}
    for word in split("the quick brown fox jumps over the lazy dog the end") {
        seen[word] = (seen[word] or 0) + 1
        if word == "fox" { c(await input(10)) }
    }
    let callbacks = []
    for i in range(3) { push(callbacks, fn() { return i + c(0) }) }
    print(seen, callbacks[2]())
    return {count: c(1), words: len(keys(seen))}
    "#,
    // recursion several frames deep when the effect happens
    r#"
    fn walk(tree, depth) {
        if type(tree) != "list" {
            let got = await input(tree * depth)
            return got
        }
        let total = 0
        for child in tree { total += walk(child, depth + 1) }
        return total
    }
    print("total", walk([1, [2, [3, 4]], 5], 0))
    "#,
    // garbage collection between and across checkpoints
    r#"
    let survivors = []
    for round in range(4) {
        for i in range(3000) {
            let tmp = {round: round, items: [i, "x{i}"]}
            if i % 1000 == 0 { push(survivors, tmp) }
        }
        print(round, len(survivors), await input(round))
    }
    return survivors[-1]
    "#,
];

/// Runs `src` with the effect `input(x)` returning `x`, calling `between` at every stop. A stop happens at every
/// effect and every `slice` instructions.
fn run_with(src: &str, slice: Option<u64>, mut between: impl FnMut(Vm) -> Vm) -> (String, Data) {
    let mut vm = Vm::new(compile(src).unwrap());
    let mut out = String::new();
    loop {
        let outcome = vm.run(slice).unwrap();
        out.push_str(&vm.take_output());
        match outcome {
            Outcome::Done(v) => return (out, v),
            Outcome::Paused => {}
            Outcome::Effect { name, args } => {
                assert_eq!(name, "input");
                vm = between(vm);
                vm.resume(&args[0]).unwrap();
            }
        }
        vm = between(vm);
    }
}

fn round_trip(mut vm: Vm) -> Vm {
    let bytes = vm.snapshot();
    let restored = Vm::restore(&bytes).expect("restore");
    // a restored VM saves to the very same bytes
    let mut again = restored.clone();
    assert_eq!(again.snapshot(), bytes);
    restored
}

#[test]
fn checkpoint_at_every_effect() {
    for src in PROGRAMS {
        let expected = run_with(src, None, |vm| vm);
        assert_eq!(run_with(src, None, round_trip), expected);
    }
}

#[test]
fn checkpoint_at_every_single_instruction() {
    for src in PROGRAMS.iter().take(2) {
        let expected = run_with(src, None, |vm| vm);
        assert_eq!(run_with(src, Some(1), round_trip), expected, "{src}");
    }
}

#[test]
fn checkpoint_every_few_hundred_instructions() {
    for src in PROGRAMS {
        let expected = run_with(src, None, |vm| vm);
        for slice in [7, 333, 4096] {
            assert_eq!(run_with(src, Some(slice), round_trip), expected);
        }
    }
}

#[test]
fn a_waiting_program_reports_its_effect_and_position() {
    let src = "let x = 41\nfn f(a) {\n  return await ask(\"q?\", a)\n}\nreturn f(x) + 1";
    let mut vm = Vm::new(compile(src).unwrap());
    vm.run(None).unwrap();
    let mut vm = Vm::restore(&vm.snapshot()).unwrap();
    assert_eq!(
        vm.pending(),
        Some(("ask".into(), vec![Data::Str("q?".into()), Data::Int(41)]))
    );
    assert_eq!(vm.line(), 3);
    let stack = vm.call_stack();
    assert_eq!(stack[0].function, "f");
    assert_eq!(stack[0].locals, vec![("a".to_string(), "41".to_string())]);
    assert_eq!(stack[1].function, "<script>");
    assert!(stack[1].locals.contains(&("x".to_string(), "41".to_string())));
    vm.resume(&Data::Int(1)).unwrap();
    assert_eq!(vm.run(None).unwrap(), Outcome::Done(Data::Int(2)));
}

#[test]
fn host_data_survives_checkpoints() {
    let mut vm = Vm::new(compile("await sleep(5)").unwrap());
    vm.run(None).unwrap();
    vm.host = Data::Map(vec![("sleep_until".into(), Data::Int(1_700_000_000_000))]);
    let vm = Vm::restore(&vm.snapshot()).unwrap();
    assert_eq!(
        vm.host,
        Data::Map(vec![("sleep_until".into(), Data::Int(1_700_000_000_000))])
    );
}

#[test]
fn a_failed_effect_raises_at_the_await() {
    let mut vm = Vm::new(compile("print(1)\nlet body = await fetch(\"x\")").unwrap());
    vm.run(None).unwrap();
    let e = vm.fail_effect("connection refused");
    assert_eq!(e.message, "fetch: connection refused");
    assert_eq!(e.trace[0].line, 2);
    assert_eq!(vm.status(), &Status::Failed);
}

#[test]
fn damaged_checkpoints_are_rejected_without_panicking() {
    let mut vm = Vm::new(compile(PROGRAMS[0]).unwrap());
    vm.run(Some(200)).unwrap();
    let bytes = vm.snapshot();
    for i in 0..bytes.len() {
        for bit in [0x01, 0x80] {
            let mut bad = bytes.clone();
            bad[i] ^= bit;
            let e = Vm::restore(&bad).expect_err("a flipped bit must be detected");
            assert_eq!(e.kind, ErrorKind::Snapshot);
        }
    }
    for len in 0..bytes.len() {
        assert!(Vm::restore(&bytes[..len]).is_err());
    }
    assert!(Vm::restore(b"not a checkpoint at all").is_err());
}

#[test]
fn a_checkpoint_only_resumes_its_own_program() {
    let mut vm = Vm::new(compile("await x()").unwrap());
    vm.run(None).unwrap();
    let mut bytes = vm.snapshot();
    // swap the source for another program of the same length, and fix up the checksum
    let at = bytes.windows(9).position(|w| w == b"await x()").unwrap();
    bytes[at..at + 9].copy_from_slice(b"await y()");
    let n = bytes.len() - 8;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in &bytes[..n] {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    bytes[n..].copy_from_slice(&h.to_le_bytes());
    let e = Vm::restore(&bytes).unwrap_err();
    assert!(e.message.contains("different version"), "{e}");
}

#[test]
fn checkpoints_are_small() {
    let mut vm = Vm::new(compile(PROGRAMS[0]).unwrap());
    vm.run(None).unwrap();
    let size = vm.snapshot().len();
    assert!(size < 2048, "checkpoint is {size} bytes");
}
