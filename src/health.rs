// Health checking, trial runs, and healing for the Veldt ecosystem

use crate::ast::*;
use std::rc::Rc;
use crate::interpreter::{Interpreter, Value};
use crate::veldt::{FunctionEntry, Health, Veldt};
use std::time::Instant;

pub struct HealthChecker;

impl HealthChecker {
    /// Check a newly grown function: parse, type check, trial run.
    /// Takes a snapshot of the veldt's functions/vars/structs to avoid borrow conflicts.
    pub fn check_at_birth(entry: &mut FunctionEntry, veldt: &Veldt) {
        Self::check_at_birth_with(entry, veldt, false)
    }

    /// use_vm: evaluate trials through the bytecode VM + JIT
    /// (same dispatch semantics — jit_call mirrors call_function).
    pub fn check_at_birth_with(entry: &mut FunctionEntry, veldt: &Veldt,
                               use_vm: bool) {
        // Clone the data we need from the veldt to avoid borrow conflicts
        let functions_snapshot = veldt.functions.clone();
        let variables_snapshot = veldt.variables.clone();
        let structs_snapshot = veldt.structs.clone();

        let probes_snapshot = veldt.probes.get(&entry.name).cloned().unwrap_or_default();
        let traces_snapshot = veldt.traces.get(&entry.name).cloned().unwrap_or_default();

        let trial_result = Self::run_trial(entry, &functions_snapshot, &variables_snapshot, &structs_snapshot, &probes_snapshot, &traces_snapshot, use_vm);

        match trial_result {
            TrialResult::Pass(score, steps) => {
                entry.health = Health::Healthy;
                entry.trial_score = score;
                // Fitness factors in score + efficiency (fewer steps = fitter)
                // Also factor in code size (smaller = fitter)
                let code_size = Self::count_statements(&entry.body) as f64;
                let size_factor = 1.0 / (1.0 + code_size * 0.01);
                entry.fitness = score * (0.7 + size_factor * 0.3);
            }
            TrialResult::Fail(err) => {
                entry.health = Health::Sick(err);
                entry.trial_score = 0.0;
                entry.fitness = 0.0;
            }
        }
    }

    fn run_trial(entry: &mut FunctionEntry,
                 functions: &std::collections::HashMap<String, Vec<FunctionEntry>>,
                 variables: &std::collections::HashMap<String, crate::veldt::VariableEntry>,
                 structs: &std::collections::HashMap<String, crate::veldt::StructEntry>,
                 probes: &[crate::veldt::Probe],
                 traces: &[crate::veldt::Trace],
                 use_vm: bool,
    ) -> TrialResult {
        let start = Instant::now();

        // Phase 1: author-written inline tests — must all pass
        let base = if !entry.tests.is_empty() {
            Self::run_inline_tests(entry, functions, variables, structs, start, use_vm)
        } else {
            Self::run_safe_defaults(entry, functions, variables, structs, start, use_vm)
        };

        let (mut score, steps) = match base {
            TrialResult::Fail(e) => return TrialResult::Fail(e),
            TrialResult::Pass(s, st) => (s, st),
        };

        // Phase 2: ecosystem probes — not lethal, but scored per-niche.
        // A variant that fails in some niche loses fitness there; specialists
        // can still survive by dominating their niche.
        if !probes.is_empty() {
            let niche_scores = Self::run_probes(entry, functions, variables, structs, probes, use_vm);
            let n_niches = niche_scores.len().max(1) as f64;
            let overall = niche_scores.values().sum::<f64>() / n_niches;
            entry.niche_scores = niche_scores;
            score += overall * 0.3; // up to +0.3 for full probe coverage
        }

        // Phase 3: real call traces — the strongest evidence. A variant that
        // produces results matching what users actually got earns fitness;
        // one that diverges from observed behavior loses it. Cap at 30 traces.
        if !traces.is_empty() {
            let trace_score = Self::score_traces(entry, functions, variables, structs, traces, use_vm);
            score += trace_score * 0.4; // up to +0.4 for matching real usage
        }

        // Phase 4: asymptotic efficiency — run this variant and the incumbent
        // fittest on big-scale probes, compare step counts. >1 = faster.
        if !probes.is_empty() {
            let big: Vec<&crate::veldt::Probe> = probes.iter().filter(|p| p.scale > 0).collect();
            if !big.is_empty() {
                let eff = Self::measure_efficiency(entry, functions, variables, structs, &big, use_vm);
                entry.efficiency = eff;
                // log-scale bonus: 2x faster => +0.17, 2x slower => -0.17
                let bonus = eff.ln().clamp(-1.0, 1.0) * 0.25;
                score += bonus;
            }
        }

        TrialResult::Pass(score, steps)
    }

