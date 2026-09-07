// Evolution engine — the garden grows itself

use crate::ast::*;
use crate::health::HealthChecker;
use crate::veldt::{FunctionEntry, Health, Veldt};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Evolver {
    rng_seed: u64,
}

impl Evolver {
    pub fn new() -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(42);
        Evolver { rng_seed: seed }
    }

    fn rand(&mut self) -> u64 {
        // xorshift64
        self.rng_seed ^= self.rng_seed << 13;
        self.rng_seed ^= self.rng_seed >> 7;
        self.rng_seed ^= self.rng_seed << 17;
        self.rng_seed
    }

    fn rand_range(&mut self, max: usize) -> usize {
        if max == 0 { return 0; }
        (self.rand() as usize) % max
    }

    fn rand_bool(&mut self) -> bool {
        self.rand() % 2 == 0
    }

    /// Run N evolution cycles. Returns a summary string.
    pub fn evolve(&mut self, veldt: &mut Veldt, cycles: usize) -> Vec<EvolutionEvent> {
        let mut events = Vec::new();

        for cycle in 1..=cycles {
            events.push(EvolutionEvent::CycleStart(cycle));

            let budget = 10 + (veldt.run_count * 2);
            let mut spent = 0;

            // Collect candidates: healthy functions with tests, sorted by usage (high usage = important)
            let candidates: Vec<(String, u32)> = veldt.functions.iter()
                .flat_map(|(name, variants)| {
                    variants.iter()
                        .filter(|e| matches!(e.health, Health::Healthy) && !e.tests.is_empty())
                        .map(|e| (name.clone(), e.id))
                        .collect::<Vec<_>>()
                })
                .collect();

            if candidates.is_empty() {
                events.push(EvolutionEvent::NoCandidates);
                events.push(EvolutionEvent::CycleEnd {
                    cycle, mutations: 0, births: 0, deaths: 0,
                });
                continue;
            }

            let mut mutations = 0;
            let mut births = 0;

            while spent < budget {
                // Pick a random candidate
                let (name, id) = &candidates[self.rand_range(candidates.len())];
                let entry = match veldt.functions.get(name).and_then(|v| v.iter().find(|e| e.id == *id)) {
                    Some(e) => e.clone(),
                    None => continue,
                };

                // Apply a random mutation
                let mutation = self.pick_mutation();
                let mutated_body = self.mutate_body(&entry.body, mutation);
                let mutated_tests = entry.tests.clone();
                let mutated_params = entry.params.clone();
                let mutation_desc = self.describe_mutation(mutation, &entry.name, entry.id);

                // Grow the mutant as a new variant
                let source = format!("fn {}(...) {{ [mutated from {}#{}] }}", name, name, entry.id);
                let new_id = veldt.grow_function(name, mutated_params, mutated_body, mutated_tests, &source);
                spent += 1;
                mutations += 1;

                events.push(EvolutionEvent::Mutate {
                    name: name.clone(),
                    parent_id: entry.id,
                    new_id,
                    desc: mutation_desc,
                });

                // Health check at birth
                let mut new_entry = veldt.functions[name].iter()
                    .find(|e| e.id == new_id).cloned().unwrap();
                HealthChecker::check_at_birth(&mut new_entry, veldt);

                // Put it back
                if let Some(variants) = veldt.functions.get_mut(name) {
                    for e in variants.iter_mut() {
                        if e.id == new_id { *e = new_entry.clone(); break; }
                    }
                }

                let healthy = matches!(new_entry.health, Health::Healthy);
                events.push(EvolutionEvent::Trial {
                    name: name.clone(),
                    id: new_id,
                    healthy,
                    score: new_entry.trial_score,
                });

                if healthy {
                    births += 1;
                }

                // Stop if we've made enough mutants this cycle
                if mutations >= 5 { break; }
            }

            // Age and reap — competition happens here
            let obituaries = veldt.age_and_reap();
            for obit in &obituaries {
                events.push(EvolutionEvent::Death {
                    name: obit.name.clone(),
                    id: obit.id,
                    cause: obit.cause_of_death.clone(),
                });
            }

            events.push(EvolutionEvent::CycleEnd {
                cycle,
                mutations,
                births,
                deaths: obituaries.len(),
            });
        }

        events
    }

    fn pick_mutation(&mut self) -> Mutation {
        match self.rand_range(4) {
            0 => Mutation::Constant,
            1 => Mutation::Operator,
            2 => Mutation::StatementSwap,
            _ => Mutation::DeadCodeRemoval,
        }
    }

    fn describe_mutation(&self, m: Mutation, name: &str, id: u32) -> String {
        match m {
            Mutation::Constant => format!("constant mutation on {}#{}", name, id),
            Mutation::Operator => format!("operator mutation on {}#{}", name, id),
            Mutation::StatementSwap => format!("statement swap on {}#{}", name, id),
            Mutation::DeadCodeRemoval => format!("dead code removal on {}#{}", name, id),
        }
    }

    fn mutate_body(&mut self, body: &[Stmt], mutation: Mutation) -> Vec<Stmt> {
        let mut new_body = body.to_vec();
        if new_body.is_empty() { return new_body; }

        match mutation {
            Mutation::Constant => self.mutate_constants(&mut new_body),
            Mutation::Operator => self.mutate_operators(&mut new_body),
            Mutation::StatementSwap => {
                if new_body.len() >= 2 {
                    let i = self.rand_range(new_body.len());
                    let j = self.rand_range(new_body.len());
                    if i != j {
                        new_body.swap(i, j);
                    }
                }
            }
            Mutation::DeadCodeRemoval => {
                // Remove a random non-return statement
                let removable: Vec<usize> = new_body.iter().enumerate()
                    .filter(|(_, s)| !matches!(s, Stmt::Return(_)))
                    .map(|(i, _)| i)
                    .collect();
                if !removable.is_empty() {
                    let idx = removable[self.rand_range(removable.len())];
                    new_body.remove(idx);
                }
            }
        }

        new_body
    }

    fn mutate_constants(&mut self, body: &mut Vec<Stmt>) {
        // Find all int constants in the body, mutate one
        let mut found = false;
        for stmt in body.iter_mut() {
            if found { break; }
            found = self.mutate_constants_stmt(stmt);
        }
    }

    fn mutate_constants_stmt(&mut self, stmt: &mut Stmt) -> bool {
        match stmt {
            Stmt::Let(_, e) | Stmt::Assign(_, e) | Stmt::Return(Some(e)) | Stmt::Print(e) | Stmt::ExprStmt(e) => {
                self.mutate_constants_expr(e)
            }
            Stmt::If(cond, then_body, else_body) => {
                if self.mutate_constants_expr(cond) { return true; }
                for s in then_body { if self.mutate_constants_stmt(s) { return true; } }
                if let Some(eb) = else_body {
                    for s in eb { if self.mutate_constants_stmt(s) { return true; } }
                }
                false
            }
            Stmt::While(cond, body) => {
                if self.mutate_constants_expr(cond) { return true; }
                for s in body { if self.mutate_constants_stmt(s) { return true; } }
                false
            }
            Stmt::For(_, iter, body) => {
                if self.mutate_constants_expr(iter) { return true; }
                for s in body { if self.mutate_constants_stmt(s) { return true; } }
                false
            }
            _ => false,
        }
    }

    fn mutate_constants_expr(&mut self, expr: &mut Expr) -> bool {
        match expr {
            Expr::Int(n) => {
                // Mutate: +1, -1, *2, or change to a nearby value
                let choice = self.rand_range(4);
                *n = match choice {
                    0 => *n + 1,
                    1 => *n - 1,
                    2 => *n * 2,
                    _ => *n + (self.rand() as i64 % 5) - 2,
                };
                true
            }
            Expr::BinOp(l, _, r) => {
                if self.mutate_constants_expr(l) { true }
                else { self.mutate_constants_expr(r) }
            }
            Expr::UnaryOp(_, e) => self.mutate_constants_expr(e),
            Expr::MethodCall(obj, _, args) => {
                if self.mutate_constants_expr(obj) { return true; }
                for a in args { if self.mutate_constants_expr(a) { return true; } }
                false
            }
            Expr::Call(_, _, args) => {
                for a in args { if self.mutate_constants_expr(a) { return true; } }
                false
            }
            Expr::List(items) => {
                for i in items { if self.mutate_constants_expr(i) { return true; } }
                false
            }
            _ => false,
        }
    }

    fn mutate_operators(&mut self, body: &mut Vec<Stmt>) {
        for stmt in body.iter_mut() {
            self.mutate_operators_stmt(stmt);
        }
    }

    fn mutate_operators_stmt(&mut self, stmt: &mut Stmt) {
        match stmt {
            Stmt::Let(_, e) | Stmt::Assign(_, e) | Stmt::Return(Some(e)) | Stmt::Print(e) | Stmt::ExprStmt(e) => {
                self.mutate_operators_expr(e);
            }
            Stmt::If(cond, then_body, else_body) => {
                self.mutate_operators_expr(cond);
                for s in then_body { self.mutate_operators_stmt(s); }
                if let Some(eb) = else_body { for s in eb { self.mutate_operators_stmt(s); } }
            }
            Stmt::While(cond, body) => {
                self.mutate_operators_expr(cond);
                for s in body { self.mutate_operators_stmt(s); }
            }
            Stmt::For(_, iter, body) => {
                self.mutate_operators_expr(iter);
                for s in body { self.mutate_operators_stmt(s); }
            }
            _ => {}
        }
    }

    fn mutate_operators_expr(&mut self, expr: &mut Expr) {
        match expr {
            Expr::BinOp(l, op, r) => {
                // Maybe swap this operator
                if self.rand_bool() {
                    *op = self.mutate_binop(op);
                }
                self.mutate_operators_expr(l);
                self.mutate_operators_expr(r);
            }
            Expr::UnaryOp(_, e) => self.mutate_operators_expr(e),
            Expr::MethodCall(obj, _, args) => {
                self.mutate_operators_expr(obj);
                for a in args { self.mutate_operators_expr(a); }
            }
            Expr::Call(_, _, args) => {
                for a in args { self.mutate_operators_expr(a); }
            }
            _ => {}
        }
    }

    fn mutate_binop(&mut self, op: &BinOp) -> BinOp {
        match op {
            BinOp::Add => BinOp::Sub,
            BinOp::Sub => BinOp::Add,
            BinOp::Mul => BinOp::Div,
            BinOp::Div => BinOp::Mul,
            BinOp::Mod => BinOp::Mul,
            BinOp::Lt => BinOp::Le,
            BinOp::Le => BinOp::Lt,
            BinOp::Gt => BinOp::Ge,
            BinOp::Ge => BinOp::Gt,
            BinOp::Eq => BinOp::Neq,
            BinOp::Neq => BinOp::Eq,
            other => other.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum EvolutionEvent {
    CycleStart(usize),
    NoCandidates,
    Mutate { name: String, parent_id: u32, new_id: u32, desc: String },
    Trial { name: String, id: u32, healthy: bool, score: f64 },
    Death { name: String, id: u32, cause: String },
    CycleEnd { cycle: usize, mutations: usize, births: usize, deaths: usize },
}

impl EvolutionEvent {
    pub fn format(&self) -> String {
        match self {
            Self::CycleStart(n) => format!("[EVOLVE] Cycle {}/", n),
            Self::NoCandidates => "  [SKIP] No healthy functions with tests to evolve".into(),
            Self::Mutate { name, parent_id, new_id, desc } => {
                format!("  [MUTATE] {}#{} → {}#{} ({})", name, parent_id, name, new_id, desc)
            }
            Self::Trial { name, id, healthy, score } => {
                if *healthy {
                    format!("  [TRIAL]  {}#{} — healthy (score: {:.2})", name, id, score)
                } else {
                    format!("  [TRIAL]  {}#{} — sick (failed trial)", name, id)
                }
            }
            Self::Death { name, id, cause } => {
                format!("  [CULL]   {}#{} died — {}", name, id, cause)
            }
            Self::CycleEnd { cycle, mutations, births, deaths } => {
                format!("[EVOLVE] Cycle {} complete: {} mutations, {} births, {} deaths",
                    cycle, mutations, births, deaths)
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Mutation {
    Constant,
    Operator,
    StatementSwap,
    DeadCodeRemoval,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_veldt() -> Veldt {
        let mut v = Veldt::with_path(PathBuf::from("/tmp/veldt_evolve_test"));
        v.clear();
        v
    }

    #[test]
    fn test_evolution_creates_mutants() {
        let mut veldt = make_veldt();
        // Grow a simple function with tests
        veldt.grow_function("add", vec![
            Param { name: "a".into(), typ: Type::Int },
            Param { name: "b".into(), typ: Type::Int },
        ], vec![Stmt::Return(Some(Expr::BinOp(
            Box::new(Expr::Var("a".into())),
            BinOp::Add,
            Box::new(Expr::Var("b".into())),
        )))], vec![TestAssert {
            call: Expr::Call("add".into(), None, vec![Expr::Int(2), Expr::Int(3)]),
            expected: Expr::Int(5),
        }], "fn add(a, b) { return a + b }");

        // Health check
        let mut entry = veldt.functions["add"][0].clone();
        HealthChecker::check_at_birth(&mut entry, &veldt);
        veldt.functions.get_mut("add").unwrap()[0] = entry;

        let initial_count = veldt.functions["add"].len();

        let mut evolver = Evolver::new();
        let events = evolver.evolve(&mut veldt, 1);

        // Should have created at least one mutant
        assert!(veldt.functions["add"].len() > initial_count);
        assert!(!events.is_empty());
    }

    #[test]
    fn test_constant_mutation() {
        // Try multiple times since mutation is probabilistic
        let mut changed = false;
        for _ in 0..20 {
            let mut evolver = Evolver::new();
            let body = vec![Stmt::Return(Some(Expr::Int(42)))];
            let mutated = evolver.mutate_body(&body, Mutation::Constant);
            if let Stmt::Return(Some(Expr::Int(n))) = &mutated[0] {
                if *n != 42 { changed = true; break; }
            } else {
                panic!("Expected Return(Int)");
            }
        }
        assert!(changed, "Constant mutation should eventually change 42");
    }

    #[test]
    fn test_operator_mutation() {
        // Operator mutation is probabilistic (50% per operator), so we test
        // that structure is preserved and try enough times to see a change.
        let mut changed = false;
        for _ in 0..20 {
            let mut evolver = Evolver::new();
            let body = vec![Stmt::Return(Some(Expr::BinOp(
                Box::new(Expr::Var("a".into())),
                BinOp::Add,
                Box::new(Expr::Var("b".into())),
            )))];
            let mutated = evolver.mutate_body(&body, Mutation::Operator);
            if let Stmt::Return(Some(Expr::BinOp(_, op, _))) = &mutated[0] {
                if *op != BinOp::Add { changed = true; break; }
            } else {
                panic!("Expected Return(BinOp)");
            }
        }
        assert!(changed, "Operator mutation should eventually change Add to Sub");
    }

    #[test]
    fn test_statement_swap() {
        let mut evolver = Evolver::new();
        let body = vec![
            Stmt::Let("x".into(), Expr::Int(1)),
            Stmt::Let("y".into(), Expr::Int(2)),
            Stmt::Return(Some(Expr::Var("x".into()))),
        ];
        let mutated = evolver.mutate_body(&body, Mutation::StatementSwap);
        // Should still have 3 statements
        assert_eq!(mutated.len(), 3);
    }

    #[test]
    fn test_dead_code_removal() {
        let mut evolver = Evolver::new();
        let body = vec![
            Stmt::Let("x".into(), Expr::Int(1)),
            Stmt::Let("y".into(), Expr::Int(2)),
            Stmt::Return(Some(Expr::Var("x".into()))),
        ];
        let mutated = evolver.mutate_body(&body, Mutation::DeadCodeRemoval);
        // Should have removed one non-return statement
        assert_eq!(mutated.len(), 2);
        // Return should still be there
        assert!(mutated.iter().any(|s| matches!(s, Stmt::Return(_))));
    }

    #[test]
    fn test_no_candidates() {
        let mut veldt = make_veldt();
        // No functions at all
        let mut evolver = Evolver::new();
        let events = evolver.evolve(&mut veldt, 1);
        assert!(events.iter().any(|e| matches!(e, EvolutionEvent::NoCandidates)));
    }
}
