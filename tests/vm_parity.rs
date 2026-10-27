// Parity: the bytecode VM must produce the same variable state as the
// tree-walking interpreter for every program in the supported subset.

use veldt::interpreter::{Interpreter, Value};
use veldt::lexer::Lexer;
use veldt::parser::Parser;
use veldt::vm::Vm;

fn interp_vars(src: &str) -> Interpreter {
    let mut lex = Lexer::new(src);
    let tokens = lex.tokenize().unwrap();
    let mut p = Parser::new(tokens);
    let stmts = p.parse_program().unwrap();
    let mut i = Interpreter::new();
    i.run(&stmts).unwrap();
    i
}

fn vm_globals(src: &str) -> Vm {
    let mut lex = Lexer::new(src);
    let tokens = lex.tokenize().unwrap();
    let mut p = Parser::new(tokens);
    let stmts = p.parse_program().unwrap();
    let mut v = Vm::new();
    v.run(&stmts, 10_000_000).unwrap();
    v
}

fn get_vm<'a>(v: &'a Vm, name: &str) -> Option<Value> {
    v.globals.get(name).cloned()
}
fn get_i(i: &Interpreter, name: &str) -> Option<Value> {
    i.vars.borrow().get(name).cloned()
}

fn parity(src: &str, names: &[&str]) {
    // VM frames + JIT'd recursion consume far more native stack per
    // level than the tree-walker — Rust test threads default to ~2MB
    // vs the CLI's 8MB main stack, so fib(15)-depth recursion
    // overflowed the harness (the CLI itself ran fine). Run the
    // comparison on a generously-stacked thread instead.
    let src = src.to_string();
    let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
    std::thread::Builder::new().stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let i = interp_vars(&src);
            let v = vm_globals(&src);
            for n in &names {
                assert_eq!(get_i(&i, n), get_vm(&v, n), "var {} diverged", n);
            }
        }).unwrap().join().unwrap();
}