    /// Run all probes for the family; returns niche -> fraction passed.
    fn run_probes(entry: &FunctionEntry,
                  functions: &std::collections::HashMap<String, Vec<FunctionEntry>>,
                  variables: &std::collections::HashMap<String, crate::veldt::VariableEntry>,
                  structs: &std::collections::HashMap<String, crate::veldt::StructEntry>,
                  probes: &[crate::veldt::Probe],
                  use_vm: bool) -> std::collections::HashMap<String, f64> {
        let mut interp = Interpreter::with_step_limit(10000);
        interp.caps = crate::interpreter::Caps::sandboxed();
        Self::load_snapshot_into_interp(functions, variables, structs, &mut interp);
        let mut vm = if use_vm { Self::mk_vm(entry, functions, variables, structs) } else { None };
        let variant = crate::interpreter::FunctionVariant {
            id: entry.id,
            fitness: entry.fitness,
            def: crate::interpreter::FunctionDef {
                params: Rc::new(entry.params.clone()),
                body: entry.body.clone().into(),
            },
        };
        interp.functions.entry(entry.name.clone())
            .or_insert_with(Vec::new)
            .push(variant);

        let mut per_niche: std::collections::HashMap<String, (u32, u32)> = std::collections::HashMap::new();
        for probe in probes {
            let args: Vec<Value> = probe.args.iter()
                .filter_map(|s| serde_json::from_str(s).ok())
                .collect();
            if args.len() != entry.params.len() { continue; }
            let expected: Result<Value, _> = serde_json::from_str(&probe.expected);
            let expected = match expected { Ok(v) => v, Err(_) => continue };
            let e = per_niche.entry(probe.niche.clone()).or_insert((0, 0));
            e.1 += 1;
            match Self::call_one(&mut interp, &mut vm, &entry.name, Some(entry.id), args) {
                Ok(v) if Self::values_equal(&v, &expected) => { e.0 += 1; }
                _ => {}
            }
        }
        per_niche.into_iter().map(|(k, (p, t))| (k, p as f64 / t as f64)).collect()
    }

    /// Score a variant against real call traces: fraction of traces where
    /// the variant produces the recorded result (what users actually got).
    fn score_traces(entry: &FunctionEntry,
                    functions: &std::collections::HashMap<String, Vec<FunctionEntry>>,
                    variables: &std::collections::HashMap<String, crate::veldt::VariableEntry>,
                    structs: &std::collections::HashMap<String, crate::veldt::StructEntry>,
                    traces: &[crate::veldt::Trace],
                  use_vm: bool) -> f64 {
        let mut interp = Interpreter::with_step_limit(10000);
        interp.caps = crate::interpreter::Caps::sandboxed();
        Self::load_snapshot_into_interp(functions, variables, structs, &mut interp);
        let mut vm = if use_vm { Self::mk_vm(entry, functions, variables, structs) } else { None };
        let variant = crate::interpreter::FunctionVariant {
            id: entry.id,
            fitness: entry.fitness,
            def: crate::interpreter::FunctionDef {
                params: Rc::new(entry.params.clone()),
                body: entry.body.clone().into(),
            },
        };
        interp.functions.entry(entry.name.clone())
            .or_insert_with(Vec::new)
            .push(variant);

        let mut ok = 0u32;
        let mut used = 0u32;
        for t in traces.iter().take(30) {
            let args: Vec<Value> = t.args.iter()
                .filter_map(|s| serde_json::from_str(s).ok()).collect();
            if args.len() != entry.params.len() { continue; }
            used += 1;
            match Self::call_one(&mut interp, &mut vm, &entry.name, Some(entry.id), args) {
                Ok(v) => {
                    if serde_json::to_string(&v).unwrap_or_default() == t.result { ok += 1; }
                }
                Err(_) => {}
            }
        }
        if used == 0 { 0.0 } else { ok as f64 / used as f64 }
    }

