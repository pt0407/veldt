// Evolution engine — the garden grows itself

use crate::ast::*;
use crate::health::HealthChecker;
use crate::veldt::{FunctionEntry, Health, Veldt};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Evolver {
    rng_seed: u64,
    /// BYOK LLM mutations — requires VELDT_LLM_KEY (see llm.rs).
    /// Falls back to mechanical mutation when unconfigured or when
    /// the model returns unparseable code.
    pub llm: bool,
}

impl Evolver {
    pub fn new() -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(42);
        Evolver { rng_seed: seed, llm: false }
    }

    /// Deterministic evolution: same seed + same garden = same outcomes.
    /// Lets benchmarks and demos be reproducible.
    pub fn set_seed(&mut self, seed: u64) {
        self.rng_seed = if seed == 0 { 0x9E3779B97F4A7C15 } else { seed };
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

            // === Phase 1: probe generation (co-evolving test pool) ===
            // For each candidate family, generate fresh typed inputs; the
            // fittest variant's answers become consensus ground truth. The
            // pool accumulates — selection pressure ratchets up.
            let mut probe_families: std::collections::HashSet<String> = std::collections::HashSet::new();
            let family_names: Vec<String> = candidates.iter().map(|(n, _)| n.clone())
                .collect::<std::collections::HashSet<_>>().into_iter().collect();
            for name in &family_names {
                if let Some(new_probes) = self.generate_probes(veldt, name, 4) {
                    if !new_probes.is_empty() {
                        let pool = veldt.probes.entry(name.clone()).or_insert_with(Vec::new);
                        for p in &new_probes {
                            events.push(EvolutionEvent::ProbeGenerated {
                                name: name.clone(),
                                niche: p.niche.clone(),
                            });
                            pool.push(p.clone());
                        }
                        // cap pool size — keep the most recent 40 probes
                        if pool.len() > 40 {
                            let excess = pool.len() - 40;
                            pool.drain(0..excess);
                        }
                        probe_families.insert(name.clone());
                    }
                }
            }

            // Re-trial healthy variants in families that got new probes so
            // their niche scores reflect the enlarged pool
            for name in &probe_families {
                let ids: Vec<u32> = veldt.functions.get(name).map(|vs|
                    vs.iter().filter(|e| matches!(e.health, Health::Healthy)).map(|e| e.id).collect()
                ).unwrap_or_default();
                for id in ids {
                    let mut e = veldt.functions[name].iter().find(|e| e.id == id).cloned().unwrap();
                    HealthChecker::check_at_birth(&mut e, veldt);
                    if let Some(variants) = veldt.functions.get_mut(name) {
                        for v in variants.iter_mut() {
                            if v.id == id { *v = e.clone(); break; }
                        }
                    }
                }
            }

            // === Phase 2: reproduction — mutation + crossover ===
            let mut mutations = 0;
            let mut births = 0;

            while spent < budget {
                // Pick a family, then a parent by lexicase selection: filter
                // candidates through a shuffled niche sequence so specialists
                // (best in some niche) get reproduced, not just the fittest.
                let families: Vec<String> = candidates.iter().map(|(n, _)| n.clone())
                    .collect::<std::collections::HashSet<_>>().into_iter().collect();
                let name = families[self.rand_range(families.len())].clone();
                let id = match self.lexicase_pick(veldt, &name) {
                    Some(i) => i,
                    None => continue,
                };
                let entry = match veldt.functions.get(&name).and_then(|v| v.iter().find(|e| e.id == id)) {
                    Some(e) => e.clone(),
                    None => continue,
                };
                let name = &name;

                let healthy_ids: Vec<u32> = veldt.functions.get(name).map(|vs|
                    vs.iter().filter(|e| matches!(e.health, Health::Healthy)).map(|e| e.id).collect()
                ).unwrap_or_default();

                let do_crossover = healthy_ids.len() >= 2 && self.rand_range(10) < 3;

                let (child_body, parents, origin) = if do_crossover {
                    let other_id = healthy_ids.iter().filter(|i| **i != entry.id)
                        .nth(self.rand_range(healthy_ids.len() - 1)).copied().unwrap();
                    let other = veldt.functions[name].iter().find(|e| e.id == other_id).cloned().unwrap();
                    let body = self.crossover_bodies(&entry.body, &other.body);
                    (body, vec![entry.id, other_id],
                     format!("crossover {}#{} x {}#{}", name, entry.id, name, other_id))
                } else if self.llm && crate::llm::configured() {
                    match self.llm_mutate(name, &entry) {
                        Some(body) => (body, vec![entry.id],
                            format!("llm-rewrite of {}#{}", name, entry.id)),
                        None => {
                            let (body, mutation) = self.mutate_retry(&entry.body);
                            (body, vec![entry.id],
                             format!("{} of {}#{}", mutation_name(mutation), name, entry.id))
                        }
                    }
                } else {
                    let (body, mutation) = self.mutate_retry(&entry.body);
                    (body, vec![entry.id],
                     format!("{} of {}#{}", mutation_name(mutation), name, entry.id))
                };

                // Semantic dedup: reject children identical to an existing variant
                if let Some(variants) = veldt.functions.get(name) {
                    let child_key = format!("{:?}", child_body);
                    if variants.iter().any(|e| format!("{:?}", e.body) == child_key) {
                        events.push(EvolutionEvent::CloneRejected { name: name.clone() });
                        spent += 1;
                        continue;
                    }
                }

                let source = format!("fn {}(...) {{ [{}] }}", name, origin);
                let new_id = veldt.grow_function_full(name, entry.params.clone(), child_body,
                    entry.tests.clone(), &source, parents.clone(), &origin);
                spent += 1;
                mutations += 1;

                events.push(EvolutionEvent::Mutate {
                    name: name.clone(),
                    parent_id: entry.id,
                    new_id,
                    desc: origin.clone(),
                });

                // Health check at birth (runs inline tests + probes)
                let mut new_entry = veldt.functions[name].iter()
                    .find(|e| e.id == new_id).cloned().unwrap();
                HealthChecker::check_at_birth(&mut new_entry, veldt);

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
                    fitness: new_entry.fitness,
                });

                if healthy {
                    births += 1;
                }

                if mutations >= 5 { break; }
            }

            // Age and reap — competition happens here (with niche protection)
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

    /// Lexicase selection: pick a parent by filtering healthy variants
    /// through a randomly-ordered niche sequence, keeping only those
    /// within epsilon of the best score at each niche. Specialists that
    /// dominate a niche get reproduced even if globally mid-fitness.
    fn lexicase_pick(&mut self, veldt: &Veldt, name: &str) -> Option<u32> {
        let variants = veldt.functions.get(name)?;
        let healthy: Vec<&FunctionEntry> = variants.iter()
            .filter(|e| matches!(e.health, Health::Healthy) && !e.tests.is_empty())
            .collect();
        if healthy.is_empty() { return None; }

        // all known niches for this family (probes + recorded scores)
        let mut niches: Vec<String> = veldt.probes.get(name)
            .map(|ps| ps.iter().map(|p| p.niche.clone()).collect()).unwrap_or_default();
        for v in &healthy {
            for k in v.niche_scores.keys() { niches.push(k.clone()); }
        }
        niches.sort();
        niches.dedup();
        if niches.is_empty() {
            return Some(healthy[self.rand_range(healthy.len())].id);
        }
        // shuffle niche order — each generation selects for different expertise
        for i in (1..niches.len()).rev() {
            let j = self.rand_range(i + 1);
            niches.swap(i, j);
        }
        let mut pool: Vec<&FunctionEntry> = healthy;
        for niche in &niches {
            if pool.len() <= 1 { break; }
            let best = pool.iter()
                .map(|v| v.niche_scores.get(niche).copied().unwrap_or(0.0))
                .fold(0.0f64, f64::max);
            pool.retain(|v| {
                let s = v.niche_scores.get(niche).copied().unwrap_or(0.0);
                (best - s).abs() < 0.001
            });
        }
        if pool.is_empty() { return None; }
        Some(pool[self.rand_range(pool.len())].id)
    }

    /// Generate typed random inputs for a family and record the fittest
    /// variant's answers as consensus ground truth.
    fn generate_probes(&mut self, veldt: &Veldt, name: &str, count: usize) -> Option<Vec<crate::veldt::Probe>> {
        let fittest = veldt.get_fittest_variant(name)?.clone();
        if !matches!(fittest.health, Health::Healthy) { return None; }

        // Build an interpreter with the veldt snapshot
        let mut interp = crate::interpreter::Interpreter::with_step_limit(10000);
        veldt.load_into_interpreter(&mut interp);

        let mut probes = Vec::new();
        for i in 0..count {
            // ~1/3 boundary corpus (fuzz edge cases), rest random + scale
            let boundary = i % 3 == 2;
            let scale = if i % 2 == 1 { 1 } else { 0 };
            let mut args = Vec::new();
            let mut ok = true;
            for p in &fittest.params {
                let v = if boundary {
                    boundary_value(&p.typ, self.rand_range(8))
                } else {
                    self.gen_value_scaled(&p.typ, scale)
                };
                match v {
                    Some(v) => args.push(v),
                    None => { ok = false; break; }
                }
            }
            if !ok { continue; }
            // run fittest variant -> consensus answer
            match interp.call_function(name, Some(fittest.id), args.clone()) {
                Ok(expected) => {
                    let arg_strs: Vec<String> = args.iter()
                        .map(|v| serde_json::to_string(v).unwrap_or_default()).collect();
                    probes.push(crate::veldt::Probe {
                        args: arg_strs,
                        expected: serde_json::to_string(&expected).unwrap_or_default(),
                        niche: Self::niche_signature(&args),
                        born: now_ts(),
                        scale,
                    });
                }
                Err(_) => {} // fittest can't answer this input either — skip
            }
        }
        Some(probes)
    }

    /// Coarse input signature — defines niches. Variants that specialise on
    /// particular input shapes can survive as niche champions.
    fn niche_signature(args: &[crate::interpreter::Value]) -> String {
        use crate::interpreter::Value;
        args.iter().map(|v| match v {
            Value::Int(n) => {
                if *n < 0 { "int:neg".to_string() }
                else if *n == 0 { "int:zero".to_string() }
                else if *n < 10 { "int:small".to_string() }
                else { "int:big".to_string() }
            }
            Value::Float(f) => {
                if f.abs() < 1.0 { "float:tiny".to_string() } else { "float:big".to_string() }
            }
            Value::Str(s) => {
                if s.is_empty() { "str:empty".to_string() }
                else if s.len() < 4 { "str:short".to_string() }
                else { "str:long".to_string() }
            }
            Value::Bool(b) => format!("bool:{}", b),
            Value::List(l) => {
                if l.is_empty() { "list:empty".to_string() }
                else if l.len() <= 3 { "list:small".to_string() }
                else { "list:big".to_string() }
            }
            Value::Dict(d) => format!("dict:{}", if d.len() <= 3 { "small" } else { "big" }),
            Value::Set(s) => format!("set:{}", if s.len() <= 3 { "small" } else { "big" }),
            Value::Struct(n, _) => format!("struct:{}", n),
            _ => "other".to_string(),
        }).collect::<Vec<_>>().join("|")
    }

    /// Typed random value generation for probes.
    fn gen_value(&mut self, typ: &Type) -> Option<crate::interpreter::Value> {
        self.gen_value_scaled(typ, 0)
    }

    /// scale 0 = small inputs; scale 1 = big inputs that amplify
    /// step-count differences so algorithmic complexity shows up.
    fn gen_value_scaled(&mut self, typ: &Type, scale: u32) -> Option<crate::interpreter::Value> {
        use crate::interpreter::Value;
        match typ {
            Type::Int => {
                let choices = [-3i64, -1, 0, 1, 2, 3, 5, 8, 13, 42, 100];
                Some(Value::Int(choices[self.rand_range(choices.len())]))
            }
            Type::Float => {
                let f = (self.rand() % 2000) as f64 / 100.0 - 10.0;
                Some(Value::Float(f))
            }
            Type::Str => {
                let words = ["", "a", "hi", "veldt", "hello world", "x"];
                Some(Value::Str(words[self.rand_range(words.len())].into()))
            }
            Type::Bool => Some(Value::Bool(self.rand_bool())),
            Type::List(inner) => {
                let n = if scale > 0 { 15 + self.rand_range(20) } else { self.rand_range(6) };
                let mut items = Vec::new();
                for _ in 0..n {
                    items.push(self.gen_value_scaled(inner, 0)?);
                }
                Some(Value::List(items.into_iter().collect()))
            }
            Type::Any => {
                // pick a random concrete type
                let t = match self.rand_range(4) {
                    0 => Type::Int,
                    1 => Type::Str,
                    2 => Type::Bool,
                    _ => Type::Int,
                };
                self.gen_value(&t)
            }
            _ => None, // Fn, Struct — can't generate
        }
    }

    /// Crossover: splice prefix of A with suffix of B at statement level.
    fn crossover_bodies(&mut self, a: &[Stmt], b: &[Stmt]) -> Vec<Stmt> {
        if a.is_empty() { return b.to_vec(); }
        if b.is_empty() { return a.to_vec(); }
        let i = self.rand_range(a.len() + 1);
        let j = self.rand_range(b.len() + 1);
        let mut child: Vec<Stmt> = a[..i].to_vec();
        child.extend_from_slice(&b[j..]);
        if child.is_empty() {
            child = a.to_vec();
        }
        child
    }

    /// Ask the LLM for a variant of this function. Returns the parsed
    /// body if the model produced parseable Veldt with the same arity.
    fn llm_mutate(&self, name: &str, entry: &FunctionEntry) -> Option<Vec<Stmt>> {
        let body_src = crate::fmt::format_program(&entry.body);
        let params_src = entry.params.iter()
            .map(|p| format!("{}: {}", p.name, type_str(&p.typ)))
            .collect::<Vec<_>>().join(", ");
        let src = format!("fn {}({}) {{\n{}\n}}", name, params_src, body_src);
        let system = "You mutate functions written in Veldt. Veldt syntax: let x = 5, \
            if/else/while/for-in, return, list literals [1,2,3], .push()/.contains()/\
            .sort() methods, str+int coercion, and/or/not booleans, % modulo, \
            dict()/put/get/has, no semicolons, # comments. Types: int float str bool list<int>.";
        let user = format!(
            "Rewrite this Veldt function with a meaningfully different approach — keep the exact signature, change the body. Return ONLY the fn definition, no fences, no explanation.\n\n{}", src);
        let text = crate::llm::complete(system, &user)?;
        // strip markdown fences if the model added them
        let code = text.trim()
            .trim_start_matches("```").trim_end_matches("```")
            .trim_start_matches("veldt").trim().to_string();
        let mut lx = crate::lexer::Lexer::new(&code);
        let tokens = lx.tokenize().ok()?;
        let lines = lx.token_lines.clone();
        let mut p = crate::parser::Parser::new_with_lines(tokens, lines);
        let stmts = p.parse_program().ok()?;
        for s in &stmts {
            if let Stmt::FnDef(_, params, body) = s {
                if params.len() == entry.params.len() && !body.is_empty() {
                    return Some(body.clone());
                }
            }
        }
        None
    }

    fn pick_mutation(&mut self) -> Mutation {
        match self.rand_range(10) {
            0 => Mutation::Constant,
            1 => Mutation::Operator,
            2 => Mutation::StatementSwap,
            3 => Mutation::DeadCodeRemoval,
            4 => Mutation::ConstFold,
            5 => Mutation::DeadBranch,
            6 => Mutation::ConstProp,
            7 => Mutation::HoistInvariant,
            8 => Mutation::EarlyExit,
            _ => Mutation::Simplify,
        }
    }

    fn describe_mutation(&self, m: Mutation, name: &str, id: u32) -> String {
        match m {
            Mutation::Constant => format!("constant mutation on {}#{}", name, id),
            Mutation::Operator => format!("operator mutation on {}#{}", name, id),
            Mutation::StatementSwap => format!("statement swap on {}#{}", name, id),
            Mutation::DeadCodeRemoval => format!("dead code removal on {}#{}", name, id),
            Mutation::Simplify => format!("simplification on {}#{}", name, id),
            Mutation::ConstFold => format!("constant folding on {}#{}", name, id),
            Mutation::DeadBranch => format!("dead branch elimination on {}#{}", name, id),
            Mutation::ConstProp => format!("constant propagation on {}#{}", name, id),
            Mutation::HoistInvariant => format!("loop-invariant hoist on {}#{}", name, id),
            Mutation::EarlyExit => format!("early-exit fusion on {}#{}", name, id),
        }
    }

    /// Pick + apply a mutation; re-roll (≤6x) when it leaves the body
    /// unchanged — conditional operators can't fire on every shape, and
    /// no-op children only burn budget downstream (clone rejection).
    fn mutate_retry(&mut self, body: &[Stmt]) -> (Vec<Stmt>, Mutation) {
        let parent_key = format!("{:?}", body);
        let mut last = Mutation::Simplify;
        let mut out = body.to_vec();
        for _ in 0..6 {
            last = self.pick_mutation();
            out = self.mutate_body(body, last);
            if format!("{:?}", out) != parent_key { break; }
        }
        (out, last)
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
            Mutation::Simplify => {
                // Algebraic simplification: x+0→x, x*1→x, x-0→x, x*0→0, x/1→x.
                // Produces provably-equivalent, smaller code — a genuine
                // improvement signal the garden can actually find.
                for stmt in new_body.iter_mut() {
                    if self.simplify_stmt(stmt) { break; }
                }
            }
            Mutation::ConstProp => {
                // let a = <const>; return a  ->  return <const>
                // Cross-statement constant propagation.
                let mut target = None;
                for i in 0..new_body.len().saturating_sub(1) {
                    if let Stmt::Let(name, val) = &new_body[i] {
                        if is_const_expr(val) {
                            if let Stmt::Return(Some(Expr::Var(r))) = &new_body[i + 1] {
                                if r == name { target = Some(i); break; }
                            }
                        }
                    }
                }
                if let Some(i) = target {
                    if let Stmt::Let(_, val) = new_body.remove(i) {
                        new_body[i] = Stmt::Return(Some(val));
                    }
                }
            }
            Mutation::EarlyExit => {
                // while c { if c2 { return x } ; rest } ->
                // while c and !c2 { rest } ; if c2 { return x }
                let mut target = None;
                for i in 0..new_body.len() {
                    if let Stmt::While(_, body) = &new_body[i] {
                        if let Some(Stmt::If(_, then_b, None)) = body.first() {
                            if matches!(&then_b[..], [Stmt::Return(_)]) {
                                target = Some(i); break;
                            }
                        }
                    }
                }
                if let Some(i) = target {
                    if let Stmt::While(c, mut body) = new_body[i].clone() {
                        if let Some(Stmt::If(c2, mut then_b, None)) = body.first().cloned() {
                            body.remove(0);
                            let ret = then_b.remove(0);
                            let guard = Expr::BinOp(
                                Box::new(c), BinOp::And,
                                Box::new(Expr::UnaryOp(UnaryOp::Not, Box::new(c2.clone()))));
                            new_body[i] = Stmt::While(guard, body);
                            new_body.insert(i + 1, Stmt::If(c2, vec![ret], None));
                        }
                    }
                }
            }
            Mutation::ConstFold => {
                // Fold literal arithmetic: 2*3 -> 6. Real work reduction —
                // shrinks per-call step count, not just size.
                for stmt in new_body.iter_mut() {
                    if Self::fold_stmt_exprs(stmt) { break; }
                }
            }
            Mutation::DeadBranch => {
                // if <const bool>: splice the taken arm in place of the if.
                // while <const false>: the loop never runs — remove it.
                let mut out: Vec<Stmt> = Vec::new();
                for stmt in new_body.into_iter() {
                    let dead = match &stmt {
                        Stmt::If(c, _, _) => Self::const_eval(c)
                            .and_then(|e| if let Expr::Bool(b) = e { Some(b) } else { None }),
                        Stmt::While(c, _) => Self::const_eval(c)
                            .and_then(|e| if let Expr::Bool(b) = e { Some(!b) } else { None }),
                        _ => None,
                    };
                    match (stmt, dead) {
                        (Stmt::If(_, then, _), Some(true)) => out.extend(then),
                        (Stmt::If(_, _, els), Some(false)) =>
                            out.extend(els.unwrap_or_default()),
                        (Stmt::While(_, _), Some(true)) => {} // dead loop dropped
                        (s, _) => out.push(s),
                    }
                }
                new_body = out;
            }
            Mutation::HoistInvariant => {
                // Lift `let x = <loop-invariant expr>` out of a while loop —
                // work done once instead of per-iteration. The classic
                // mechanical route to fewer steps at scale.
                for i in 0..new_body.len() {
                    let hoisted = if let Stmt::While(_, body) = &new_body[i] {
                        // vars mutated anywhere in the loop body
                        let mut assigned = std::collections::HashSet::new();
                        Self::collect_assigned(body, &mut assigned);
                        // first let whose rhs reads no loop-mutated var
                        body.iter().position(|st| {
                            if let Stmt::Let(name, e) = st {
                                // RHS must read no loop-mutated var, and
                                // the name must not be re-assigned inside
                                // (else hoisting changes reset semantics)
                                !Self::expr_uses_any(e, &assigned)
                                    && !body.iter().any(|s2| matches!(s2,
                                        Stmt::Assign(n2, _) if n2 == name))
                            } else { false }
                        }).map(|pos| pos)
                    } else { None };
                    if let Some(pos) = hoisted {
                        if let Stmt::While(_, body) = &mut new_body[i] {
                            let stmt = body.remove(pos);
                            new_body.insert(i, stmt);
                        }
                        break;
                    }
                }
            }
        }

        new_body
    }

    /// Evaluate a constant expression to a literal Expr (Int/Float/Bool/Str).
    /// Returns None if the expr isn't fully constant.
    fn const_eval(e: &Expr) -> Option<Expr> {
        match e {
            Expr::Int(_) | Expr::Float(_) | Expr::Bool(_)
            | Expr::Str(_) | Expr::Null => Some(e.clone()),
            Expr::UnaryOp(crate::ast::UnaryOp::Not, inner) => {
                match Self::const_eval(inner)? {
                    Expr::Bool(b) => Some(Expr::Bool(!b)),
                    _ => None,
                }
            }
            Expr::UnaryOp(crate::ast::UnaryOp::Neg, inner) => {
                match Self::const_eval(inner)? {
                    Expr::Int(n) => Some(Expr::Int(-n)),
                    Expr::Float(f) => Some(Expr::Float(-f)),
                    _ => None,
                }
            }
            Expr::BinOp(l, op, r) => {
                let lv = Self::const_eval(l)?;
                let rv = Self::const_eval(r)?;
                use Expr::{Int as I, Float as F, Bool as B, Str as S};
                match (op, &lv, &rv) {
                    (BinOp::Add, I(a), I(b)) => Some(I(a + b)),
                    (BinOp::Sub, I(a), I(b)) => Some(I(a - b)),
                    (BinOp::Mul, I(a), I(b)) => Some(I(a * b)),
                    (BinOp::Div, I(a), I(b)) if *b != 0 => Some(I(a / b)),
                    (BinOp::Mod, I(a), I(b)) if *b != 0 => Some(I(a % b)),
                    (BinOp::Add, F(a), F(b)) => Some(F(a + b)),
                    (BinOp::Sub, F(a), F(b)) => Some(F(a - b)),
                    (BinOp::Mul, F(a), F(b)) => Some(F(a * b)),
                    (BinOp::Div, F(a), F(b)) if *b != 0.0 => Some(F(a / b)),
                    (BinOp::Add, S(a), S(b)) => Some(S(format!("{}{}", a, b))),
                    (BinOp::Lt, I(a), I(b)) => Some(B(a < b)),
                    (BinOp::Gt, I(a), I(b)) => Some(B(a > b)),
                    (BinOp::Le, I(a), I(b)) => Some(B(a <= b)),
                    (BinOp::Ge, I(a), I(b)) => Some(B(a >= b)),
                    (BinOp::Eq, I(a), I(b)) => Some(B(a == b)),
                    (BinOp::Neq, I(a), I(b)) => Some(B(a != b)),
                    (BinOp::And, B(a), B(b)) => Some(B(*a && *b)),
                    (BinOp::Or, B(a), B(b)) => Some(B(*a || *b)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Replace the first fully-constant BinOp in a statement with its value.
    fn fold_stmt_exprs(stmt: &mut Stmt) -> bool {
        match stmt {
            Stmt::Let(_, e) | Stmt::Assign(_, e) | Stmt::Print(e)
            | Stmt::ExprStmt(e) | Stmt::LetDestructure(_, e)
            | Stmt::Return(Some(e)) => Self::fold_in_expr(e),
            Stmt::FieldAssign(o, _, e) =>
                Self::fold_in_expr(o) || Self::fold_in_expr(e),
            Stmt::IndexAssign(o, i, v) =>
                Self::fold_in_expr(o) || Self::fold_in_expr(i) || Self::fold_in_expr(v),
            Stmt::If(c, then, els) =>
                Self::fold_in_expr(c)
                    || then.iter_mut().any(Self::fold_stmt_exprs)
                    || els.as_mut().map(|b| b.iter_mut().any(Self::fold_stmt_exprs)).unwrap_or(false),
            Stmt::While(c, body) | Stmt::For(_, c, body) =>
                Self::fold_in_expr(c) || body.iter_mut().any(Self::fold_stmt_exprs),
            Stmt::Try(b, _, h) =>
                b.iter_mut().any(Self::fold_stmt_exprs)
                    || h.iter_mut().any(Self::fold_stmt_exprs),
            _ => false,
        }
    }

    /// Fold one constant BinOp inside an expression (depth-first).
    fn fold_in_expr(e: &mut Expr) -> bool {
        // children first so 2*3+1 folds to 6+1 -> 7 over two visits
        match e {
            Expr::BinOp(l, _, r) => {
                if Self::fold_in_expr(l) || Self::fold_in_expr(r) { return true; }
            }
            Expr::UnaryOp(_, i) => { if Self::fold_in_expr(i) { return true; } }
            Expr::Call(_, _, args) | Expr::List(args) =>
                { if args.iter_mut().any(Self::fold_in_expr) { return true; } }
            Expr::MethodCall(o, _, args) =>
                { if Self::fold_in_expr(o) || args.iter_mut().any(Self::fold_in_expr) { return true; } }
            Expr::Index(o, i) =>
                { if Self::fold_in_expr(o) || Self::fold_in_expr(i) { return true; } }
            Expr::SafeGet(o, _, args) => {
                if Self::fold_in_expr(o) { return true; }
                if let Some(a) = args {
                    if a.iter_mut().any(Self::fold_in_expr) { return true; }
                }
            }
            Expr::Slice(o, lo, hi) => {
                if Self::fold_in_expr(o) { return true; }
                if let Some(x) = lo { if Self::fold_in_expr(x) { return true; } }
                if let Some(x) = hi { if Self::fold_in_expr(x) { return true; } }
            }
            Expr::Interp(parts) => {
                for p in parts {
                    if Self::fold_in_expr(p) { return true; }
                }
            }
            Expr::Lambda(_, body) =>
                { if body.iter_mut().any(Self::fold_stmt_exprs) { return true; } }
            Expr::ListComp(x, _, iter, cond) => {
                if Self::fold_in_expr(x) || Self::fold_in_expr(iter) { return true; }
                if let Some(c) = cond { if Self::fold_in_expr(c) { return true; } }
            }
            Expr::StructLit(_, fields) => {
                for (_, x) in fields {
                    if Self::fold_in_expr(x) { return true; }
                }
            }
            _ => {}
        }
        // now try folding self
        if let Some(folded) = Self::const_eval(e) {
            if std::mem::discriminant(&folded) != std::mem::discriminant(e)
                || !Self::expr_is_literal(&folded) {
                *e = folded;
                return true;
            }
            if Self::expr_is_literal(e) { return false; }
            *e = folded;
            return true;
        }
        false
    }

    fn expr_is_literal(e: &Expr) -> bool {
        matches!(e, Expr::Int(_) | Expr::Float(_) | Expr::Bool(_)
                 | Expr::Str(_) | Expr::Null)
    }

    /// Collect names assigned anywhere in a statement list.
    fn collect_assigned(body: &[Stmt], out: &mut std::collections::HashSet<String>) {
        for s in body {
            match s {
                Stmt::Let(n, _) | Stmt::Assign(n, _) => { out.insert(n.clone()); }
                Stmt::LetDestructure(names, _) => {
                    for n in names { out.insert(n.clone()); }
                }
                Stmt::If(_, then, els) => {
                    Self::collect_assigned(then, out);
                    if let Some(b) = els { Self::collect_assigned(b, out); }
                }
                Stmt::While(_, b) | Stmt::For(_, _, b) => Self::collect_assigned(b, out),
                Stmt::Try(b, _, h) => {
                    Self::collect_assigned(b, out);
                    Self::collect_assigned(h, out);
                }
                _ => {}
            }
        }
    }

    /// True if the expression references any variable in `names`.
    fn expr_uses_any(e: &Expr, names: &std::collections::HashSet<String>) -> bool {
        match e {
            Expr::Var(n) => names.contains(n),
            Expr::BinOp(l, _, r) => Self::expr_uses_any(l, names) || Self::expr_uses_any(r, names),
            Expr::UnaryOp(_, i) => Self::expr_uses_any(i, names),
            Expr::Call(n, _, args) => names.contains(n)
                || args.iter().any(|a| Self::expr_uses_any(a, names)),
            Expr::List(items) => items.iter().any(|a| Self::expr_uses_any(a, names)),
            Expr::MethodCall(o, _, args) => Self::expr_uses_any(o, names)
                || args.iter().any(|a| Self::expr_uses_any(a, names)),
            Expr::Index(o, i) => Self::expr_uses_any(o, names) || Self::expr_uses_any(i, names),
            Expr::FieldAccess(o, _) => Self::expr_uses_any(o, names),
            Expr::SafeGet(o, _, args) => Self::expr_uses_any(o, names)
                || args.as_ref().map(|a| a.iter().any(|x| Self::expr_uses_any(x, names))).unwrap_or(false),
            Expr::Slice(o, lo, hi) => Self::expr_uses_any(o, names)
                || lo.as_ref().map(|x| Self::expr_uses_any(x, names)).unwrap_or(false)
                || hi.as_ref().map(|x| Self::expr_uses_any(x, names)).unwrap_or(false),
            Expr::Interp(parts) =>
                parts.iter().any(|x| Self::expr_uses_any(x, names)),
            _ => false,
        }
    }

    /// Try to simplify every expr in a statement (depth-first, first match wins).
    fn simplify_stmt(&mut self, stmt: &mut Stmt) -> bool {
        match stmt {
            Stmt::Let(_, e) | Stmt::Assign(_, e) | Stmt::Print(e) | Stmt::ExprStmt(e) => {
                self.simplify_expr(e)
            }
            Stmt::LetDestructure(_, e) => self.simplify_expr(e),
            Stmt::Return(Some(e)) => self.simplify_expr(e),
            Stmt::FieldAssign(a, _, b) => self.simplify_expr(a) || self.simplify_expr(b),
            Stmt::IndexAssign(o, i, v) => {
                self.simplify_expr(o) || self.simplify_expr(i) || self.simplify_expr(v)
            }
            Stmt::If(c, then, els) => {
                self.simplify_expr(c)
                    || then.iter_mut().any(|s| self.simplify_stmt(s))
                    || els.as_mut().map(|b| b.iter_mut().any(|s| self.simplify_stmt(s))).unwrap_or(false)
            }
            Stmt::While(c, body) | Stmt::For(_, c, body) => {
                self.simplify_expr(c) || body.iter_mut().any(|s| self.simplify_stmt(s))
            }
            Stmt::Try(b, _, h) => {
                b.iter_mut().any(|s| self.simplify_stmt(s))
                    || h.iter_mut().any(|s| self.simplify_stmt(s))
            }
            Stmt::Match(scrut, arms, els) => {
                self.simplify_expr(scrut)
                    || arms.iter_mut().any(|(p, b)| {
                        self.simplify_expr(p) || b.iter_mut().any(|s| self.simplify_stmt(s))
                    })
                    || els.as_mut().map(|b| b.iter_mut().any(|s| self.simplify_stmt(s))).unwrap_or(false)
            }
            Stmt::Grow(crate::ast::GrowItem::Fn(_, _, body, _)) => {
                body.iter_mut().any(|s| self.simplify_stmt(s))
            }
            _ => false,
        }
    }

    /// Apply one algebraic simplification to an expression (self first, then children).
    fn simplify_expr(&mut self, e: &mut Expr) -> bool {
        // check self: e op const patterns
        let replacement: Option<Expr> = if let Expr::BinOp(l, op, r) = e {
            match (op, l.as_ref(), r.as_ref()) {
                // literal folding: Int op Int -> Int/Bool (cross-statement
                // constant folding lands via ConstProp on the stmt level)
                (BinOp::Add, Expr::Int(a), Expr::Int(b)) => Some(Expr::Int(a + b)),
                (BinOp::Sub, Expr::Int(a), Expr::Int(b)) => Some(Expr::Int(a - b)),
                (BinOp::Mul, Expr::Int(a), Expr::Int(b)) => Some(Expr::Int(a * b)),
                (BinOp::Div, Expr::Int(a), Expr::Int(b)) if *b != 0 => Some(Expr::Int(a / b)),
                (BinOp::Mod, Expr::Int(a), Expr::Int(b)) if *b != 0 => Some(Expr::Int(a % b)),
                (BinOp::Eq, Expr::Int(a), Expr::Int(b)) => Some(Expr::Bool(a == b)),
                (BinOp::Neq, Expr::Int(a), Expr::Int(b)) => Some(Expr::Bool(a != b)),
                (BinOp::Lt, Expr::Int(a), Expr::Int(b)) => Some(Expr::Bool(a < b)),
                (BinOp::Gt, Expr::Int(a), Expr::Int(b)) => Some(Expr::Bool(a > b)),
                (BinOp::Le, Expr::Int(a), Expr::Int(b)) => Some(Expr::Bool(a <= b)),
                (BinOp::Ge, Expr::Int(a), Expr::Int(b)) => Some(Expr::Bool(a >= b)),
                // identity rules
                (BinOp::Add, _, Expr::Int(0)) => Some((**l).clone()),
                (BinOp::Add, Expr::Int(0), _) => Some((**r).clone()),
                (BinOp::Sub, _, Expr::Int(0)) => Some((**l).clone()),
                (BinOp::Mul, _, Expr::Int(1)) => Some((**l).clone()),
                (BinOp::Mul, Expr::Int(1), _) => Some((**r).clone()),
                (BinOp::Mul, _, Expr::Int(0)) => Some(Expr::Int(0)),
                (BinOp::Mul, Expr::Int(0), _) => Some(Expr::Int(0)),
                (BinOp::Div, _, Expr::Int(1)) => Some((**l).clone()),
                _ => None,
            }
        } else { None };
        if let Some(n) = replacement { *e = n; return true; }

        // recurse into children
        match e {
            Expr::BinOp(l, _, r) => self.simplify_expr(l) || self.simplify_expr(r),
            Expr::UnaryOp(_, i) => self.simplify_expr(i),
            Expr::Call(_, _, args) | Expr::List(args) => {
                args.iter_mut().any(|a| self.simplify_expr(a))
            }
            Expr::MethodCall(o, _, args) => {
                self.simplify_expr(o) || args.iter_mut().any(|a| self.simplify_expr(a))
            }
            Expr::FieldAccess(o, _) => self.simplify_expr(o),
            Expr::SafeGet(o, _, args) => {
                self.simplify_expr(o)
                    || args.as_mut().map(|a| a.iter_mut().any(|x| self.simplify_expr(x))).unwrap_or(false)
            }
            Expr::Index(o, i) => self.simplify_expr(o) || self.simplify_expr(i),
            Expr::Interp(parts) => parts.iter_mut().any(|p| self.simplify_expr(p)),
            Expr::StructLit(_, fields) => fields.iter_mut().any(|(_, v)| self.simplify_expr(v)),
            Expr::ListComp(head, _, iter, cond) => {
                self.simplify_expr(head)
                    || self.simplify_expr(iter)
                    || cond.as_mut().map(|c| self.simplify_expr(c)).unwrap_or(false)
            }
            Expr::Lambda(_, body) => body.iter_mut().any(|s| self.simplify_stmt(s)),
            _ => false,
        }
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

fn now_ts() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

#[derive(Debug, Clone)]
pub enum EvolutionEvent {
    CycleStart(usize),
    NoCandidates,
    ProbeGenerated { name: String, niche: String },
    CloneRejected { name: String },
    Mutate { name: String, parent_id: u32, new_id: u32, desc: String },
    Trial { name: String, id: u32, healthy: bool, score: f64, fitness: f64 },
    Death { name: String, id: u32, cause: String },
    CycleEnd { cycle: usize, mutations: usize, births: usize, deaths: usize },
}

impl EvolutionEvent {
    pub fn format(&self) -> String {
        match self {
            Self::CycleStart(n) => format!("[EVOLVE] Cycle {}/", n),
            Self::NoCandidates => "  [SKIP] No healthy functions with tests to evolve".into(),
            Self::ProbeGenerated { name, niche } => {
                format!("  [PROBE]  {} — new test generated in niche {}", name, niche)
            }
            Self::CloneRejected { name } => {
                format!("  [REJECT] {} — child identical to existing variant, not born", name)
            }
            Self::Mutate { name, parent_id, new_id, desc } => {
                format!("  [MUTATE] {}#{} → {}#{} ({})", name, parent_id, name, new_id, desc)
            }
            Self::Trial { name, id, healthy, score, fitness } => {
                if *healthy {
                    format!("  [TRIAL]  {}#{} — healthy (score: {:.2}, fitness: {:.2})", name, id, score, fitness)
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
    Simplify,
    ConstFold,
    DeadBranch,
    ConstProp,
    HoistInvariant,
    EarlyExit,
}

/// Fuzz boundary corpus — canonical edge cases per type (std:fuzz).
/// `i` selects the case; returns None for types we can't generate.
pub(crate) fn boundary_value(t: &Type, i: usize) -> Option<crate::interpreter::Value> {
    use crate::interpreter::Value;
    match t {
        Type::Int => Some(Value::Int(match i % 8 {
            0 => i64::MIN, 1 => i64::MAX, 2 => 0, 3 => -1,
            4 => 1, 5 => -1000, 6 => i64::MAX - 1, _ => i64::MIN + 1,
        })),
        Type::Float => Some(Value::Float(match i % 8 {
            0 => 0.0, 1 => -0.0, 2 => f64::INFINITY, 3 => f64::NEG_INFINITY,
            4 => f64::EPSILON, 5 => 1e308, 6 => -1e308, _ => f64::MIN_POSITIVE,
        })),
        Type::Str => {
            let s: String = match i % 8 {
                0 => "".into(), 1 => " ".into(), 2 => "\n\t\r".into(),
                3 => "hello world".into(), 4 => "x".repeat(1024),
                5 => "\\\"'".into(), 6 => "%s%d{0}".into(), _ => "".into(),
            };
            Some(Value::Str(s.into()))
        }
        Type::Bool => Some(Value::Bool(i % 2 == 0)),
        Type::List(inner) => {
            let items: Vec<Value> = match i % 5 {
                0 => vec![],                                              // empty
                1 => vec![boundary_value(inner, 2)?],                     // singleton
                2 => (0..50).map(|j| boundary_value(inner, j % 4).unwrap()).collect(), // mixed
                3 => vec![boundary_value(inner, 1)?; 20],                 // dupes
                _ => vec![],                                              // also empty
            };
            Some(Value::List(items.into_iter().collect()))
        }
        _ => None,
    }
}

/// Constant condition truthiness — for dead-branch elimination.
fn const_truthy(e: &Expr) -> Option<bool> {
    match e {
        Expr::Bool(b) => Some(*b),
        Expr::Int(n) => Some(*n != 0),
        Expr::Null => Some(false),
        _ => None,
    }
}

/// Pure literal — safe to substitute across statements.
fn is_const_expr(e: &Expr) -> bool {
    matches!(e, Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Null)
}

/// All variables written anywhere inside a statement block (recursive).
fn collect_assigned(stmts: &[Stmt]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    fn go(v: &[Stmt], out: &mut std::collections::HashSet<String>) {
        for st in v {
            match st {
                Stmt::Assign(n, _) | Stmt::Let(n, _) => { out.insert(n.clone()); }
                Stmt::LetDestructure(ns, _) => { for n in ns { out.insert(n.clone()); } }
                Stmt::IndexAssign(Expr::Var(n), _, _)
                | Stmt::FieldAssign(Expr::Var(n), _, _) => { out.insert(n.clone()); }
                Stmt::If(_, t, e) => {
                    go(t, out);
                    if let Some(eb) = e { go(eb, out); }
                }
                Stmt::While(_, b) => go(b, out),
                Stmt::For(var, _, b) => { out.insert(var.clone()); go(b, out); }
                Stmt::Try(b, _, h) => { go(b, out); go(h, out); }
                Stmt::Match(_, arms, els) => {
                    for (_, b) in arms { go(b, out); }
                    if let Some(eb) = els { go(eb, out); }
                }
                _ => {}
            }
        }
    }
    go(stmts, &mut out);
    out
}

/// Does `e` read any variable in `vars`? Conservative — unknown
/// composite exprs count as "uses" (never hoist what we can't prove).
fn expr_uses(e: &Expr, vars: &std::collections::HashSet<String>) -> bool {
    match e {
        Expr::Var(n) => vars.contains(n),
        Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Null => false,
        Expr::BinOp(l, _, r) => expr_uses(l, vars) || expr_uses(r, vars),
        Expr::UnaryOp(_, x) => expr_uses(x, vars),
        Expr::Call(_, _, args) | Expr::List(args) | Expr::Interp(args) =>
            args.iter().any(|a| expr_uses(a, vars)),
        Expr::MethodCall(o, _, args) =>
            expr_uses(o, vars) || args.iter().any(|a| expr_uses(a, vars)),
        Expr::FieldAccess(o, _) => expr_uses(o, vars),
        Expr::Index(o, i) => expr_uses(o, vars) || expr_uses(i, vars),
        Expr::SafeGet(o, _, args) =>
            expr_uses(o, vars)
                || args.as_ref().map(|a| a.iter().any(|x| expr_uses(x, vars))).unwrap_or(false),
        Expr::StructLit(_, fields) => fields.iter().any(|(_, v)| expr_uses(v, vars)),
        Expr::ListComp(head, _, iter, cond) =>
            expr_uses(head, vars) || expr_uses(iter, vars)
                || cond.as_ref().map(|c| expr_uses(c, vars)).unwrap_or(false),
        Expr::Lambda(_, _) => true, // closures may capture loop vars
        _ => true,
    }
}

fn type_str(t: &Type) -> String {
    match t {
        Type::Int => "int".into(),
        Type::Float => "float".into(),
        Type::Str => "str".into(),
        Type::Bool => "bool".into(),
        Type::List(inner) => format!("list<{}>", type_str(inner)),
        Type::Fn => "fn".into(),
        Type::Struct(n) => n.clone(),
        Type::Any => "any".into(),
    }
}

fn mutation_name(m: Mutation) -> &'static str {
    match m {
        Mutation::Constant => "constant mutation",
        Mutation::Operator => "operator mutation",
        Mutation::StatementSwap => "statement swap",
        Mutation::DeadCodeRemoval => "dead code removal",
        Mutation::ConstFold => "constant folding",
        Mutation::DeadBranch => "dead branch elimination",
        Mutation::HoistInvariant => "loop-invariant hoist",
        Mutation::Simplify => "simplification",
        Mutation::DeadBranch => "dead-branch elimination",
        Mutation::ConstProp => "constant propagation",
        Mutation::HoistInvariant => "loop-invariant hoisting",
        Mutation::EarlyExit => "early-exit fusion",
    }
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
            Box::new(Expr::BinOp(
                Box::new(Expr::Var("b".into())),
                BinOp::Add,
                Box::new(Expr::Int(1)),
            )),
        )))], vec![TestAssert {
            call: Expr::Call("add".into(), None, vec![Expr::Int(2), Expr::Int(3)]),
            expected: Some(Expr::Int(6)),
        }], "fn add(a, b) { return a + b + 1 }");

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

#[cfg(test)]
mod phase7_tests {
    use super::*;
    use crate::ast::*;

    fn ev() -> Evolver { Evolver::new() }

    #[test]
    fn const_eval_folds_binop() {
        let e = Expr::BinOp(
            Box::new(Expr::Int(2)), BinOp::Mul, Box::new(Expr::Int(3)));
        assert!(matches!(Evolver::const_eval(&e), Some(Expr::Int(6))));
    }

    #[test]
    fn const_eval_rejects_nonconst() {
        let e = Expr::BinOp(
            Box::new(Expr::Var("x".into())), BinOp::Mul, Box::new(Expr::Int(3)));
        assert!(Evolver::const_eval(&e).is_none());
    }

    #[test]
    fn dead_branch_splices_taken_arm() {
        let body = vec![
            Stmt::If(Expr::Bool(false),
                vec![Stmt::Let("a".into(), Expr::Int(1))],
                Some(vec![Stmt::Let("b".into(), Expr::Int(2))])),
        ];
        let mut e = ev();
        let out = e.mutate_body(&body, Mutation::DeadBranch);
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Stmt::Let(n, _) if n == "b"));
    }

    #[test]
    fn dead_branch_removes_false_while() {
        let body = vec![
            Stmt::While(Expr::Bool(false), vec![Stmt::Let("a".into(), Expr::Int(1))]),
        ];
        let mut e = ev();
        let out = e.mutate_body(&body, Mutation::DeadBranch);
        assert!(out.is_empty());
    }

    #[test]
    fn hoist_invariant_lifts_let() {
        let body = vec![
            Stmt::While(Expr::BinOp(
                    Box::new(Expr::Var("i".into())), BinOp::Lt, Box::new(Expr::Var("n".into()))),
                vec![
                    Stmt::Let("scale".into(), Expr::BinOp(
                        Box::new(Expr::Int(2)), BinOp::Mul, Box::new(Expr::Int(50)))),
                    Stmt::Assign("total".into(), Expr::Var("scale".into())),
                    Stmt::Assign("i".into(), Expr::Int(1)),
                ]),
        ];
        let mut e = ev();
        let out = e.mutate_body(&body, Mutation::HoistInvariant);
        // first stmt should now be the hoisted let
        assert!(matches!(&out[0], Stmt::Let(n, _) if n == "scale"));
        // while body no longer contains it
        if let Stmt::While(_, b) = &out[1] {
            assert!(!b.iter().any(|s| matches!(s, Stmt::Let(n, _) if n == "scale")));
        } else { panic!("expected while second"); }
    }

    #[test]
    fn hoist_skips_loop_dependent_lets() {
        // `let k = i + 1` reads a loop-mutated var — must NOT hoist
        let body = vec![
            Stmt::While(Expr::BinOp(
                    Box::new(Expr::Var("i".into())), BinOp::Lt, Box::new(Expr::Var("n".into()))),
                vec![
                    Stmt::Let("k".into(), Expr::BinOp(
                        Box::new(Expr::Var("i".into())), BinOp::Add, Box::new(Expr::Int(1)))),
                    Stmt::Assign("i".into(), Expr::Int(1)),
                ]),
        ];
        let mut e = ev();
        let out = e.mutate_body(&body, Mutation::HoistInvariant);
        assert!(matches!(&out[0], Stmt::While(_, _)));
    }
}
