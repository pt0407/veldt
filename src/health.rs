// Health checking, trial runs, and healing for the Veldt ecosystem

use crate::ast::*;
use crate::interpreter::{Interpreter, Value};
use crate::veldt::{FunctionEntry, Health, Veldt};
use std::time::Instant;

pub struct HealthChecker;

impl HealthChecker {
    /// Check a newly grown function: parse, type check, trial run.
    /// Takes a snapshot of the veldt's functions/vars/structs to avoid borrow conflicts.
    pub fn check_at_birth(entry: &mut FunctionEntry, veldt: &Veldt) {
        // Clone the data we need from the veldt to avoid borrow conflicts
        let functions_snapshot = veldt.functions.clone();
        let variables_snapshot = veldt.variables.clone();
        let structs_snapshot = veldt.structs.clone();

        let trial_result = Self::run_trial(entry, &functions_snapshot, &variables_snapshot, &structs_snapshot);

        match trial_result {
            TrialResult::Pass(score) => {
                entry.health = Health::Healthy;
                entry.trial_score = score;
                entry.fitness = score;
            }
            TrialResult::Fail(err) => {
                entry.health = Health::Sick(err);
                entry.trial_score = 0.0;
                entry.fitness = 0.0;
            }
        }
    }

    fn run_trial(entry: &FunctionEntry,
                 functions: &std::collections::HashMap<String, Vec<FunctionEntry>>,
                 variables: &std::collections::HashMap<String, crate::veldt::VariableEntry>,
                 structs: &std::collections::HashMap<String, crate::veldt::StructEntry>,
    ) -> TrialResult {
        let start = Instant::now();

        if !entry.tests.is_empty() {
            return Self::run_inline_tests(entry, functions, variables, structs, start);
        }

        Self::run_safe_defaults(entry, functions, variables, structs, start)
    }

    fn load_snapshot_into_interp(
        functions: &std::collections::HashMap<String, Vec<FunctionEntry>>,
        variables: &std::collections::HashMap<String, crate::veldt::VariableEntry>,
        structs: &std::collections::HashMap<String, crate::veldt::StructEntry>,
        interp: &mut Interpreter,
    ) {
        for (name, variants) in functions {
            for entry in variants {
                if matches!(entry.health, Health::Healthy | Health::Sick(_)) {
                    let variant = crate::interpreter::FunctionVariant {
                        id: entry.id,
                        def: crate::interpreter::FunctionDef {
                            params: entry.params.clone(),
                            body: entry.body.clone(),
                        },
                    };
                    interp.functions.entry(name.clone())
                        .or_insert_with(Vec::new)
                        .push(variant);
                }
            }
        }
        for (name, entry) in variables {
            if let Ok(val) = serde_json::from_str::<Value>(&entry.value) {
                interp.vars.insert(name.clone(), val);
            }
        }
        for (name, entry) in structs {
            interp.structs.insert(name.clone(), entry.fields.clone());
        }
    }

    fn run_inline_tests(entry: &FunctionEntry,
                        functions: &std::collections::HashMap<String, Vec<FunctionEntry>>,
                        variables: &std::collections::HashMap<String, crate::veldt::VariableEntry>,
                        structs: &std::collections::HashMap<String, crate::veldt::StructEntry>,
                        start: Instant) -> TrialResult {
        let mut interp = Interpreter::new();
        Self::load_snapshot_into_interp(functions, variables, structs, &mut interp);

        // Load the function being tested
        let variant = crate::interpreter::FunctionVariant {
            id: entry.id,
            def: crate::interpreter::FunctionDef {
                params: entry.params.clone(),
                body: entry.body.clone(),
            },
        };
        interp.functions.entry(entry.name.clone())
            .or_insert_with(Vec::new)
            .push(variant);

        let mut passed = 0;
        let mut total = 0;

        for test in &entry.tests {
            total += 1;
            match (interp.eval_expr_in_scope(&test.call),
                   interp.eval_expr_in_scope(&test.expected)) {
                (Ok(call_val), Ok(expected_val)) => {
                    if Self::values_equal(&call_val, &expected_val) {
                        passed += 1;
                    }
                }
                (Err(e), _) => return TrialResult::Fail(format!("Test call failed: {}", e)),
                (_, Err(e)) => return TrialResult::Fail(format!("Test expected value failed: {}", e)),
            }
        }

        let elapsed = start.elapsed().as_secs_f64();
        let correctness = passed as f64 / total as f64;
        let speed_bonus = (1.0 / (1.0 + elapsed * 10.0)).max(0.0);
        let score = correctness + speed_bonus * 0.1;

        if passed == total {
            TrialResult::Pass(score)
        } else {
            TrialResult::Fail(format!("{} of {} tests failed", total - passed, total))
        }
    }

