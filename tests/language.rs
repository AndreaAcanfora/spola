//! The language, one feature at a time: each test runs a program and checks what it printed or returned.

use spola::{Data, Outcome, Vm, compile};

/// Runs `src` to the end. Effects return their first argument, so `await echo(x)` evaluates to `x`.
fn run(src: &str) -> Result<(String, Data), String> {
    let program = compile(src).map_err(|e| e.to_string())?;
    let mut vm = Vm::new(program);
    let mut out = String::new();
    loop {
        let outcome = vm.run(None);
        out.push_str(&vm.take_output());
        match outcome.map_err(|e| e.to_string())? {
            Outcome::Done(v) => return Ok((out, v)),
            Outcome::Effect { args, .. } => vm.resume(&args.first().cloned().unwrap_or(Data::Nil)).unwrap(),
            Outcome::Paused => unreachable!(),
        }
    }
}

fn output(src: &str) -> String {
    match run(src) {
        Ok((out, _)) => out,
        Err(e) => panic!("{e}\n--- program:\n{src}"),
    }
}

fn value(src: &str) -> Data {
    run(src).unwrap_or_else(|e| panic!("{e}")).1
}

fn error(src: &str) -> String {
    match run(src) {
        Ok((out, v)) => panic!("expected an error, got {v:?} after printing {out:?}"),
        Err(e) => e,
    }
}

#[test]
fn arithmetic_and_precedence() {
    assert_eq!(value("return 1 + 2 * 3 - 4 / 2"), Data::Int(5));
    assert_eq!(value("return (1 + 2) * 3"), Data::Int(9));
    assert_eq!(value("return 7 / 2"), Data::Int(3));
    assert_eq!(value("return 7.0 / 2"), Data::Float(3.5));
    assert_eq!(value("return -7 % 3"), Data::Int(2));
    assert_eq!(value("return 2 + 0.5"), Data::Float(2.5));
    assert_eq!(value("return --3"), Data::Int(3));
    assert_eq!(value("return 1 == 1.0"), Data::Bool(true));
    assert_eq!(value("return not 1 < 2"), Data::Bool(false));
    assert_eq!(value("return \"b\" > \"a\""), Data::Bool(true));
}

#[test]
fn logic_short_circuits() {
    assert_eq!(
        output("fn loud(x) { print(x)\n return x }\nprint(loud(false) and loud(true))"),
        "false\nfalse\n"
    );
    assert_eq!(
        output("fn loud(x) { print(x)\n return x }\nprint(loud(1) or loud(2))"),
        "1\n1\n"
    );
    assert_eq!(value("return nil or \"default\""), Data::Str("default".into()));
    assert_eq!(
        value("return 0 and \"zero is truthy\""),
        Data::Str("zero is truthy".into())
    );
}

#[test]
fn variables_and_scopes() {
    let src = r#"
        let x = "global"
        {
            let x = "block"
            print(x)
        }
        print(x)
        fn shadow(x) { x = "param"; return x }
        print(shadow(1), x)
    "#;
    assert_eq!(output(src), "block\nglobal\nparam global\n");
}

