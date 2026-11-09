<div align="center">

# Spola

**A small scripting language whose running programs can be saved, killed and resumed exactly where they stopped.**

Every `await` checkpoints the whole virtual machine (stack, heap, call frames) into a few hundred bytes.
Kill the process, reboot the machine, copy the checkpoint somewhere else: the program continues from that line,
with every variable intact.

### [▶ Try it in your browser](https://andreaacanfora.github.io/spola/)

[![CI](https://github.com/AndreaAcanfora/spola/actions/workflows/ci.yml/badge.svg)](https://github.com/AndreaAcanfora/spola/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/rust-2024-CE422B?logo=rust&logoColor=white)](Cargo.toml)
[![Dependencies](https://img.shields.io/badge/dependencies-0-success)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Donate with PayPal](https://img.shields.io/badge/donate-PayPal-00457C?logo=paypal)](https://www.paypal.com/paypalme/AndreaAca)

![The Spola playground: a countdown runs, the process is killed mid-sleep, it is restored from its checkpoint and finishes; then the timeline is dragged back in time](docs/demo.gif)

</div>

```js
// approval.sp: a workflow that waits for a human, for as long as it takes
let version = "v1.4.2"
print("✓ build {version}")

let answer = await ask("Deploy {version} to production?")   // the program stops here
if answer == "yes" {
    print("✓ deploy {version}")
}
```

```console
$ spola run approval.sp --state deploy.state --detach
✓ build v1.4.2
⏸ waiting for ask("Deploy v1.4.2 to production?") at line 5
  checkpoint: deploy.state (1.1 KB)

$ # ...three days later, on another machine...
$ spola resume deploy.state --with yes
✓ deploy v1.4.2
```

## Why

Long-running programs (deploy pipelines, onboarding flows, AI agents that wait for a human to approve a step,
jobs that sleep for a week) need to survive restarts. Today that takes a workflow engine: Temporal, Step Functions,
a queue and a database, plus rules about what your code may and may not do so that it can be replayed.

Spola makes it a property of the language instead. The VM has no pointers, only indexes into one heap, so its
entire state serializes trivially. A program is just code: loops, closures, recursion, local variables. Where it
touches the outside world it says `await`, and that is where it can be paused, saved and resumed.

## Try it

```bash
cargo install --git https://github.com/AndreaAcanfora/spola

spola run examples/countdown.sp --state countdown.state
# press Ctrl-C halfway, then run the same command again: it continues from the same number
```

```console
$ spola inspect countdown.state
checkpoint countdown.state
  status     waiting for sleep(1)
  wakes in   0.8s
  progress   77 steps, 4 effects done
  size       756 B (8 heap objects)

       9 │     push(log, n)
      10 │     print("T-{n}")
  →   11 │     await sleep(1)
      12 │ }

in <script> (line 11)
    n = 7
    launched = 1794231457262
    log = [10, 9, 8, 7]
```

Or skip the install and [open the playground](https://andreaacanfora.github.io/spola/): run a program, press
**Kill the process**, then **Restore from checkpoint**. Reload the page and the checkpoint is still there. Drag the
thread under the editor to travel back to any earlier instruction. A checkpoint downloaded from the browser resumes
in the terminal with `spola resume`, and the other way round.

## The language

A small dynamic language that should feel familiar after one read:

```js
fn make_counter() {
    let count = 0
    return fn() {          // closures capture variables, not copies
        count += 1
        return count
    }
}

let next = make_counter()
next()
print("counter: {next()}")                      // string interpolation: counter: 2

let person = {name: "Ada", langs: ["en", "it"]}  // maps keep insertion order
person.age = 36
for lang in person.langs {
    print("{person.name} speaks {lang}")
}

let started = await now()                        // effects: the only way out of the program
await sleep(2)
```

- Values: `nil`, booleans, 64-bit integers (overflow is an error, not a wraparound), floats, strings, lists, maps
  with string keys, functions.
- Statements: `let`, `fn`, `if`/`else`, `while`, `for x in list | map | string`, `break`, `continue`, `return`,
  compound assignment `+= -= *= /=`. Semicolons are optional.
- Built-in functions: `print len push pop insert remove keys values has str int float type range join split slice
  contains upper lower trim abs min max floor round sqrt sort reverse assert error json parse_json repr`.
- Built-ins are deterministic. Anything that reads the outside world (time, randomness, files, the network, a
  human) is an **effect**, written `await name(args)`.

### Effects

`await` hands control to the host, which performs the effect and resumes the program with the result. The
`spola` command provides:

| Effect | Returns |
| --- | --- |
| `sleep(seconds)` | `nil`, after the time has passed. Durable: a crash mid-sleep resumes with the time that was left |
| `ask(prompt)` | a line typed by the user; with `--detach`, the program saves and exits until `spola resume --with` |
| `now()` | Unix time in milliseconds |
| `random()` | a float in `[0, 1)` |
| `env(name)` | an environment variable, or `nil` |
| `exec(command)` | `{code, stdout, stderr}` of a shell command |
| `read_file(path)`, `write_file(path, text)` | the file's text; `nil` |

Embedders define their own effects: an effect is just a name and arguments that the host receives (see below).

### What "durable" means exactly

The `spola` command saves a checkpoint right before each effect and right after it completes:

- an effect that **completed** is never performed again: its result is in the checkpoint;
- an effect interrupted **while running** (a crash during `exec`) is performed again on resume, so effects are
  *at least once*, like in every other durable-execution system;
- code **between** two effects may run again after a crash, so a `print` there can appear twice.

Checkpoints are written atomically (write, fsync, rename). They carry a checksum, the program's source and a
fingerprint of the compiler: a checkpoint resumes only on the program and Spola version that made it, and a
damaged one is rejected with an error instead of crashing the VM. Every reference inside is validated on load.

## The `spola` command

```text
spola run <file.sp> [--state <checkpoint>] [--detach] [--fresh]
spola resume <checkpoint> [--with <value>]
spola inspect <checkpoint>
spola disasm <file.sp>
spola check <file.sp>
```

`<checkpoint>` is a file, or `redis://host:port/key` to keep it on any Redis-compatible server (Redis, Valkey,
[loomkv](https://github.com/AndreaAcanfora/loomkv)), so a program can stop on one machine and continue on another.
Checkpoints are binary, so the server must store values byte for byte, as all of these do; CI runs a full
stop-and-resume against a real Redis.
`--with` takes JSON (`--with '{"approved": true}'`) or plain text (`--with yes`).

## Embedding

The VM is a library with no dependencies. The host loop is a `match`:

```rust
use spola::{compile, Data, Outcome, Vm};

let mut vm = Vm::new(compile(source)?);
loop {
    match vm.run(None)? {
        Outcome::Done(value) => break,
        Outcome::Effect { name, args } => {
            // Perform it now, or save `vm.snapshot()` anywhere and come back next week:
            //   let mut vm = Vm::restore(&bytes)?;
            let result = my_effects(&name, &args)?;
            vm.resume(&result)?;
        }
        Outcome::Paused => {} // only when running with a step budget
    }
}
print!("{}", vm.take_output());
```

`vm.run(Some(n))` runs at most `n` instructions, so a host can interleave thousands of programs on one thread, or
checkpoint between instructions as well as at effects. `vm.call_stack()` returns every frame with its local
variables, which is what `spola inspect` and the playground show.

## How it works

```text
source ──lexer──▶ tokens ──parser──▶ syntax tree ──compiler──▶ bytecode ──▶ VM ⇄ checkpoint bytes
```

- **Lexer** ([`src/lexer.rs`](src/lexer.rs)): newlines end statements the way they do in Go, so semicolons are
  optional. String interpolation is lexed as source and parsed recursively, with correct positions in errors.
- **Parser** ([`src/parser.rs`](src/parser.rs)): recursive descent, with errors that say what was expected and
  what was found, and where.
- **Compiler** ([`src/compiler.rs`](src/compiler.rs)): two passes. The first finds which local variables some
  closure captures; the second emits a stack-machine bytecode, putting exactly those variables in heap cells and
  everything else in stack slots. It also records which variable lives in which slot, for the debugger views.
- **VM** ([`src/vm.rs`](src/vm.rs)): about 50 instructions. Values are an enum of `nil`, booleans, numbers and
  *indexes* into one heap arena, never pointers. A mark-and-sweep collector frees what is unreachable.
- **Checkpoints** ([`src/snapshot.rs`](src/snapshot.rs)): because there are no pointers, the heap, stack, frames
  and globals are written as they are, as varints, after a garbage collection. A program waiting on an effect is
  typically a few hundred bytes to a few kilobytes.
- **Determinism**: every built-in is a pure function; time, randomness and I/O are effects. So once effect results
  are fixed, a run can be replayed exactly. The playground's time travel uses that: it keeps a checkpoint every
  1000 instructions and the result of every effect, and rebuilds any past instruction by restoring the nearest
  checkpoint and replaying from it.
- **Browser**: the same crate compiles to WebAssembly with plain exported functions and no imports
  ([`src/wasm.rs`](src/wasm.rs)); the page is plain JavaScript.

Speed: roughly on par with CPython on recursive code (`fib(30)` takes about 0.18 s on an M-series Mac). Spola
optimizes for being small, readable and checkpointable rather than for speed.

## Development

```bash
cargo test                    # language, checkpoints, JSON
scripts/build-web.sh          # builds the playground into web/
python3 -m http.server -d web # then open http://localhost:8000
```

The checkpoint tests are the heart of the suite: they run programs with closures, recursion, maps and garbage
collection, save and restore the VM **at every single instruction**, and require the output to be identical to an
uninterrupted run. Another test flips every bit of a checkpoint and truncates it at every length, and requires each
one to be rejected cleanly.

## Ideas

- [ ] `try`/`catch`, so a program can handle a failed effect
- [ ] Effects that run in parallel (`await all(...)`)
- [ ] Migrating a waiting program to a new version of its source
- [ ] A Python package to embed the VM
- [ ] A faster dispatch loop

## Support

Spola is free and open source. If it saved you from setting up a workflow engine, you can
[buy me a coffee on PayPal](https://www.paypal.com/paypalme/AndreaAca). Thank you!

## License

[MIT](LICENSE) © Andrea Acanfora