    /// Asymptotic efficiency: incumbent-steps / this-steps on big probes.
    /// Bigger inputs amplify step-count differences, so O(n) vs O(n^2)
    /// becomes visible to selection.
    fn measure_efficiency(entry: &FunctionEntry,
                          functions: &std::collections::HashMap<String, Vec<FunctionEntry>>,
                          variables: &std::collections::HashMap<String, crate::veldt::VariableEntry>,
                          structs: &std::collections::HashMap<String, crate::veldt::StructEntry>,
                          big_probes: &[&crate::veldt::Probe],
                          use_vm: bool) -> f64 {
        let mut interp = Interpreter::with_step_limit(200000);
        interp.caps = crate::interpreter::Caps::sandboxed();
        Self::load_snapshot_into_interp(functions, variables, structs, &mut interp);
        let mut vm = if use_vm { Self::mk_vm(entry, functions, variables, structs) } else { None };
        // add this variant
        interp.functions.entry(entry.name.clone()).or_insert_with(Vec::new)
            .push(crate::interpreter::FunctionVariant {
                id: entry.id, fitness: entry.fitness,
                def: crate::interpreter::FunctionDef {
                    params: Rc::new(entry.params.clone()), body: entry.body.clone().into(),
                },
            });

        // incumbent = fittest OTHER variant in the family
        let incumbent_id = functions.get(&entry.name)
            .and_then(|vs| vs.iter()
                .filter(|v| v.id != entry.id && matches!(v.health, Health::Healthy))
                .max_by(|a, b| a.fitness.partial_cmp(&b.fitness).unwrap_or(std::cmp::Ordering::Equal))
                .map(|v| v.id));

        let mut my_steps = 0u64;
        let mut base_steps = 0u64;
        let mut used = 0;
        for p in big_probes.iter().take(10) {
            let args: Vec<Value> = p.args.iter()
                .filter_map(|s| serde_json::from_str(s).ok()).collect();
            if args.len() != entry.params.len() { continue; }
            let before = interp.step_count;
            let _ = Self::call_one(&mut interp, &mut vm, &entry.name, Some(entry.id), args.clone());
            my_steps += interp.step_count - before;
            used += 1;
            if let Some(id) = incumbent_id {
                let before = interp.step_count;
                let _ = Self::call_one(&mut interp, &mut vm, &entry.name, Some(id), args);
                base_steps += interp.step_count - before;
            }
        }
        if used == 0 || my_steps == 0 { return 1.0; }
        if base_steps == 0 { return 1.0; }
        (base_steps as f64 / my_steps as f64).clamp(0.05, 20.0)
    }

    /// Call a variant through the VM (boxed-Value ABI path) or the
    /// tree-walker — identical dispatch semantics either way.
    fn call_one(interp: &mut Interpreter, vm: &mut Option<crate::vm::Vm>,
                name: &str, id: Option<u32>, args: Vec<Value>)
        -> Result<Value, String> {
        match vm {
            Some(v) => v.jit_call(name, id, args),
            None => interp.call_function(name, id, args),
        }
    }