    fn run_safe_defaults(entry: &FunctionEntry,
                         functions: &std::collections::HashMap<String, Vec<FunctionEntry>>,
                         variables: &std::collections::HashMap<String, crate::veldt::VariableEntry>,
                         structs: &std::collections::HashMap<String, crate::veldt::StructEntry>,
                         start: Instant) -> TrialResult {
        let mut interp = Interpreter::new();
        Self::load_snapshot_into_interp(functions, variables, structs, &mut interp);

        let variant = crate::interpreter::FunctionVariant {
            id: entry.id,
            def: crate::interpreter::FunctionDef {
                params: entry.params.clone(),
                body: entry.body.clone(),
            },
        };
        interp.functions.entry(entry.name.clone())
            .or_insert_with(Vec::new)
            .push(variant);

        let args: Vec<Value> = entry.params.iter().map(|p| Self::safe_default(&p.typ)).collect();

        match interp.call_function(&entry.name, Some(entry.id), args) {
            Ok(_) => {
                let elapsed = start.elapsed().as_secs_f64();
                let speed_bonus = (1.0 / (1.0 + elapsed * 10.0)).max(0.0);
                TrialResult::Pass(0.5 + speed_bonus * 0.1)
            }
            Err(e) => TrialResult::Fail(e),
        }
    }

    fn eval_test_expr(interp: &mut Interpreter, expr: &Expr) -> Result<Value, String> {
        interp.eval_expr_in_scope(expr)
    }

    fn safe_default(typ: &Type) -> Value {
        match typ {
            Type::Int => Value::Int(0),
            Type::Str => Value::Str("".into()),
            Type::Bool => Value::Bool(true),
            Type::List(_) => Value::List(vec![]),
            Type::Fn => Value::Null,
            Type::Struct(_) => Value::Null,
        }
    }

    fn values_equal(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::List(a), Value::List(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| Self::values_equal(x, y))
            }
            (Value::Null, Value::Null) => true,
            _ => false,
        }
    }

    // === Healing ===

    /// Attempt to heal a sick function. Returns true if healed.
    pub fn heal(entry: &mut FunctionEntry, veldt: &Veldt) -> bool {
        let attempt = format!("Healing attempt for {}#{}", entry.name, entry.id);
        entry.healing_attempts.push(attempt);

        // Find a healthy healer in the same lineage (same-purpose)
        let same_purpose_healer = Self::find_same_purpose_healer(entry, veldt);
        if let Some(healer) = same_purpose_healer {
            // Hybrid merge: blend the sick function's unique parts with the healer's working parts
            return Self::hybrid_merge(entry, healer);
        }

        // Find a different-purpose healer (similar structure, different name)
        let diff_purpose_healer = Self::find_diff_purpose_healer(entry, veldt);
        if let Some(healer) = diff_purpose_healer {
            // Patching: borrow type annotations or structural patterns
            return Self::patch(entry, healer);
        }

        false
    }

    fn find_same_purpose_healer<'a>(entry: &FunctionEntry, veldt: &'a Veldt) -> Option<&'a FunctionEntry> {
        let variants = veldt.functions.get(&entry.name)?;
        variants.iter()
            .find(|v| v.id != entry.id && matches!(v.health, Health::Healthy))
    }

    fn find_diff_purpose_healer<'a>(entry: &FunctionEntry, veldt: &'a Veldt) -> Option<&'a FunctionEntry> {
        // Find a healthy function with the same number of params but different name
        for (name, variants) in &veldt.functions {
            if name == &entry.name { continue; }
            for v in variants {
                if matches!(v.health, Health::Healthy) && v.params.len() == entry.params.len() {
                    return Some(v);
                }
            }
        }
        None
    }

    /// Hybrid merge: blend sick code's unique parts with healthy code's working parts.
    /// Since we can't do semantic merging without an LLM, we use a simple strategy:
    /// - If the sick function has a different body structure than the healer,
    ///   keep the sick function's body but borrow the healer's type annotations.
    /// - If the bodies are identical, the sick function is a duplicate — replace
    ///   with healer's body (effectively a clone, but it will compete and lose).
    /// - If the sick function's body is empty or trivially broken, replace with
    ///   healer's body.
    fn hybrid_merge(sick: &mut FunctionEntry, healer: &FunctionEntry) -> bool {
        // Strategy: borrow type annotations from healer
        let mut changed = false;
        for (i, param) in sick.params.iter_mut().enumerate() {
            if i < healer.params.len() {
                let healer_type = &healer.params[i].typ;
                if param.typ != *healer_type {
                    param.typ = healer_type.clone();
                    changed = true;
                }
            }
        }

        // If the sick body is empty or very short, use the healer's body
        if sick.body.len() <= 1 {
            sick.body = healer.body.clone();
            changed = true;
        }

        // If something changed, mark as healthy
        if changed {
            sick.health = Health::Healthy;
            sick.lifespan = 100.0;
            return true;
        }

        // If nothing to merge, the merge degrades to replacement
        sick.body = healer.body.clone();
        sick.health = Health::Healthy;
        sick.lifespan = 100.0;
        true
    }

    /// Patching: borrow type annotations or structural patterns from a different-purpose healer.
    fn patch(sick: &mut FunctionEntry, healer: &FunctionEntry) -> bool {
        // Type grafting: borrow type annotations
        let mut changed = false;
        for (i, param) in sick.params.iter_mut().enumerate() {
            if i < healer.params.len() {
                let healer_type = &healer.params[i].typ;
                if param.typ != *healer_type {
                    param.typ = healer_type.clone();
                    changed = true;
                }
            }
        }

        if changed {
            sick.health = Health::Healthy;
            sick.lifespan = 100.0;
            return true;
        }

        false
    }
}

