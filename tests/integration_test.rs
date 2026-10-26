// Integration test: end-to-end Veldt program execution

use veldt::ast::*;
use veldt::health::HealthChecker;
use veldt::interpreter::Interpreter;
use veldt::lexer::Lexer;
use veldt::parser::Parser;
use veldt::veldt::{Health, Veldt};

// We need to expose modules for integration tests
// This is done via `pub mod` in lib.rs or by making the binary also a lib

#[test]
fn test_full_program_execution() {
    let source = r#"
        let x = 5
        fn add(a: int, b: int) { return a + b }
        let result = add(3, 4)
        print(result)
        if x > 3 { print("big") } else { print("small") }
        for i in range(3) { print(i) }
    "#;

    let mut lex = Lexer::new(source);
    let tokens = lex.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let stmts = parser.parse_program().unwrap();
    let mut interp = Interpreter::new();
    interp.run(&stmts).unwrap();

    assert_eq!(interp.vars.borrow().get("result"), Some(&veldt::interpreter::Value::Int(7)));
}

#[test]
fn test_grow_and_persist() {
    use std::path::PathBuf;

    let path = PathBuf::from("/tmp/veldt_integration_test");
    // Clean start
    let mut veldt = Veldt::with_path(path.clone());
    veldt.clear();
    veldt.save().unwrap();

    // Run a program that grows a function
    let source = r#"
        grow fn greet(name: str) { print("hi " + name) }
    "#;
    let mut lex = Lexer::new(source);
    let tokens = lex.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let stmts = parser.parse_program().unwrap();
    let mut interp = Interpreter::new();
    veldt.load_into_interpreter(&mut interp);
    interp.run(&stmts).unwrap();

    // Process grows
    let grows = interp.collect_grows(&stmts);
    for item in &grows {
        if let GrowItem::Fn(name, params, body, tests) = item {
            let id = veldt.grow_function(name, params.clone(), body.clone(), tests.clone(), "fn greet");
            let mut entry = veldt.functions[name].iter().find(|e| e.id == id).cloned().unwrap();
            HealthChecker::check_at_birth(&mut entry, &veldt);
            if let Some(variants) = veldt.functions.get_mut(name) {
                for e in variants.iter_mut() {
                    if e.id == id { *e = entry.clone(); break; }
                }
            }
        }
    }
    veldt.save().unwrap();

    // Load in a new veldt instance and verify
    let mut veldt2 = Veldt::with_path(path.clone());
    veldt2.load().unwrap();
    assert!(veldt2.functions.contains_key("greet"));
}

#[test]
fn test_variant_competition() {
    use std::path::PathBuf;

    let path = PathBuf::from("/tmp/veldt_competition_test");
    let mut veldt = Veldt::with_path(path.clone());
    veldt.clear();

    // Grow two variants of the same function
    veldt.grow_function("sort", vec![], vec![Stmt::Return(Some(Expr::Int(1)))], vec![], "fn sort() { return 1 }");
    veldt.grow_function("sort", vec![], vec![Stmt::Return(Some(Expr::Int(2)))], vec![], "fn sort() { return 2 }");

    // Make variant 1 fitter (more usage)
    veldt.functions.get_mut("sort").unwrap()[0].usage_count = 10;
    veldt.functions.get_mut("sort").unwrap()[0].last_used = 1;
    veldt.functions.get_mut("sort").unwrap()[0].trial_score = 0.8;
    veldt.run_count = 1;

    let fittest = veldt.get_fittest_variant("sort").unwrap();
    assert_eq!(fittest.id, 1);
}

#[test]
fn test_cross_run_persistence() {
    use std::path::PathBuf;

    let path = PathBuf::from("/tmp/veldt_cross_run_test");

    // Run 1: grow a function
    let mut veldt = Veldt::with_path(path.clone());
    veldt.clear();
    veldt.grow_function("helper", vec![
        Param { name: "x".into(), typ: Type::Int },
    ], vec![Stmt::Return(Some(Expr::BinOp(
        Box::new(Expr::Var("x".into())),
        BinOp::Mul,
        Box::new(Expr::Int(2)),
    )))], vec![], "fn helper(x: int) { return x * 2 }");
    veldt.save().unwrap();

    // Run 2: load and use the grown function
    let mut veldt2 = Veldt::with_path(path.clone());
    veldt2.load().unwrap();
    assert!(veldt2.functions.contains_key("helper"));

    let mut interp = Interpreter::new();
    veldt2.load_into_interpreter(&mut interp);

    // Call the function from the veldt
    let result = interp.call_function("helper", None, vec![veldt::interpreter::Value::Int(21)]);
    assert_eq!(result.unwrap(), veldt::interpreter::Value::Int(42));
}

fn run_src(source: &str) -> Interpreter {
    let mut lex = Lexer::new(source);
    let tokens = lex.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let stmts = parser.parse_program().unwrap();
    let mut interp = Interpreter::new();
    interp.run(&stmts).unwrap();
    interp
}

#[test]
fn test_append_insert_dict_index() {
    let i = run_src(r#"
        let xs = []
        xs.append(1)
        xs.append(2)
        let d = dict()
        d.insert("a", 1)
        d["b"] = 2
        let s = set()
        s.insert(5)
    "#);
    let xs_v = i.vars.borrow().get("xs").cloned().unwrap();
    let d_v = i.vars.borrow().get("d").cloned().unwrap();
    match xs_v {
        veldt::interpreter::Value::List(l) => assert_eq!(l.len(), 2),
        _ => panic!("xs not a list"),
    }
    match d_v {
        veldt::interpreter::Value::Dict(m) => {
            assert!(m.contains_key("a") && m.contains_key("b"));
        }
        _ => panic!("d not a dict"),
    }
}

#[test]
fn test_dict_index_read_and_str_len() {
    let i = run_src(r#"
        let d = dict()
        d["k"] = 42
        let got = d["k"]
        let sl = "hello".len()
        let xs = [10, 20, 30]
        let last = xs[-1]
    "#);
    assert_eq!(i.vars.borrow().get("got").cloned(), Some(veldt::interpreter::Value::Int(42)));
    assert_eq!(i.vars.borrow().get("sl").cloned(), Some(veldt::interpreter::Value::Int(5)));
    assert_eq!(i.vars.borrow().get("last").cloned(), Some(veldt::interpreter::Value::Int(30)));
}

#[test]
fn test_borrow_read_methods() {
    let i = run_src(r#"
        let d = dict()
        d["x"] = 7
        let g = d.get("x")
        let h = d.has("x")
        let n = d.len()
        let xs = [1, 2, 3]
        let e = xs.is_empty()
    "#);
    assert_eq!(i.vars.borrow().get("g").cloned(), Some(veldt::interpreter::Value::Int(7)));
    assert_eq!(i.vars.borrow().get("h").cloned(), Some(veldt::interpreter::Value::Bool(true)));
    assert_eq!(i.vars.borrow().get("n").cloned(), Some(veldt::interpreter::Value::Int(1)));
    assert_eq!(i.vars.borrow().get("e").cloned(), Some(veldt::interpreter::Value::Bool(false)));
}