    /// Build a Vm from the snapshot maps, force-including the variant
    /// under test (load_garden skips non-Healthy entries).
    fn mk_vm(entry: &FunctionEntry,
             functions: &std::collections::HashMap<String, Vec<FunctionEntry>>,
             variables: &std::collections::HashMap<String, crate::veldt::VariableEntry>,
             structs: &std::collections::HashMap<String, crate::veldt::StructEntry>)
        -> Option<crate::vm::Vm> {
        let mut vv = Veldt::new();
        vv.functions = functions.clone();
        vv.variables = variables.clone();
        vv.structs = structs.clone();
        let mut e2 = entry.clone();
        e2.health = Health::Healthy;
        let vs = vv.functions.entry(entry.name.clone()).or_default();
        vs.retain(|x| x.id != e2.id);
        vs.push(e2);
        let mut v = crate::vm::Vm::new();
        v.load_garden(&vv).ok()?;
        Some(v)
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
                        fitness: entry.fitness,
                        def: crate::interpreter::FunctionDef {
                            params: Rc::new(entry.params.clone()),
                            body: entry.body.clone().into(),
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
                interp.vars.borrow_mut().insert(name.clone(), val);
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
                        start: Instant, use_vm: bool) -> TrialResult {
        let mut interp = Interpreter::with_step_limit(10000);
        interp.caps = crate::interpreter::Caps::sandboxed();
        Self::load_snapshot_into_interp(functions, variables, structs, &mut interp);
        let mut vm = if use_vm { Self::mk_vm(entry, functions, variables, structs) } else { None };

        // Load the function being tested
        let variant = crate::interpreter::FunctionVariant {
            id: entry.id,
            fitness: entry.fitness,
            def: crate::interpreter::FunctionDef {
                params: Rc::new(entry.params.clone()),
                body: entry.body.clone().into(),
            },
        };
        interp.functions.entry(entry.name.clone())
            .or_insert_with(Vec::new)
            .push(variant);

        let mut passed = 0;
        let mut total = 0;

        for test in &entry.tests {
            total += 1;
            match &test.expected {
                Some(expected) => {
                    match (interp.eval_expr_in_scope(&test.call),
                           interp.eval_expr_in_scope(expected)) {
                        (Ok(call_val), Ok(expected_val)) => {
                            if Self::values_equal(&call_val, &expected_val) {
                                passed += 1;
                            }
                        }
                        (Err(e), _) => return TrialResult::Fail(format!("Test call failed: {}", e)),
                        (_, Err(e)) => return TrialResult::Fail(format!("Test expected value failed: {}", e)),
                    }
                }
                None => {
                    match interp.eval_expr_in_scope(&test.call) {
                        Ok(v) => {
                            if Self::is_truthy_pub(&v) { passed += 1; }
                        }
                        Err(e) => return TrialResult::Fail(format!("Test predicate failed: {}", e)),
                    }
                }
            }
        }

        let elapsed = start.elapsed().as_secs_f64();
        let correctness = passed as f64 / total as f64;
        let steps = interp.step_count;
        // Efficiency bonus: fewer steps = higher score
        // Formula: 1.0 base + up to 0.3 bonus for efficiency
        let efficiency = 1.0 - (steps as f64 / interp.step_limit as f64).min(0.7);
        let speed_bonus = (1.0 / (1.0 + elapsed * 10.0)).max(0.0);
        let score = correctness + speed_bonus * 0.05 + efficiency * 0.15;

        if passed == total {
            TrialResult::Pass(score, steps)
        } else {
            TrialResult::Fail(format!("{} of {} tests failed", total - passed, total))
        }
    }

    fn run_safe_defaults(entry: &FunctionEntry,
                         functions: &std::collections::HashMap<String, Vec<FunctionEntry>>,
                         variables: &std::collections::HashMap<String, crate::veldt::VariableEntry>,
                         structs: &std::collections::HashMap<String, crate::veldt::StructEntry>,
                         start: Instant, use_vm: bool) -> TrialResult {
        let mut interp = Interpreter::with_step_limit(10000);
        interp.caps = crate::interpreter::Caps::sandboxed();
        Self::load_snapshot_into_interp(functions, variables, structs, &mut interp);
        let mut vm = if use_vm { Self::mk_vm(entry, functions, variables, structs) } else { None };

        let variant = crate::interpreter::FunctionVariant {
            id: entry.id,
            fitness: entry.fitness,
            def: crate::interpreter::FunctionDef {
                params: Rc::new(entry.params.clone()),
                body: entry.body.clone().into(),
            },
        };
        interp.functions.entry(entry.name.clone())
            .or_insert_with(Vec::new)
            .push(variant);

        let args: Vec<Value> = entry.params.iter().map(|p| Self::safe_default(&p.typ)).collect();

        match Self::call_one(&mut interp, &mut vm, &entry.name, Some(entry.id), args) {
            Ok(_) => {
                let elapsed = start.elapsed().as_secs_f64();
                let steps = interp.step_count;
                let speed_bonus = (1.0 / (1.0 + elapsed * 10.0)).max(0.0);
                TrialResult::Pass(0.5 + speed_bonus * 0.1, steps)
            }
            Err(e) => TrialResult::Fail(e),
        }
    }

    fn count_statements(body: &[Stmt]) -> usize {
        let mut count = 0;
        for stmt in body {
            count += 1 + match stmt {
                Stmt::If(_, then_body, else_body) => {
                    Self::count_statements(then_body) +
                    else_body.as_ref().map(|eb| Self::count_statements(eb)).unwrap_or(0)
                }
                Stmt::While(_, body) => Self::count_statements(body),
                Stmt::For(_, _, body) => Self::count_statements(body),
                _ => 0,
            };
        }
        count
    }

    fn eval_test_expr(interp: &mut Interpreter, expr: &Expr) -> Result<Value, String> {
        interp.eval_expr_in_scope(expr)
    }

    fn safe_default(typ: &Type) -> Value {
        match typ {
            Type::Int => Value::Int(0),
            Type::Float => Value::Float(0.0),
            Type::Str => Value::Str("".into()),
            Type::Bool => Value::Bool(true),
            Type::List(_) => Value::List(im::vector![]),
            Type::Fn => Value::Null,
            Type::Struct(_) => Value::Null,
            Type::Any => Value::Null,
        }
    }

    pub fn is_truthy_pub(v: &Value) -> bool {
        match v {
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Float(n) => *n != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::Null => false,
            Value::List(l) => !l.is_empty(),
            Value::Dict(d) => !d.is_empty(),
            Value::Set(s) => !s.is_empty(),
            Value::Struct(_, f) => !f.is_empty(),
            Value::Closure(_, _, _) => true,
        }
    }

    fn values_equal(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Int(a), Value::Float(b)) => (*a as f64) == *b,
            (Value::Float(a), Value::Int(b)) => *a == (*b as f64),
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::List(a), Value::List(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| Self::values_equal(x, y))
            }
            (Value::Dict(a), Value::Dict(b)) => a == b,
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
    Pass(f64, u64),  // (score, step_count)
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
        // Division by zero — genuinely crashes at runtime
        veldt.grow_function("broken", vec![
            Param { name: "a".into(), typ: Type::Int },
        ], vec![Stmt::Return(Some(Expr::BinOp(
            Box::new(Expr::Var("a".into())),
            BinOp::Div,
            Box::new(Expr::Int(0)),
        )))], vec![], "fn broken(a: int) { return a / 0 }");

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
            expected: Some(Expr::Int(6)),
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
            expected: Some(Expr::Int(6)),
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