enum TrialResult {
    Pass(f64),
    Fail(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::veldt::Veldt;
    use std::path::PathBuf;

    fn make_veldt() -> Veldt {
        let mut v = Veldt::with_path(PathBuf::from("/tmp/veldt_health_test"));
        v.clear();
        v
    }

    fn check_entry(veldt: &mut Veldt, name: &str) {
        // Clone the entry, check it, put it back — avoids borrow conflicts
        let mut entry = veldt.functions.get(name).unwrap()[0].clone();
        HealthChecker::check_at_birth(&mut entry, veldt);
        veldt.functions.get_mut(name).unwrap()[0] = entry;
    }

    #[test]
    fn test_healthy_function_passes_trial() {
        let mut veldt = make_veldt();
        veldt.grow_function("add", vec![
            Param { name: "a".into(), typ: Type::Int },
            Param { name: "b".into(), typ: Type::Int },
        ], vec![Stmt::Return(Some(Expr::BinOp(
            Box::new(Expr::Var("a".into())),
            BinOp::Add,
            Box::new(Expr::Var("b".into())),
        )))], vec![], "fn add(a: int, b: int) { return a + b }");

        check_entry(&mut veldt, "add");
        let entry = &veldt.functions["add"][0];
        assert!(matches!(entry.health, Health::Healthy));
        assert!(entry.trial_score > 0.0);
    }

    #[test]
    fn test_sick_function_fails_trial() {
        let mut veldt = make_veldt();
        veldt.grow_function("broken", vec![
            Param { name: "a".into(), typ: Type::Int },
        ], vec![Stmt::Return(Some(Expr::BinOp(
            Box::new(Expr::Var("a".into())),
            BinOp::Add,
            Box::new(Expr::Str("bad".into())),
        )))], vec![], "fn broken(a: int) { return a + \"bad\" }");

        check_entry(&mut veldt, "broken");
        let entry = &veldt.functions["broken"][0];
        assert!(matches!(entry.health, Health::Sick(_)));
    }

    #[test]
    fn test_inline_tests_pass() {
        let mut veldt = make_veldt();
        veldt.grow_function("double", vec![
            Param { name: "x".into(), typ: Type::Int },
        ], vec![Stmt::Return(Some(Expr::BinOp(
            Box::new(Expr::Var("x".into())),
            BinOp::Mul,
            Box::new(Expr::Int(2)),
        )))], vec![TestAssert {
            call: Expr::Call("double".into(), None, vec![Expr::Int(3)]),
            expected: Expr::Int(6),
        }], "fn double(x: int) { return x * 2 }");

        check_entry(&mut veldt, "double");
        let entry = &veldt.functions["double"][0];
        assert!(matches!(entry.health, Health::Healthy));
    }

    #[test]
    fn test_inline_tests_fail() {
        let mut veldt = make_veldt();
        veldt.grow_function("bad_double", vec![
            Param { name: "x".into(), typ: Type::Int },
        ], vec![Stmt::Return(Some(Expr::BinOp(
            Box::new(Expr::Var("x".into())),
            BinOp::Add,
            Box::new(Expr::Int(1)),
        )))], vec![TestAssert {
            call: Expr::Call("bad_double".into(), None, vec![Expr::Int(3)]),
            expected: Expr::Int(6),
        }], "fn bad_double(x: int) { return x + 1 }");

        check_entry(&mut veldt, "bad_double");
        let entry = &veldt.functions["bad_double"][0];
        assert!(matches!(entry.health, Health::Sick(_)));
    }

    #[test]
    fn test_healing_same_purpose() {
        let mut veldt = make_veldt();
        // Healthy variant
        veldt.grow_function("greet", vec![
            Param { name: "n".into(), typ: Type::Str },
        ], vec![Stmt::Print(Expr::BinOp(
            Box::new(Expr::Str("hi ".into())),
            BinOp::Add,
            Box::new(Expr::Var("n".into())),
        ))], vec![], "fn greet(n: str) { print(\"hi \" + n) }");
        veldt.functions.get_mut("greet").unwrap()[0].health = Health::Healthy;

        // Sick variant (empty body)
        veldt.grow_function("greet", vec![
            Param { name: "n".into(), typ: Type::Str },
        ], vec![], vec![], "fn greet(n: str) { }");
        veldt.functions.get_mut("greet").unwrap()[1].health = Health::Sick("empty body".into());

        // Heal: clone the sick entry, heal it, put it back
        let mut sick = veldt.functions["greet"][1].clone();
        let healed = HealthChecker::heal(&mut sick, &veldt);
        assert!(healed);
        assert!(matches!(sick.health, Health::Healthy));
    }
}