#[test]
fn arithmetic_parity() {
    parity(r#"
        let a = 3 + 4 * 2
        let b = 10 % 3
        let c = -5 + 8
        let d = 2.5 * 4
        let e = (a > b) and (b < 10)
        let f = a ?? 99
    "#, &["a","b","c","d","e","f"]);
}

#[test]
fn loops_parity() {
    parity(r#"
        let total = 0
        let i = 0
        while i < 20 {
            total = total + i * 2
            i = i + 1
        }
        let acc = 0
        for x in [1,2,3,4] {
            acc = acc + x * x
        }
    "#, &["total","acc"]);
}

#[test]
fn fns_and_closures_parity() {
    parity(r#"
        fn fib(n: int) { if n < 2 { return n } return fib(n-1)+fib(n-2) }
        let f = fib(15)
        let mk = fn(n: int) { return fn(x: int) { return x + n } }
        let a5 = mk(5)
        let g = a5(10)
    "#, &["f","g"]);
}

#[test]
fn collections_parity() {
    parity(r#"
        let xs = [1,2,3]
        xs.append(4)
        let d = dict()
        d["k"] = 7
        d.insert("j", 8)
        let m = xs.map(fn(v) { return v * 2 })
        let l = xs.len()
        let s = "hello"[1:4]
        let last = xs[-1]
        let dk = d["k"]
    "#, &["l","s","last","dk","xs","d","m"]);
}

#[test]
fn match_and_ops_parity() {
    parity(r#"
        let [a, b] = [9, 8]
        let out = 0
        match a { 9 => out = 1, else => out = 2 }
        let n = null ?? 42
        let has = 3 in [1,2,3]
    "#, &["a","b","out","n","has"]);
}

#[test]
fn try_catch_parity() {
    parity(r#"
        let err = ""
        try { let x = 1 / 0 } catch e { err = e }
        let ok = 7
    "#, &["ok"]);
}

#[test]
fn jit_engages_on_hot_fn() {
    let src = r#"
        fn hot(x: int) { return x * 2 + 1 }
        let acc = 0
        for i in range(300) { acc = acc + hot(i) }
    "#;
    let mut v = vm_globals(src);
    let _ = &mut v;
    assert!(v.fns["hot"][0].jit.borrow().is_some(),
        "hot() should have been JIT-compiled after 64 calls");
}

#[test]
fn jit_loop_variant_matches_vm() {
    // The evolved iterative fib — heavy slot usage + backward jumps.
    let src = r#"
        fn fib(n: int) {
            if n <= 1 { return n }
            let a = 0
            let b = 1
            let i = 2
            while i <= n {
                let tmp = a + b
                a = b
                b = tmp
                i = i + 1
            }
            return b
        }
        let r = 0
        for i in range(200) { r = fib(15) }
        let rr = r
    "#;
    let i = interp_vars(src);
    let v = vm_globals(src);
    assert_eq!(get_i(&i, "rr"), get_vm(&v, "rr"),
        "iterative fib diverged: JIT'd loop body wrong");
}

/// Smoke coverage: every example in examples/ runs to completion
/// under --vm (exit 0). Nondeterministic ones (rand/time) are
/// checked for exit success, not output equality.
#[test]
fn vm_all_examples_run() {
    let veldt = env!("CARGO_BIN_EXE_veldt");
    let mut failures = Vec::new();
    // Providers (examples that `grow` garden functions/structs) run
    // first — consumers like ecosystem/use_fib depend on them.
    let mut paths: Vec<_> = std::fs::read_dir("examples").unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str())
            == Some("veldt"))
        .collect();
    paths.sort_by_key(|p| {
        let src = std::fs::read_to_string(p).unwrap_or_default();
        !src.contains("grow ")
    });
    // hello.veldt is the documented garden bootstrap ("run hello.veldt
    // first") — but it's also a provider itself (grow fn double), so the
    // alphabetical provider pass runs ecosystem BEFORE it and shout is
    // missing. Seed it explicitly first.
    let hello = paths.iter().find(|p| p.ends_with("hello.veldt"))
        .map(|p| p.clone());
    if let Some(h) = &hello {
        let _ = std::process::Command::new(veldt)
            .args(["run", h.to_str().unwrap(), "--vm",
                   "--steps", "500000000"])
            .output().expect("seed hello.veldt");
    }
    // data_pipeline reads /tmp/pipeline_in.txt — a fixture the repo
    // doesn't ship; write it so the example actually runs
    let _ = std::fs::write("/tmp/pipeline_in.txt",
        "alpha\nbeta\ngamma\nbeta\nalpha\nalpha\n");
    for p in paths {
        // skip examples that need stdin/args/serve-loop
        let name = p.file_name().unwrap().to_str().unwrap();
        if matches!(name,
            "web_server.veldt" | "climb.veldt" | "playable.veldt" | "depths.veldt")
        { continue; }
        if hello.as_ref().map(|h| *p == *h).unwrap_or(false) { continue; }
        let out = std::process::Command::new(veldt)
            .args(["run", p.to_str().unwrap(), "--vm",
                   "--steps", "500000000"])
            .output().expect("spawn veldt --vm");
        if !out.status.success() {
            failures.push(format!(
                "{}: {}", name,
                String::from_utf8_lossy(&out.stderr)
                    .lines().last().unwrap_or("?")));
        }
    }
    assert!(failures.is_empty(),
        "examples failing under --vm:\n{}", failures.join("\n"));
}

#[test]
fn jit_recursion_result_not_sentinel() {
    // Regression: a JIT-compiled recursive fn returned Int(4294967295)
    // — the nok() -1 sentinel leaking as a boxed Value instead of the
    // callee's real result (in fib it feeds back into `n` → unbounded
    // recursion → stack overflow). Depth >64 so the JIT actually
    // engages. Left failing until the JIT lane fixes the leak.
    parity(r#"
        fn count(n: int) { if n > 200 { return n } return count(n+1) }
        let f = count(0)
    "#, &["f"]);
}