#[test]
fn strings_interpolate_expressions() {
    assert_eq!(
        output(r#"let n = 3; print("{n} * 2 = {n * 2}, {"nested {n}"}")"#),
        "3 * 2 = 6, nested 3\n"
    );
    assert_eq!(output(r#"print("braces: \{ \}")"#), "braces: { }\n");
    assert_eq!(
        output(r#"print("list {[1, "a"]} map {{"k": nil}}")"#),
        "list [1, \"a\"] map {k: nil}\n"
    );
}

#[test]
fn control_flow() {
    let src = r#"
        let out = []
        let i = 0
        while true {
            i += 1
            if i % 2 == 0 { continue }
            if i > 9 { break }
            push(out, i)
        }
        print(out)
        for c in "abc" {
            if c == "b" { continue }
            print(c)
        }
        let total = 0
        for k in {a: 1, b: 2} { total += {a: 1, b: 2}[k] }
        print(total)
        if false { print("no") }
        else if nil { print("no") }
        else { print("else") }
    "#;
    assert_eq!(output(src), "[1, 3, 5, 7, 9]\na\nc\n3\nelse\n");
}

#[test]
fn break_inside_nested_scopes_pops_locals() {
    let src = r#"
        let found = nil
        for row in [[1, 2], [3, 4], [5, 6]] {
            let sum = 0
            for x in row {
                let doubled = x * 2
                if doubled == 8 { found = x; break }
                sum += doubled
            }
            if found { break }
        }
        let after = "stack is balanced"
        print(found, after)
    "#;
    assert_eq!(output(src), "4 stack is balanced\n");
}

#[test]
fn functions_and_recursion() {
    let src = r#"
        fn fact(n) { if n <= 1 { return 1 } return n * fact(n - 1) }
        fn even(n) { if n == 0 { return true } return odd(n - 1) }
        fn odd(n) { if n == 0 { return false } return even(n - 1) }
        print(fact(20), even(10), odd(7))
        fn nothing() {}
        print(nothing())
    "#;
    assert_eq!(output(src), "2432902008176640000 true true\nnil\n");
}

#[test]
fn closures_share_captured_variables() {
    let src = r#"
        fn make() {
            let n = 0
            let inc = fn() { n += 1; return n }
            let get = fn() { return n }
            return [inc, get]
        }
        let pair = make()
        pair[0]()
        pair[0]()
        print(pair[1]())

        let fns = []
        for i in range(3) { push(fns, fn() { return i * 10 }) }
        print(fns[0](), fns[1](), fns[2]())

        fn outer() {
            let x = 1
            fn middle() {
                fn inner() { x += 1; return x }
                return inner
            }
            let f = middle()
            f()
            return x
        }
        print(outer())

        fn local_recursion() {
            fn down(n) { if n == 0 { return "done" } return down(n - 1) }
            return down(5)
        }
        print(local_recursion())
    "#;
    assert_eq!(output(src), "2\n0 10 20\n2\ndone\n");
}

#[test]
fn lists_and_maps() {
    let src = r#"
        let xs = [3, 1, 2]
        xs[0] = 30
        xs[-1] += 5
        push(xs, 4)
        print(xs, len(xs), sort(xs), reverse(xs), slice(xs, 1, -1), contains(xs, 4))
        let m = {"a key": 1, b: [1, 2]}
        m.c = "new"
        m["a key"] += 1
        m.b[0] = 9
        print(m, keys(m), has(m, "c"), m.missing, remove(m, "c"), len(m))
        print([1, [2]] == [1, [2]], {a: 1} == {a: 1}, {a: 1} != {a: 2}, [1] + [2])
    "#;
    let expected = concat!(
        "[30, 1, 7, 4] 4 [1, 4, 7, 30] [4, 7, 1, 30] [1, 7] true\n",
        "{\"a key\": 2, b: [9, 2]} [\"a key\", \"b\", \"c\"] true nil new 2\n",
        "true true true [1, 2]\n"
    );
    assert_eq!(output(src), expected);
}

#[test]
fn builtins() {
    let src = r#"
        print(str(1.5), int("42"), float("2.5"), type([]), type(print), type(nil))
        print(join(split("a,b,c", ","), "-"), split("  two   words "), upper("é"), trim("  x "))
        print(range(3), range(2, 8, 3), range(5, 0, -2))
        print(abs(-3), min(3, 1, 2), max([1, 9, 4]), floor(2.7), round(2.5), sqrt(16))
        print(json({a: [1, 2.5, "x"], b: nil}), parse_json("[1, \{\"k\": true\}]"))
        print(repr("quoted"), slice("hello", -3), int(3.9), insert([1, 3], 1, 2))
    "#;
    let expected = concat!(
        "1.5 42 2.5 list function nil\n",
        "a-b-c [\"two\", \"words\"] É x\n",
        "[0, 1, 2] [2, 5] [5, 3, 1]\n",
        "3 1 9 2 3 4.0\n",
        "{\"a\":[1,2.5,\"x\"],\"b\":null} [1, {k: true}]\n",
        "\"quoted\" llo 3 [1, 2, 3]\n"
    );
    assert_eq!(output(src), expected);
}

#[test]
fn await_passes_values_through_the_host() {
    let src = r#"
        let a = await echo(20)
        let b = await echo({nested: [a, "x"]})
        return [a + 1, b.nested[1]]
    "#;
    assert_eq!(value(src), Data::List(vec![Data::Int(21), Data::Str("x".into())]));
}

#[test]
fn semicolons_are_optional_and_newlines_continue_expressions() {
    let src = "let total = 1 +\n  2 +\n  3\nlet xs = [\n  1,\n  2,\n]\nlet m = {\n  a: 1,\n}\nprint(total, xs, m); print(\"one line\")\nif true {\n  print(\"x\")\n}\nelse {\n  print(\"y\")\n}";
    assert_eq!(output(src), "6 [1, 2] {a: 1}\none line\nx\n");
}

#[test]
fn runtime_errors_say_where() {
    let e = error("fn f(x) {\n  return x + nil\n}\nf(1)");
    assert!(e.contains("cannot add int and nil"), "{e}");
    assert!(e.contains("in f (line 2)") && e.contains("in <script> (line 4)"), "{e}");
    assert!(error("print(undefined_thing)").contains("undefined variable `undefined_thing`"));
    assert!(error("x = 1").contains("undeclared variable `x`"));
    assert!(error("return [1][5]").contains("index 5 out of range for length 1"));
    assert!(error("return 1 / 0").contains("division by zero"));
    assert!(error("return 9223372036854775807 + 1").contains("integer overflow"));
    assert!(error("fn f(a) {}\nf()").contains("f takes 1 argument but got 0"));
    assert!(error("fn f() { return f() }\nf()").contains("stack overflow"));
    assert!(error("assert(1 > 2, \"math is broken\")").contains("assert: math is broken"));
    assert!(error("for x in 5 {}").contains("cannot loop over int"));
    assert!(error("await echo(fn() {})").contains("a function cannot leave the program"));
}

#[test]
fn syntax_errors_say_where() {
    let cases = [
        ("let = 1", "1:5", "expected a variable name, found `=`"),
        ("print(1", "1:8", "expected `)` or `,`"),
        ("1 < 2 < 3", "1:7", "comparisons cannot be chained"),
        ("break", "1:1", "`break` outside a loop"),
        ("let s = \"abc", "1:9", "unterminated string"),
        ("1 + 2 = 3", "1:7", "can only assign to a variable"),
        ("fn f(a, a) {}", "1:9", "duplicate parameter `a`"),
        ("await 5", "1:7", "expected an effect name"),
        ("if !x {}", "1:4", "use `not`"),
        ("print(\"{}\")", "1:8", "empty `{}`"),
        ("print(\"{1 +}\")", "1:12", "expected an expression"),
    ];
    for (src, at, msg) in cases {
        let e = compile(src).unwrap_err().to_string();
        assert!(
            e.contains(at) && e.contains(msg),
            "{src:?} gave {e:?}, wanted {at} {msg:?}"
        );
    }
}

#[test]
fn garbage_is_collected() {
    let src = r#"
        let keep = []
        for i in range(200000) {
            let garbage = [i, {k: "temporary {i}"}]
            if i % 1000 == 0 { push(keep, garbage) }
        }
        return len(keep)
    "#;
    let mut vm = Vm::new(compile(src).unwrap());
    assert_eq!(vm.run(None).unwrap(), Outcome::Done(Data::Int(200)));
    vm.compact();
    assert!(vm.heap_size() < 1500, "heap holds {} objects", vm.heap_size());
}

#[test]
fn deep_values_do_not_overflow_the_native_stack() {
    let src = r#"
        let x = []
        for i in range(5000) { x = [x] }
        let y = []
        for i in range(5000) { y = [y] }
        print(len(str(x)) > 0)
        return x == y
    "#;
    let (out, v) = run(src).unwrap();
    assert_eq!(out, "true\n");
    assert_eq!(v, Data::Bool(false));
}
