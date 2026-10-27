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
    /// Variant-level filter expression — only breed from variants
    /// matching every &&-clause (the garden --filter DSL).
    pub filter: Option<String>,
    pub use_vm: bool,   // birth-trials on the bytecode VM + JIT
    /// Sort the candidate pool by a variant field before selection
    /// — the garden --sort DSL applied to the breeding order.
    pub sort: Option<String>,
    /// Self-maintaining cycles — heal sick variants and run the
    /// mechanical dead-code repairs across the garden each cycle.
    pub tend: bool,
    /// Families whose fittest variant's body isn't side-effect-free —
    /// `let x = f()` where f is impure must NOT hoist (it would run the
    /// side effect once instead of per-iteration). Recomputed at the
    /// start of each evolve run by fixpoint scan.
    impure_fns: std::collections::HashSet<String>,
}

impl Evolver {
    pub fn new() -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(42);
        Evolver { rng_seed: seed, llm: false, filter: None, sort: None,
                  use_vm: false, tend: false,
                  impure_fns: std::collections::HashSet::new() }
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
        self.evolve_scoped(veldt, cycles, None)
    }

    /// Evolve with an optional family scope — `only` restricts candidates
    /// to a single lineage (targeted mutation on one family).
    pub fn evolve_scoped(&mut self, veldt: &mut Veldt, cycles: usize, only: Option<&str>) -> Vec<EvolutionEvent> {
        self.impure_fns = Self::impure_families(&veldt.functions);
        let mut events = Vec::new();

        for cycle in 1..=cycles {
            events.push(EvolutionEvent::CycleStart(cycle));
            if self.tend {
                let (healed, repaired) = self.tend_garden(veldt);
                if healed + repaired > 0 {
                    events.push(EvolutionEvent::Tended { healed, repaired });
                }
            }
            let pre = veldt.functions.values().flatten()
                .map(|e| e.fitness).fold(f64::NEG_INFINITY, f64::max);

            let budget = 10 + (veldt.run_count * 2);
            let mut spent = 0;

            // Collect candidates: healthy functions with tests, sorted by usage (high usage = important)
            let or_groups: Vec<Vec<String>> = self.filter.as_ref()
                .map(|f| f.split("||")
                    .map(|g| g.split("&&").map(|s| s.trim().to_string()).collect())
                    .collect())
                .unwrap_or_default();
            let candidates: Vec<(String, u32)> = veldt.functions.iter()
                .filter(|(name, _)| only.map(|n| n == name.as_str()).unwrap_or(true))
                .flat_map(|(name, variants)| {
                    variants.iter()
                        .filter(|e| matches!(e.health, Health::Healthy) && !e.tests.is_empty()
                            && (or_groups.is_empty() || or_groups.iter().any(|g|
                                g.iter().all(|c| crate::veldt::variant_matches_in(e, name, c)))))
                        .map(|e| (name.clone(), e.id))
                        .collect::<Vec<_>>()
                })
                .collect();

            // --sort <field> — order the candidate pool by a variant field
            let mut candidates = candidates;
            if let Some(field) = &self.sort {
                let key = |name: &str, id: u32| -> f64 {
                    veldt.functions.get(name)
                        .and_then(|vs| vs.iter().find(|e| e.id == id))
                        .map(|e| match field.as_str() {
                            "fitness" => e.fitness,
                            "age" => e.age as f64,
                            "usage" => e.usage_count as f64,
                            "size" => crate::fmt::format_program(&e.body).len() as f64,
                            "args" => e.params.len() as f64,
                            "lifespan" => e.lifespan,
                            "last_used" => e.last_used as f64,
                            "niches" => e.niche_scores.len() as f64,
                            "fit_rate" => e.fitness / (e.age as f64 + 1.0),
                            "usage_rate" => e.usage_count as f64 / (e.age as f64 + 1.0),
                            _ => 0.0,
                        })
                        .unwrap_or(0.0)
                };
                candidates.sort_by(|(na, ia), (nb, ib)| {
                    key(nb, *ib).partial_cmp(&key(na, *ia))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }

            if candidates.is_empty() {
                events.push(EvolutionEvent::NoCandidates);
                events.push(EvolutionEvent::CycleEnd {
                    cycle, mutations: 0, births: 0, deaths: 0, delta: 0.0,
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
                    HealthChecker::check_at_birth_with(
                        &mut e, veldt, self.use_vm);
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

                let (mut child_body, parents, origin) = if do_crossover {
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

                // Mechanical normalization: run the repair passes on the
                // child before judging it — mutations that only splice in
                // dead code normalize back to the parent shape and get
                // CloneRejected instead of being born dirty
                crate::health::HealthChecker::repair_dead_tail(&mut child_body);
                crate::health::HealthChecker::repair_dead_lets(&mut child_body);

                // Semantic dedup: reject children identical to an existing variant
                if let Some(variants) = veldt.functions.get(name) {
                    let child_key = format!("{:?}", child_body);
                    if variants.iter().any(|e| format!("{:?}", e.body) == child_key) {
                        events.push(EvolutionEvent::CloneRejected { name: name.clone() });
                        spent += 1;
                        continue;
                    }
                }

                // Strict-lint gate: reject mutants that introduce real-bug
                // warnings the parent didn't already carry — mutations
                // shouldn't birth unreachable/self-reading/arity-broken code
                {
                    let parent_warns = mutant_realbugs(name, &entry.params,
                        &entry.body, &entry.tests);
                    let child_warns = mutant_realbugs(name, &entry.params,
                        &child_body, &entry.tests);
                    let new_warns: Vec<String> = child_warns.iter()
                        .filter(|w| !parent_warns.contains(w))
                        .cloned().collect();
                    if !new_warns.is_empty() {
                        events.push(EvolutionEvent::LintRejected {
                            name: name.clone(),
                            warnings: new_warns,
                        });
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
                HealthChecker::check_at_birth_with(
                    &mut new_entry, veldt, self.use_vm);

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
                    parent_fitness: entry.fitness,
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

            let post = veldt.functions.values().flatten()
                .map(|e| e.fitness).fold(f64::NEG_INFINITY, f64::max);
            events.push(EvolutionEvent::CycleEnd {
                cycle,
                mutations,
                births,
                deaths: obituaries.len(),
                delta: post - pre,
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
        interp.caps = crate::interpreter::Caps::sandboxed();
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
    pub(crate) fn niche_signature(args: &[crate::interpreter::Value]) -> String {
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
        match self.rand_range(11) {
            0 => Mutation::Constant,
            1 => Mutation::Operator,
            2 => Mutation::StatementSwap,
            3 => Mutation::DeadCodeRemoval,
            4 => Mutation::ConstFold,
            5 => Mutation::DeadBranch,
            6 => Mutation::ConstProp,
            7 => Mutation::HoistInvariant,
            8 => Mutation::EarlyExit,
            9 => Mutation::IdiomRewrite,
            _ => Mutation::Simplify,
        }
    }

    /// Pick + apply a mutation; re-roll (≤6x) when it leaves the body
    /// unchanged — conditional operators can't fire on every shape, and
    /// no-op children only burn budget downstream (clone rejection).
    fn mutate_retry(&mut self, body: &[Stmt]) -> (Vec<Stmt>, Mutation) {
        let parent_key = format!("{:?}", body);
        let rewritten = self.mutate_body(body, Mutation::IdiomRewrite);
        if format!("{:?}", rewritten) != parent_key {
            return (rewritten, Mutation::IdiomRewrite);
        }
        let mut last = Mutation::Simplify;
        let mut out = body.to_vec();
        for _ in 0..6 {
            last = self.pick_mutation();
            out = self.mutate_body(body, last);
            if format!("{:?}", out) != parent_key { break; }
        }
        (out, last)
    }

    /// Deterministic idiom rewrite for the CLI — returns the rewritten
    /// body (unchanged body = no idiom matched).
    pub fn rewrite_idioms(&mut self, body: &[Stmt]) -> Vec<Stmt> {
        Self::idiom_rewrite_body(body)
    }

    /// Self-free idiom pass — usable from &self contexts (tend, repair).
    pub fn idiom_rewrite_body(body: &[Stmt]) -> Vec<Stmt> {
        if let Some((xs, is_max)) = Self::detect_sort_extremum(body) {
            Self::make_linear_extremum_scan(&xs, is_max)
        } else {
            let mut b = body.to_vec();
            Self::apply_idiom_pass(&mut b);
            b
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
                                // RHS must read no loop-mutated var, the
                                // name must not be re-assigned inside
                                // (else hoisting changes reset semantics),
                                // and the RHS must be side-effect-free —
                                // hoisting `print(x)` would fire it once
                                // instead of per-iteration
                                !Self::expr_uses_any(e, &assigned)
                                    && !body.iter().any(|s2| matches!(s2,
                                        Stmt::Assign(n2, _) if n2 == name))
                                    && self.expr_hoistable(e)
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
            Mutation::IdiomRewrite => {
                if let Some((xs, is_max)) = Self::detect_sort_extremum(&new_body) {
                    new_body = Self::make_linear_extremum_scan(&xs, is_max);
                } else {
                    Self::apply_idiom_pass(&mut new_body);
                }
            }
        }

        new_body
    }

    /// Detect O(n log n) sort-to-find-extremum idioms:
    /// - `return xs.sort().reverse()[0]` or `let s = xs.sort(); let r = s.reverse(); return r[0]` -> Some((xs, true))
    /// - `return xs.sort()[0]` or `let s = xs.sort(); return s[0]` -> Some((xs, false))
    fn detect_sort_extremum(stmts: &[Stmt]) -> Option<(String, bool)> {
        match stmts {
            // Inline: return xs.sort().reverse()[0] or return xs.sort()[0]
            [Stmt::Return(Some(Expr::Index(recv, idx)))] if matches!(idx.as_ref(), Expr::Int(0)) => {
                match recv.as_ref() {
                    Expr::MethodCall(inner, rev_m, rev_args)
                        if rev_m == "reverse" && rev_args.is_empty() =>
                    {
                        if let Expr::MethodCall(base, sort_m, sort_args) = inner.as_ref() {
                            if sort_m == "sort" && sort_args.is_empty() {
                                if let Expr::Var(xs) = base.as_ref() {
                                    return Some((xs.clone(), true));
                                }
                            }
                        }
                        None
                    }
                    Expr::MethodCall(base, sort_m, sort_args)
                        if sort_m == "sort" && sort_args.is_empty() =>
                    {
                        if let Expr::Var(xs) = base.as_ref() {
                            return Some((xs.clone(), false));
                        }
                        None
                    }
                    _ => None,
                }
            }
            // Staged max: let s = xs.sort(); let r = s.reverse(); return r[0]
            [Stmt::Let(s, Expr::MethodCall(base, sort_m, sort_args)),
             Stmt::Let(r, Expr::MethodCall(s_var, rev_m, rev_args)),
             Stmt::Return(Some(Expr::Index(r_var, idx)))]
                if sort_m == "sort"
                    && sort_args.is_empty()
                    && rev_m == "reverse"
                    && rev_args.is_empty()
                    && matches!(idx.as_ref(), Expr::Int(0)) =>
            {
                if let (Expr::Var(xs), Expr::Var(s2), Expr::Var(r2)) =
                    (base.as_ref(), s_var.as_ref(), r_var.as_ref())
                {
                    if s == s2 && r == r2 {
                        return Some((xs.clone(), true));
                    }
                }
                None
            }
            // Staged min: let s = xs.sort(); return s[0]
            [Stmt::Let(s, Expr::MethodCall(base, sort_m, sort_args)),
             Stmt::Return(Some(Expr::Index(s_var, idx)))]
                if sort_m == "sort"
                    && sort_args.is_empty()
                    && matches!(idx.as_ref(), Expr::Int(0)) =>
            {
                if let (Expr::Var(xs), Expr::Var(s2)) = (base.as_ref(), s_var.as_ref()) {
                    if s == s2 {
                        return Some((xs.clone(), false));
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Recursive idiom pass: finds sort-extremum windows ANYWHERE in a
    /// body (nested bodies included), plus `let x = xs.sort()...[0]`
    /// bindings — not just whole-body shapes.
    fn apply_idiom_pass(stmts: &mut Vec<Stmt>) -> bool {
        for s in stmts.iter_mut() {
            let changed = match s {
                Stmt::If(_, t, e) =>
                    Self::apply_idiom_pass(t)
                        || e.as_mut().map(|b| Self::apply_idiom_pass(b)).unwrap_or(false),
                Stmt::While(_, b) | Stmt::For(_, _, b) | Stmt::FnDef(_, _, b) =>
                    Self::apply_idiom_pass(b),
                Stmt::Try(t, _, c) =>
                    Self::apply_idiom_pass(t) || Self::apply_idiom_pass(c),
                Stmt::Match(_, arms, els) =>
                    arms.iter_mut().any(|(_, b)| Self::apply_idiom_pass(b))
                        || els.as_mut().map(|b| Self::apply_idiom_pass(b)).unwrap_or(false),
                _ => false,
            };
            if changed { return true; }
        }
        for i in 0..stmts.len() {
            if let Some((span, repl)) = Self::idiom_window(&stmts[i..]) {
                stmts.splice(i..i + span, repl);
                return true;
            }
        }
        false
    }

    /// Match one idiom window at the head of `win`; returns (span, replacement).
    fn idiom_window(win: &[Stmt]) -> Option<(usize, Vec<Stmt>)> {
        // is e an `Index(sort/rev chain, 0|last)`? -> (base expr, is_max)
        fn extremum(e: &Expr) -> Option<(Expr, bool)> {
            if let Expr::Index(recv, idx) = e {
                // last-element index: -1 or <same recv>.len() - 1
                let is_last = |idx: &Expr, recv: &Expr| -> bool {
                    if matches!(idx, Expr::Int(-1))
                        || matches!(idx, Expr::UnaryOp(UnaryOp::Neg, i)
                            if matches!(i.as_ref(), Expr::Int(1))) { return true; }
                    if let Expr::BinOp(l, BinOp::Sub, r) = idx {
                        if matches!(r.as_ref(), Expr::Int(1)) {
                            if let Expr::MethodCall(base, m, a) = l.as_ref() {
                                return m == "len" && a.is_empty() && base.as_ref() == recv;
                            }
                        }
                    }
                    false
                };
                match (recv.as_ref(), idx.as_ref()) {
                    // xs.sort().reverse()[0] | xs.sort()[-1 | len-1] -> max
                    (Expr::MethodCall(inner, m, a), _)
                        if m == "reverse" && a.is_empty()
                            && matches!(idx.as_ref(), Expr::Int(0)) =>
                    {
                        if let Expr::MethodCall(base, sm, sa) = inner.as_ref() {
                            if sm == "sort" && sa.is_empty() {
                                return Some(((**base).clone(), true));
                            }
                        }
                        None
                    }
                    (Expr::MethodCall(inner, m, a), _)
                        if m == "reverse" && a.is_empty() && is_last(idx, recv) =>
                    {
                        // sort().reverse()[last] -> min
                        if let Expr::MethodCall(base, sm, sa) = inner.as_ref() {
                            if sm == "sort" && sa.is_empty() {
                                return Some(((**base).clone(), false));
                            }
                        }
                        None
                    }
                    (Expr::MethodCall(base, sm, sa), _)
                        if sm == "sort" && sa.is_empty()
                            && matches!(idx.as_ref(), Expr::Int(0)) =>
                        Some(((**base).clone(), false)),
                    (Expr::MethodCall(base, sm, sa), _)
                        if sm == "sort" && sa.is_empty() && is_last(idx, recv) =>
                        Some(((**base).clone(), true)),
                    _ => None,
                }
            } else {
                None
            }
        }
        // 3-stmt staged: let s = xs.sort(); let r = s.reverse(); <tail r[0]>
        if win.len() >= 3 {
            if let (Stmt::Let(s, Expr::MethodCall(base, sm, sa)),
                    Stmt::Let(r, Expr::MethodCall(recv, rm, ra))) = (&win[0], &win[1]) {
                if sm == "sort" && sa.is_empty() && rm == "reverse" && ra.is_empty()
                    && matches!(recv.as_ref(), Expr::Var(v) if v == s) {
                    if let Some((tgt, ret, is_last)) = tail_uses(&win[2], r) {
                        let mut out = Self::scan_binding(base, &tgt, !is_last);
                        if ret { out.push(Stmt::Return(Some(Expr::Var(tgt)))); }
                        return Some((3, out));
                    }
                }
            }
        }
        // 2-stmt staged: let s = xs.sort(); <tail s[0]>
        if win.len() >= 2 {
            if let Stmt::Let(s, Expr::MethodCall(base, sm, sa)) = &win[0] {
                if sm == "sort" && sa.is_empty() {
                    if let Some((tgt, ret, is_last)) = tail_uses(&win[1], s) {
                        let mut out = Self::scan_binding(base, &tgt, is_last);
                        if ret { out.push(Stmt::Return(Some(Expr::Var(tgt)))); }
                        return Some((2, out));
                    }
                }
            }
        }
        // 1-stmt inline: return <extremum>  /  let x = <extremum>
        if let Some(stmt) = win.first() {
            match stmt {
                Stmt::Return(Some(e)) => {
                    if let Some((base, is_max)) = extremum(e) {
                        let mut out = Self::scan_binding(&base, "m", is_max);
                        out.push(Stmt::Return(Some(Expr::Var("m".into()))));
                        return Some((1, out));
                    }
                }
                Stmt::Let(name, e) => {
                    if let Some((base, is_max)) = extremum(e) {
                        return Some((1, Self::scan_binding(&base, name, is_max)));
                    }
                }
                _ => {}
            }
        }
        if let Some(r) = Self::accum_window(win) { return Some(r); }
        if let Some(r) = Self::linsearch_window(win) { return Some(r); }
        if let Some(r) = Self::dedup_window(win) { return Some(r); }
        if let Some(r) = Self::contains_window(win) { return Some(r); }
        if let Some(r) = Self::while_contains_window(win) { return Some(r); }
        if let Some(r) = Self::count_window(win) { return Some(r); }
        if let Some(r) = Self::map_filter_window(win) { return Some(r); }
        Self::cse_window(win)
    }

    /// Count-occurrences loop -> `xs.count(t)`:
    ///   `let c = 0; for x in xs { if x == t { c += 1 } } return c`
    fn count_window(win: &[Stmt]) -> Option<(usize, Vec<Stmt>)> {
        if win.len() < 3 { return None; }
        let c_name = match &win[0] {
            Stmt::Let(n, Expr::Int(0)) | Stmt::Assign(n, Expr::Int(0)) => n.clone(),
            _ => return None,
        };
        let (x, xs, t) = match &win[1] {
            Stmt::For(x, xs, body) if body.len() == 1 => {
                match &body[0] {
                    Stmt::If(
                        Expr::BinOp(l, BinOp::Eq, r),
                        then_b, els
                    ) if els.is_none() && then_b.len() == 1
                        && matches!(&then_b[0],
                            Stmt::Assign(t2, Expr::BinOp(l2, BinOp::Add, r2))
                                if *t2 == c_name
                                && matches!(l2.as_ref(), Expr::Var(v) if *v == c_name)
                                && matches!(r2.as_ref(), Expr::Int(1))) =>
                    {
                        let (l, r) = (l.as_ref(), r.as_ref());
                        let t = if matches!(l, Expr::Var(v) if v == x) { r }
                            else if matches!(r, Expr::Var(v) if v == x) { l }
                            else { return None; };
                        if matches!(t, Expr::Var(v) if v == x) { return None; }
                        (x.clone(), xs.clone(), t.clone())
                    }
                    _ => return None,
                }
            }
            _ => return None,
        };
        if !matches!(&win[2], Stmt::Return(Some(e)) if matches!(e, Expr::Var(v) if *v == c_name)) {
            return None;
        }
        let set: std::collections::HashSet<String> =
            [x.clone(), c_name.clone()].into_iter().collect();
        if expr_uses(&t, &set) { return None; }
        Some((3, vec![Stmt::Return(Some(Expr::MethodCall(
            Box::new(xs), "count".into(), vec![t])))]))
    }

    /// Membership early-exit loop -> `xs.contains(t)`:
    ///   `for x in xs { if x == t { return true } } return false`
    fn contains_window(win: &[Stmt]) -> Option<(usize, Vec<Stmt>)> {
        if win.len() < 2 { return None; }
        let (x, xs, t) = match &win[0] {
            Stmt::For(x, xs, body) if body.len() == 1 => {
                match &body[0] {
                    Stmt::If(
                        Expr::BinOp(l, BinOp::Eq, r),
                        then_b, els
                    ) if els.is_none() && then_b.len() == 1
                        && matches!(&then_b[0], Stmt::Return(Some(e))
                            if matches!(e, Expr::Bool(true))) =>
                    {
                        let (l, r) = (l.as_ref(), r.as_ref());
                        let t = if matches!(l, Expr::Var(v) if v == x) { r }
                            else if matches!(r, Expr::Var(v) if v == x) { l }
                            else { return None; };
                        if matches!(t, Expr::Var(v) if v == x) { return None; }
                        (x.clone(), xs.clone(), t.clone())
                    }
                    _ => return None,
                }
            }
            _ => return None,
        };
        if !matches!(&win[1], Stmt::Return(Some(e)) if matches!(e, Expr::Bool(false))) {
            return None;
        }
        let set: std::collections::HashSet<String> = [x.clone()].into_iter().collect();
        if expr_uses(&t, &set) { return None; }
        Some((2, vec![Stmt::Return(Some(Expr::MethodCall(
            Box::new(xs), "contains".into(), vec![t])))]))
    }

    /// While-index membership test -> `xs.contains(t)`:
    ///   `let i=0; while i<xs.len() { if xs[i]==t { return true } i+=1 }
    ///    return false`
    fn while_contains_window(win: &[Stmt]) -> Option<(usize, Vec<Stmt>)> {
        if win.len() < 3 { return None; }
        let i_name = match &win[0] {
            Stmt::Let(n, Expr::Int(0)) | Stmt::Assign(n, Expr::Int(0)) => n.clone(),
            _ => return None,
        };
        let (xs, t) = match &win[1] {
            Stmt::While(cond, body) => {
                let xs = match cond {
                    Expr::BinOp(l, BinOp::Lt, r) => {
                        if !matches!(l.as_ref(), Expr::Var(v) if *v == i_name) { return None; }
                        match r.as_ref() {
                            Expr::MethodCall(b, m, a) if m == "len" && a.is_empty() =>
                                (**b).clone(),
                            _ => return None,
                        }
                    }
                    _ => return None,
                };
                if body.len() != 2 { return None; }
                let is_incr = |s: &Stmt| matches!(s,
                    Stmt::Assign(t2, Expr::BinOp(l2, BinOp::Add, r2))
                        if *t2 == i_name
                        && matches!(l2.as_ref(), Expr::Var(v) if *v == i_name)
                        && matches!(r2.as_ref(), Expr::Int(1)));
                let if_s = if is_incr(&body[0]) { &body[1] }
                    else if is_incr(&body[1]) { &body[0] }
                    else { return None; };
                let t = match if_s {
                    Stmt::If(c, then_b, _) if then_b.len() == 1
                        && matches!(&then_b[0], Stmt::Return(Some(e))
                            if matches!(e, Expr::Bool(true))) =>
                    {
                        match c {
                            Expr::BinOp(l, BinOp::Eq, r) => {
                                let elem = |e: &Expr| matches!(e,
                                    Expr::Index(b, i2)
                                        if b.as_ref() == &xs
                                        && matches!(i2.as_ref(), Expr::Var(v) if *v == i_name));
                                if elem(l) { (**r).clone() }
                                else if elem(r) { (**l).clone() }
                                else { return None; }
                            }
                            _ => return None,
                        }
                    }
                    _ => return None,
                };
                (xs, t)
            }
            _ => return None,
        };
        if !matches!(&win[2], Stmt::Return(Some(e)) if matches!(e, Expr::Bool(false))) {
            return None;
        }
        let set: std::collections::HashSet<String> =
            [i_name.clone()].into_iter().collect();
        if expr_uses(&t, &set) { return None; }
        Some((3, vec![Stmt::Return(Some(Expr::MethodCall(
            Box::new(xs), "contains".into(), vec![t])))]))
    }

    /// `let dy = E` where an earlier `let dx = E` (identical pure
    /// expr) already exists and `dx` is never reassigned in the window:
    /// drop the redundant let and rename `dy` -> `dx` downstream.
    /// Restricted to pure exprs (no calls / no mutations) so renaming
    /// can't skip a side effect. Window spans the whole block —
    /// searches every earlier let, not just win[0].
    fn cse_window(win: &[Stmt]) -> Option<(usize, Vec<Stmt>)> {
        fn pure(e: &Expr) -> bool {
            match e {
                Expr::Int(_) | Expr::Float(_) | Expr::Str(_)
                | Expr::Bool(_) | Expr::Var(_) | Expr::Null => true,
                Expr::BinOp(l, _, r) => pure(l) && pure(r),
                Expr::UnaryOp(_, x) => pure(x),
                Expr::Index(o, i) => pure(o) && pure(i),
                Expr::List(xs) => xs.iter().all(pure),
                Expr::MethodCall(o, m, a) =>
                    matches!(m.as_str(),
                        "len"|"contains"|"index_of"|"first"|"last"|"sum"
                        |"min"|"max"|"product"|"get")
                    && pure(o) && a.iter().all(pure),
                _ => false,
            }
        }
        // find (earlier-let-name, shared-expr) pairs where the same
        // expr is rebound later in the block
        for i in 1..win.len() {
            let (name, expr) = match &win[i] {
                Stmt::Let(n, e) if pure(e) => (n.clone(), e.clone()),
                _ => continue,
            };
            let mut earlier: Option<String> = None;
            let mut shadowed = false;
            for st in &win[..i] {
                match st {
                    Stmt::Let(n, e) => {
                        if format!("{:?}", e) == format!("{:?}", expr) && *n != name {
                            earlier = Some(n.clone());
                        }
                        if *n == name { shadowed = true; } // same name rebound — bail
                    }
                    Stmt::Assign(n, _) => {
                        if Some(n) == earlier.as_ref() { earlier = None; }
                    }
                    _ => {}
                }
            }
            if shadowed { continue; }
            if let Some(dx) = earlier {
                // bail when the dup var is REASSIGNED downstream —
                // aliasing would smash dx with dy's new value. Recursive:
                // assignments inside If/While/Try bodies count too.
                fn assigns_to(stmts: &[Stmt], name: &str) -> bool {
                    stmts.iter().any(|st| match st {
                        Stmt::Assign(n, _) if n == name => true,
                        Stmt::If(_, t, b) => assigns_to(t, name)
                            || b.as_ref().map(|x| assigns_to(x, name)).unwrap_or(false),
                        Stmt::While(_, b) | Stmt::For(_, _, b) => assigns_to(b, name),
                        Stmt::Try(t, _, c) => assigns_to(t, name) || assigns_to(c, name),
                        Stmt::Match(_, arms, els) =>
                            arms.iter().any(|(_, b)| assigns_to(b, name))
                            || els.as_ref().map(|x| assigns_to(x, name)).unwrap_or(false),
                        _ => false,
                    })
                }
                if assigns_to(&win[i+1..], &name) { continue; }
                // rewrite: keep win[..i], drop the redundant let, rename
                let mut out: Vec<Stmt> = win[..i].to_vec();
                for st in &win[i+1..] {
                    out.push(subst_var_stmt(st, &name, &dx));
                }
                // dx must not be reassigned later either (same hazard,
                // recursive through nested blocks).
                if assigns_to(&out, &dx) { continue; }
                return Some((win.len(), out));
            }
        }
        None
    }

    /// O(n^2) hand-rolled dedup -> `xs.unique()`:
    ///   `let out = []` + `for x in xs { if not x in out { out = out.push(x) } }`
    ///   (also `!out.contains(x)` form)
    fn dedup_window(win: &[Stmt]) -> Option<(usize, Vec<Stmt>)> {
        if win.len() < 2 { return None; }
        let out_name = match &win[0] {
            Stmt::Let(n, Expr::List(items)) if items.is_empty() => n.clone(),
            _ => return None,
        };
        if let Stmt::For(x, xs, body) = &win[1] {
            if body.len() != 1 { return None; }
            if let Stmt::If(cond, then_b, None) = &body[0] {
                if then_b.len() != 1 { return None; }
                // condition: `not x in out` / `not out.contains(x)` /
                // `!(x in out)`
                let is_member_test = |e: &Expr| -> bool {
                    match e {
                        Expr::BinOp(l, BinOp::In, r) =>
                            matches!(l.as_ref(), Expr::Var(v) if v == x)
                                && matches!(r.as_ref(), Expr::Var(v) if v == &out_name),
                        Expr::MethodCall(o, m, a) if m == "contains" && a.len() == 1 =>
                            matches!(o.as_ref(), Expr::Var(v) if v == &out_name)
                                && matches!(&a[0], Expr::Var(v) if v == x),
                        _ => false,
                    }
                };
                let negated = match cond {
                    Expr::UnaryOp(UnaryOp::Not, inner) => is_member_test(inner),
                    _ => false,
                };
                if !negated { return None; }
                // then: out = out.push(x)
                let is_push = matches!(&then_b[0],
                    Stmt::Assign(t, Expr::MethodCall(o, m, a))
                        if t == &out_name && m == "push" && a.len() == 1
                        && matches!(o.as_ref(), Expr::Var(v) if v == &out_name)
                        && matches!(&a[0], Expr::Var(v) if v == x));
                if !is_push { return None; }
                return Some((2, vec![Stmt::Let(out_name, Expr::MethodCall(
                    Box::new(xs.clone()), "unique".into(), vec![]))]));
            }
        }
        None
    }

    /// `let out = []; for x in xs { out = out.push(f(x)) } return out`
    /// (or `out.append(f(x))`)  ->  `return xs.map(fn(x){ return f(x) })`.
    /// Filter form: `if cond { out = out.push(x) }` -> `xs.filter(fn(x){return cond})`.
    fn map_filter_window(win: &[Stmt]) -> Option<(usize, Vec<Stmt>)> {
        if win.len() < 3 { return None; }
        let out_name = match &win[0] {
            Stmt::Let(n, Expr::List(items)) if items.is_empty() => n.clone(),
            _ => return None,
        };
        if let Stmt::For(x, xs, body) = &win[1] {
            if body.is_empty() { return None; }
            // `return out` right after the loop
            let ret_out = matches!(&win[2],
                Stmt::Return(Some(Expr::Var(v))) if v == &out_name);
            if !ret_out { return None; }
            let lam = |body_stmts: Vec<Stmt>| Expr::Lambda(
                vec![crate::ast::Param {
                    name: x.clone(),
                    typ: crate::ast::Type::Any,
                }],
                body_stmts,
            );
            // helper: is `s` an `out.append(E)` or `out = out.push(E)`?
            let pushed = |s: &Stmt| -> Option<Expr> {
                match s {
                    Stmt::ExprStmt(Expr::MethodCall(o, m, a))
                        if m == "append" && a.len() == 1
                        && matches!(o.as_ref(), Expr::Var(v) if v == &out_name) =>
                        Some(a[0].clone()),
                    Stmt::Assign(t, Expr::MethodCall(o, m, a))
                        if t == &out_name && m == "push" && a.len() == 1
                        && matches!(o.as_ref(), Expr::Var(v) if v == &out_name) =>
                        Some(a[0].clone()),
                    _ => None,
                }
            };
            let mut out_used = std::collections::HashSet::new();
            out_used.insert(out_name.clone());
            // map form: body == [ push(f(x)) ]
            if body.len() == 1 {
                if let Some(f) = pushed(&body[0]) {
                    if expr_uses(&f, &out_used) { return None; }
                    // map-identity `out.push(x)` -> `return xs`
                    if matches!(&f, Expr::Var(v) if v == x) {
                        return Some((3, vec![Stmt::Return(Some(
                            xs.clone()))]));
                    }
                    return Some((3, vec![Stmt::Return(Some(
                        Expr::MethodCall(
                            Box::new(xs.clone()),
                            "map".into(),
                            vec![lam(vec![Stmt::Return(Some(f))])],
                        )))]));
                }
            }
            // filter form: body == [ if cond { push(x) } ]
            if let Stmt::If(cond, then_b, None) = &body[0] {
                if body.len() != 1 || then_b.len() != 1 { return None; }
                if let Some(f) = pushed(&then_b[0]) {
                    if !matches!(&f, Expr::Var(v) if v == x) { return None; }
                    if expr_uses(cond, &out_used) { return None; }
                    return Some((3, vec![Stmt::Return(Some(
                        Expr::MethodCall(
                            Box::new(xs.clone()),
                            "filter".into(),
                            vec![lam(vec![Stmt::Return(Some(cond.clone()))])],
                        )))]));
                }
            }
        }
        None
    }

    /// O(n) hand-rolled linear search -> `xs.index_of(t)` builtin:
    ///   `let i = 0; while i < xs.len() { if xs[i] == t { return i } i += 1 }`
    ///   `return -1`  ->  `return xs.index_of(t)`
    fn linsearch_window(win: &[Stmt]) -> Option<(usize, Vec<Stmt>)> {
        if win.len() < 3 { return None; }
        let i_name = match &win[0] {
            Stmt::Let(n, Expr::Int(0)) | Stmt::Assign(n, Expr::Int(0)) => n.clone(),
            _ => return None,
        };
        let (xs, t) = match &win[1] {
            Stmt::While(cond, body) => {
                let xs = match cond {
                    Expr::BinOp(l, BinOp::Lt, r) => {
                        if !matches!(l.as_ref(), Expr::Var(v) if *v == i_name) { return None; }
                        match r.as_ref() {
                            Expr::MethodCall(b, m, a) if m == "len" && a.is_empty() =>
                                (**b).clone(),
                            _ => return None,
                        }
                    }
                    _ => return None,
                };
                // body must be [If(xs[i] == t, [return i]), i += 1] (either order)
                if body.len() != 2 { return None; }
                let is_incr = |s: &Stmt| matches!(s,
                    Stmt::Assign(t2, Expr::BinOp(l2, BinOp::Add, r2))
                        if *t2 == i_name
                        && matches!(l2.as_ref(), Expr::Var(v) if *v == i_name)
                        && matches!(r2.as_ref(), Expr::Int(1)));
                let if_s = if is_incr(&body[0]) { &body[1] }
                    else if is_incr(&body[1]) { &body[0] }
                    else { return None; };
                let t = match if_s {
                    Stmt::If(c, then_b, _) if then_b.len() == 1
                        && matches!(&then_b[0], Stmt::Return(Some(r))
                            if matches!(r, Expr::Var(v) if *v == i_name)) =>
                    {
                        match c {
                            Expr::BinOp(l, BinOp::Eq, r) => {
                                let elem = |e: &Expr| matches!(e,
                                    Expr::Index(b, i2)
                                        if b.as_ref() == &xs
                                        && matches!(i2.as_ref(), Expr::Var(v) if *v == i_name));
                                if elem(l) { (**r).clone() }
                                else if elem(r) { (**l).clone() }
                                else { return None; }
                            }
                            _ => return None,
                        }
                    }
                    _ => return None,
                };
                (xs, t)
            }
            _ => return None,
        };
        // tail: return -1
        if !matches!(&win[2], Stmt::Return(Some(e))
            if matches!(e, Expr::Int(-1))
               || matches!(e, Expr::UnaryOp(UnaryOp::Neg, i)
                    if matches!(i.as_ref(), Expr::Int(1)))) {
            return None;
        }
        let set: std::collections::HashSet<String> =
            [i_name.clone()].into_iter().collect();
        if expr_uses(&t, &set) { return None; }
        Some((3, vec![Stmt::Return(Some(Expr::MethodCall(
            Box::new(xs), "index_of".into(), vec![t])))]))
    }

    /// O(n) accumulate loop -> O(1) closed form:
    ///   `let t = init; for x in ITER { t = t + k }`
    ///     -> `let t = init + k * len(ITER)`   (k loop-invariant)
    ///   `let i = 0; let t = init; while i < n { t += k; i += 1 }`
    ///     -> `let t = init + k * n; let i = n`
    fn accum_window(win: &[Stmt]) -> Option<(usize, Vec<Stmt>)> {
        // `t = t + k` / `t = t - k` / `t = k + t` -> (t, op, k)
        fn accum_parts(s: &Stmt) -> Option<(String, BinOp, Expr)> {
            if let Stmt::Assign(t, Expr::BinOp(l, op, r)) = s {
                if !matches!(op, BinOp::Add | BinOp::Sub) { return None; }
                if matches!(l.as_ref(), Expr::Var(v) if v == t) {
                    return Some((t.clone(), op.clone(), (**r).clone()));
                }
                if *op == BinOp::Add && matches!(r.as_ref(), Expr::Var(v) if v == t) {
                    return Some((t.clone(), BinOp::Add, (**l).clone()));
                }
            }
            None
        }
        let uses = |e: &Expr, names: &[&str]| {
            let set: std::collections::HashSet<String> =
                names.iter().map(|n| n.to_string()).collect();
            expr_uses(e, &set)
        };
        let mul = |a: Expr, b: Expr| Expr::BinOp(Box::new(a), BinOp::Mul, Box::new(b));
        let count_of = |iter: &Expr| match iter {
            Expr::Call(n, _, a) if n == "range" && a.len() == 1 => a[0].clone(),
            other => Expr::MethodCall(Box::new(other.clone()), "len".into(), vec![]),
        };
        // for-form: [Let/Assign(t, init), For(x, iter, [t = t +- k])]
        if win.len() >= 2 {
            let init_bind = match &win[0] {
                Stmt::Let(t, e) | Stmt::Assign(t, e) => Some((t.clone(), e.clone())),
                _ => None,
            };
            if let (Some((t, init)), Stmt::For(x, iter, body)) = (init_bind, &win[1]) {
                if body.len() == 1 {
                    if let Some((t2, op, k)) = accum_parts(&body[0]) {
                        if t2 == t {
                            // t = t op x (the loop var itself) -> init op ITER.sum()
                            if matches!(&k, Expr::Var(v) if v == x) {
                                let summed = Expr::MethodCall(
                                    Box::new(iter.clone()), "sum".into(), vec![]);
                                let closed = Expr::BinOp(Box::new(init), op, Box::new(summed));
                                return Some((2, vec![Stmt::Let(t, closed)]));
                            }
                            if !uses(&k, &[&t, x]) {
                                let closed = Expr::BinOp(
                                    Box::new(init), op,
                                    Box::new(mul(k, count_of(iter))));
                                return Some((2, vec![Stmt::Let(t, closed)]));
                            }
                        }
                    }
                }
            }
        }
        // while-form: two lets (i=0, t=init either order) + while
        if win.len() >= 3 {
            let bind = |s: &Stmt| -> Option<(String, Expr)> {
                match s { Stmt::Let(t, e) | Stmt::Assign(t, e) => Some((t.clone(), e.clone())), _ => None }
            };
            if let (Some((v1, e1)), Some((v2, e2))) = (bind(&win[0]), bind(&win[1])) {
                let (i_name, t, init) = if matches!(e1, Expr::Int(0)) {
                    (v1, v2, e2)
                } else if matches!(e2, Expr::Int(0)) {
                    (v2, v1, e1)
                } else { return None; };
                if let Stmt::While(cond, body) = &win[2] {
                    if let Expr::BinOp(l, BinOp::Lt, n) = cond {
                        if matches!(l.as_ref(), Expr::Var(v) if *v == i_name)
                            && !uses(n, &[&i_name, &t]) && body.len() == 2 {
                            let is_incr = |s: &Stmt| matches!(s,
                                Stmt::Assign(t2, Expr::BinOp(l2, BinOp::Add, r2))
                                    if *t2 == i_name
                                    && matches!(l2.as_ref(), Expr::Var(v) if *v == i_name)
                                    && matches!(r2.as_ref(), Expr::Int(1)));
                            let (acc_s, inc_ok) = if is_incr(&body[0]) {
                                (Some(&body[1]), true)
                            } else if is_incr(&body[1]) {
                                (Some(&body[0]), true)
                            } else { (None, false) };
                            if inc_ok {
                                if let Some((t2, op, k)) = acc_s.and_then(accum_parts) {
                                    if t2 == t && !uses(&k, &[&i_name, &t]) {
                                        let closed = Expr::BinOp(
                                            Box::new(init), op,
                                            Box::new(mul(k, (**n).clone())));
                                        return Some((3, vec![
                                            Stmt::Let(t, closed),
                                            Stmt::Let(i_name, (**n).clone()),
                                        ]));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }


    /// O(n) scan binding `tgt` to the extremum of `xs` (arbitrary expr —
    /// non-Var receivers hoist into a `let __xs` first).
    fn scan_binding(xs: &Expr, tgt: &str, is_max: bool) -> Vec<Stmt> {
        let (mut pre, xs_expr) = match xs {
            Expr::Var(n) => (Vec::new(), Expr::Var(n.clone())),
            other => (vec![Stmt::Let("__xs".into(), other.clone())],
                      Expr::Var("__xs".into())),
        };
        pre.extend(Self::scan_stmts(&xs_expr, tgt, is_max));
        pre
    }

    /// Canonical O(n) extremum scan — a `for` over elements (no index
    /// var, no while): `let m = xs[0]; for __v in xs { if __v > m { m = __v } }`
    fn make_linear_extremum_scan(xs: &str, is_max: bool) -> Vec<Stmt> {
        let mut out = Self::scan_stmts(&Expr::Var(xs.into()), "m", is_max);
        out.push(Stmt::Return(Some(Expr::Var("m".into()))));
        out
    }

    /// `let t = xs[0]; for __v in xs { if __v <cmp> t { t = __v } }`
    fn scan_stmts(xs: &Expr, tgt: &str, is_max: bool) -> Vec<Stmt> {
        let cmp = if is_max { BinOp::Gt } else { BinOp::Lt };
        vec![
            Stmt::Let(tgt.into(), Expr::Index(
                Box::new(xs.clone()), Box::new(Expr::Int(0)))),
            Stmt::For("__v".into(), xs.clone(), vec![
                Stmt::If(
                    Expr::BinOp(
                        Box::new(Expr::Var("__v".into())), cmp,
                        Box::new(Expr::Var(tgt.into()))),
                    vec![Stmt::Assign(tgt.into(), Expr::Var("__v".into()))],
                    None),
            ]),
        ]
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

    /// True if hoisting `let x = e` out of a loop preserves behavior —
    /// i.e. `e` is side-effect-free. Calls are allowed only to fns proven
    /// pure (impure_fns is the complement set) or to builtins that
    /// aren't in the side-effecting denylist.
    fn expr_hoistable(&self, e: &Expr) -> bool {
        match e {
            Expr::Call(n, _, args) => {
                !Self::impure_builtin(n)
                    && !self.impure_fns.contains(n)
                    && args.iter().all(|a| self.expr_hoistable(a))
            }
            Expr::MethodCall(o, m, args) => {
                !Self::impure_method(m)
                    && self.expr_hoistable(o)
                    && args.iter().all(|a| self.expr_hoistable(a))
            }
            Expr::SafeGet(o, _, args) => self.expr_hoistable(o)
                && args.as_ref()
                    .map(|a| a.iter().all(|x| self.expr_hoistable(x)))
                    .unwrap_or(true),
            Expr::BinOp(a, _, b) | Expr::Index(a, b) =>
                self.expr_hoistable(a) && self.expr_hoistable(b),
            Expr::UnaryOp(_, a) | Expr::FieldAccess(a, _) => self.expr_hoistable(a),
            Expr::List(items) | Expr::Interp(items) =>
                items.iter().all(|i| self.expr_hoistable(i)),
            Expr::StructLit(_, fs) => fs.iter().all(|(_, v)| self.expr_hoistable(v)),
            Expr::Slice(o, lo, hi) => self.expr_hoistable(o)
                && lo.as_ref().map(|e| self.expr_hoistable(e)).unwrap_or(true)
                && hi.as_ref().map(|e| self.expr_hoistable(e)).unwrap_or(true),
            Expr::ListComp(x, _, it, c) => self.expr_hoistable(x)
                && self.expr_hoistable(it)
                && c.as_ref().map(|e| self.expr_hoistable(e)).unwrap_or(true),
            Expr::Lambda(_, _) => true, // creating a closure is pure — its
            // effects fire where it's called, same either way
            _ => true,
        }
    }

    /// Side-effecting builtins — a `let` holding one can't hoist.
    fn impure_builtin(n: &str) -> bool {
        matches!(n,
            "print" | "eprint" | "rand" | "randint" | "randf" | "input"
            | "readline" | "now" | "time" | "sleep" | "exit" | "quit"
            | "read_file" | "write_file" | "append_file" | "read_lines"
            | "list_dir" | "exec" | "shell" | "http_get" | "http_post"
            | "http_put" | "http_delete" | "fetch" | "open" | "connect"
            | "query" | "save" | "load" | "absorb" | "spawn" | "join_task"
            | "env" | "setenv" | "gc" | "log" | "debug")
    }

    /// Mutating methods — `obj.m()` mutates the receiver, not pure.
    fn impure_method(m: &str) -> bool {
        matches!(m, "append" | "insert" | "remove" | "pop" | "clear"
            | "push" | "extend" | "update" | "delete" | "fill" | "sort_in_place")
    }

    /// Body scan for the fixpoint — a stmt list is pure iff nothing
    /// side-effects: no Print, no IndexAssign/FieldAssign (mutates a
    /// possibly-shared structure), no Grow/Import (environment effects),
    /// and every call target is a pure fn or a non-denylisted builtin.
    fn stmt_pure(s: &Stmt, pure: &std::collections::HashSet<String>,
                 fns: &std::collections::HashMap<String, Vec<FunctionEntry>>) -> bool {
        let ep = |e: &Expr| Self::expr_pure_in(e, pure, fns);
        match s {
            Stmt::Print(_) | Stmt::Grow(_) | Stmt::Import(..)
            | Stmt::IndexAssign(..) | Stmt::FieldAssign(..) => false,
            Stmt::Let(_, e) | Stmt::Assign(_, e) | Stmt::ExprStmt(e)
            | Stmt::LetDestructure(_, e) => ep(e),
            Stmt::Return(Some(e)) => ep(e),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue
            | Stmt::StructDef(..) | Stmt::FnDef(..) => true,
            Stmt::If(c, a, b) => ep(c)
                && a.iter().all(|s| Self::stmt_pure(s, pure, fns))
                && b.as_ref()
                    .map(|v| v.iter().all(|s| Self::stmt_pure(s, pure, fns)))
                    .unwrap_or(true),
            Stmt::While(c, b) => ep(c)
                && b.iter().all(|s| Self::stmt_pure(s, pure, fns)),
            Stmt::For(_, it, b) => ep(it)
                && b.iter().all(|s| Self::stmt_pure(s, pure, fns)),
            Stmt::Try(b, _, h) =>
                b.iter().chain(h.iter()).all(|s| Self::stmt_pure(s, pure, fns)),
            Stmt::Match(e, arms, els) => ep(e)
                && arms.iter().all(|(p, b)|
                    ep(p) && b.iter().all(|s| Self::stmt_pure(s, pure, fns)))
                && els.as_ref()
                    .map(|v| v.iter().all(|s| Self::stmt_pure(s, pure, fns)))
                    .unwrap_or(true),
        }
    }

    fn expr_pure_in(e: &Expr, pure: &std::collections::HashSet<String>,
                    fns: &std::collections::HashMap<String, Vec<FunctionEntry>>) -> bool {
        match e {
            Expr::Call(n, _, args) => {
                if Self::impure_builtin(n) { return false; }
                // a user fn that isn't (yet) proven pure is impure
                if fns.contains_key(n) && !pure.contains(n) { return false; }
                args.iter().all(|a| Self::expr_pure_in(a, pure, fns))
            }
            Expr::MethodCall(o, m, args) => {
                !Self::impure_method(m)
                    && Self::expr_pure_in(o, pure, fns)
                    && args.iter().all(|a| Self::expr_pure_in(a, pure, fns))
            }
            Expr::SafeGet(o, _, args) => Self::expr_pure_in(o, pure, fns)
                && args.as_ref()
                    .map(|a| a.iter().all(|x| Self::expr_pure_in(x, pure, fns)))
                    .unwrap_or(true),
            Expr::BinOp(a, _, b) | Expr::Index(a, b) =>
                Self::expr_pure_in(a, pure, fns) && Self::expr_pure_in(b, pure, fns),
            Expr::UnaryOp(_, a) | Expr::FieldAccess(a, _) =>
                Self::expr_pure_in(a, pure, fns),
            Expr::List(items) | Expr::Interp(items) =>
                items.iter().all(|i| Self::expr_pure_in(i, pure, fns)),
            Expr::StructLit(_, fs) =>
                fs.iter().all(|(_, v)| Self::expr_pure_in(v, pure, fns)),
            Expr::Slice(o, lo, hi) => Self::expr_pure_in(o, pure, fns)
                && lo.as_ref().map(|e| Self::expr_pure_in(e, pure, fns)).unwrap_or(true)
                && hi.as_ref().map(|e| Self::expr_pure_in(e, pure, fns)).unwrap_or(true),
            Expr::ListComp(x, _, it, c) => Self::expr_pure_in(x, pure, fns)
                && Self::expr_pure_in(it, pure, fns)
                && c.as_ref().map(|e| Self::expr_pure_in(e, pure, fns)).unwrap_or(true),
            Expr::Lambda(_, body) =>
                body.iter().all(|s| Self::stmt_pure(s, pure, fns)),
            _ => true,
        }
    }

    /// Fixpoint: which families' FITTEST variant is side-effecting?
    /// A fn is pure iff its body scans clean AND every user-fn call it
    /// makes is already proven pure. Everything else lands in the set.
    fn impure_families(fns: &std::collections::HashMap<String, Vec<FunctionEntry>>)
        -> std::collections::HashSet<String> {
        let mut pure: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        loop {
            let mut changed = false;
            for (name, variants) in fns {
                if pure.contains(name) { continue; }
                let fittest = variants.iter()
                    .max_by(|a, b| a.fitness.partial_cmp(&b.fitness)
                        .unwrap_or(std::cmp::Ordering::Equal));
                if let Some(v) = fittest {
                    if v.body.iter().all(|s| Self::stmt_pure(s, &pure, fns)) {
                        pure.insert(name.clone());
                        changed = true;
                    }
                } else {
                    // empty family — can't call it anyway; mark pure
                    // so it doesn't poison the set
                    pure.insert(name.clone());
                }
            }
            if !changed { break; }
        }
        fns.keys().filter(|n| !pure.contains(*n)).cloned().collect()
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
        let replacement: Option<Expr> = if let Expr::MethodCall(inner, m, a) = e {
            // x.reverse().reverse() -> x  (O(n) double-reversal elimination)
            if m == "reverse" && a.is_empty() {
                if let Expr::MethodCall(x, m2, a2) = inner.as_ref() {
                    if m2 == "reverse" && a2.is_empty() { Some((**x).clone()) } else { None }
                } else { None }
            } else { None }
        } else if let Expr::BinOp(l, op, r) = e {
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
            // + → - or * — geometric steps are how loop mutations
            // reach asymptotic improvement (i += k → i *= k is
            // O(n) → O(log n) iteration count)
            BinOp::Add => if self.rand_bool() { BinOp::Sub } else { BinOp::Mul },
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

#[derive(Debug, Clone, serde::Serialize)]
pub enum EvolutionEvent {
    CycleStart(usize),
    NoCandidates,
    ProbeGenerated { name: String, niche: String },
    CloneRejected { name: String },
    LintRejected { name: String, warnings: Vec<String> },
    Tended { healed: usize, repaired: usize },
    Mutate { name: String, parent_id: u32, new_id: u32, desc: String },
    Trial { name: String, id: u32, healthy: bool, score: f64, fitness: f64, parent_fitness: f64 },
    Death { name: String, id: u32, cause: String },
    CycleEnd { cycle: usize, mutations: usize, births: usize, deaths: usize, delta: f64 },
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
            Self::LintRejected { name, warnings } => {
                format!("  [LINT]   {} — child introduces real-bug warnings, not born ({})",
                        name, warnings.join("; "))
            }
            Self::Tended { healed, repaired } => {
                format!("  [TEND]   garden tended — {} healed, {} dead-code repairs",
                        healed, repaired)
            }
            Self::Mutate { name, parent_id, new_id, desc } => {
                format!("  [MUTATE] {}#{} → {}#{} ({})", name, parent_id, name, new_id, desc)
            }
            Self::Trial { name, id, healthy, score, fitness, parent_fitness } => {
                if *healthy {
                    format!("  [TRIAL]  {}#{} — healthy (score: {:.2}, fitness: {:.2}, Δ {:+.2})",
                        name, id, score, fitness, fitness - parent_fitness)
                } else {
                    format!("  [TRIAL]  {}#{} — sick (failed trial)", name, id)
                }
            }
            Self::Death { name, id, cause } => {
                format!("  [CULL]   {}#{} died — {}", name, id, cause)
            }
            Self::CycleEnd { cycle, mutations, births, deaths, delta } => {
                format!("[EVOLVE] Cycle {} complete: {} mutations, {} births, {} deaths  Δ {:+.2}",
                    cycle, mutations, births, deaths, delta)
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
    IdiomRewrite,
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
        Type::Any => match i % 6 {
            0 => boundary_value(&Type::Int, i),
            1 => boundary_value(&Type::Str, i),
            2 => boundary_value(&Type::List(Box::new(Type::Int)), i),
            3 => Some(Value::Dict(std::collections::HashMap::new().into_iter().collect())),
            4 => boundary_value(&Type::Bool, i),
            _ => Some(Value::Null),
        },
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
/// Rename free occurrences of `from` to `to` inside an expression
/// (stops at fn/lambda/match-arm boundaries — captured scopes keep
/// their own name).
fn subst_var_expr(e: &Expr, from: &str, to: &str) -> Expr {
    match e {
        Expr::Var(n) => if n == from { Expr::Var(to.into()) } else { e.clone() },
        Expr::BinOp(l, op, r) => Expr::BinOp(
            Box::new(subst_var_expr(l, from, to)), op.clone(),
            Box::new(subst_var_expr(r, from, to))),
        Expr::UnaryOp(op, x) => Expr::UnaryOp(
            op.clone(), Box::new(subst_var_expr(x, from, to))),
        Expr::Index(o, i) => Expr::Index(
            Box::new(subst_var_expr(o, from, to)),
            Box::new(subst_var_expr(i, from, to))),
        Expr::Slice(o, lo, hi) => Expr::Slice(
            Box::new(subst_var_expr(o, from, to)),
            lo.as_ref().map(|x| Box::new(subst_var_expr(x, from, to))),
            hi.as_ref().map(|x| Box::new(subst_var_expr(x, from, to)))),
        Expr::Call(f, v, a) => Expr::Call(f.clone(), *v,
            a.iter().map(|x| subst_var_expr(x, from, to)).collect()),
        Expr::MethodCall(o, m, a) => Expr::MethodCall(
            Box::new(subst_var_expr(o, from, to)), m.clone(),
            a.iter().map(|x| subst_var_expr(x, from, to)).collect()),
        Expr::List(xs) => Expr::List(
            xs.iter().map(|x| subst_var_expr(x, from, to)).collect()),
        Expr::Interp(xs) => Expr::Interp(
            xs.iter().map(|x| subst_var_expr(x, from, to)).collect()),
        Expr::FieldAccess(o, f) => Expr::FieldAccess(
            Box::new(subst_var_expr(o, from, to)), f.clone()),
        Expr::StructLit(n, f) => Expr::StructLit(n.clone(),
            f.iter().map(|(k, v)| (k.clone(), subst_var_expr(v, from, to))).collect()),
        Expr::SafeGet(o, n, a) => Expr::SafeGet(
            Box::new(subst_var_expr(o, from, to)), n.clone(),
            a.as_ref().map(|xs| xs.iter().map(|y| subst_var_expr(y, from, to)).collect())),
        Expr::Lambda(p, b) => {
            if p.iter().any(|x| x.name == from) { e.clone() } // captured name
            else { Expr::Lambda(p.clone(),
                b.iter().map(|st| subst_var_stmt(st, from, to)).collect()) }
        }
        Expr::ListComp(h, v, i, c) => {
            let (i2, c2) = (subst_var_expr(i, from, to),
                c.as_ref().map(|x| Box::new(subst_var_expr(x, from, to))));
            let h2 = if v == from { h.clone() } else { Box::new(subst_var_expr(h, from, to)) };
            Expr::ListComp(h2, v.clone(), Box::new(i2), c2)
        }
        _ => e.clone(),
    }
}

fn subst_var_stmt(s: &Stmt, from: &str, to: &str) -> Stmt {
    let sub = |e: &Expr| subst_var_expr(e, from, to);
    let subs = |xs: &Vec<Stmt>| xs.iter().map(|x| subst_var_stmt(x, from, to)).collect();
    match s {
        Stmt::Let(n, e) => Stmt::Let(n.clone(), sub(e)),
        Stmt::Assign(n, e) => Stmt::Assign(n.clone(), sub(e)),
        Stmt::Print(e) => Stmt::Print(sub(e)),
        Stmt::ExprStmt(e) => Stmt::ExprStmt(sub(e)),
        Stmt::Return(e) => Stmt::Return(e.as_ref().map(|x| sub(x))),
        Stmt::If(c, t, b) => Stmt::If(sub(c), subs(t),
            b.as_ref().map(|x| subs(x))),
        Stmt::While(c, b) => Stmt::While(sub(c), subs(b)),
        Stmt::For(v, i, b) => {
            if v == from { s.clone() } // shadowed inside the loop
            else { Stmt::For(v.clone(), sub(i), subs(b)) }
        }
        Stmt::Try(t, v, c) => {
            let c2 = if v == from { c.clone() } else { subs(c) };
            Stmt::Try(subs(t), v.clone(), c2)
        }
        Stmt::Match(e, arms, els) => Stmt::Match(sub(e),
            arms.iter().map(|(p, b)| (p.clone(), subs(b))).collect(),
            els.as_ref().map(|x| subs(x))),
        Stmt::FnDef(n, p, b) => {
            if p.iter().any(|x| x.name == from) { s.clone() }
            else { Stmt::FnDef(n.clone(), p.clone(), subs(b)) }
        }
        _ => s.clone(),
    }
}

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

/// `return var[0|last]` -> (target, is_return, is_last); same for `let x =`.
fn tail_uses(stmt: &Stmt, var: &str) -> Option<(String, bool, bool)> {
    let var_idx_kind = |e: &Expr| -> Option<bool> {
        if let Expr::Index(recv, idx) = e {
            if !matches!(recv.as_ref(), Expr::Var(v) if v == var) { return None; }
            match idx.as_ref() {
                Expr::Int(0) => Some(false),
                Expr::Int(-1) => Some(true),
                Expr::UnaryOp(UnaryOp::Neg, i)
                    if matches!(i.as_ref(), Expr::Int(1)) => Some(true),
                // s[s.len() - 1]
                Expr::BinOp(l, BinOp::Sub, r) if matches!(r.as_ref(), Expr::Int(1)) => {
                    if matches!(l.as_ref(), Expr::MethodCall(b, m, a)
                        if m == "len" && a.is_empty()
                           && matches!(b.as_ref(), Expr::Var(v) if v == var)) {
                        Some(true)
                    } else { None }
                }
                _ => None,
            }
        } else { None }
    };
    match stmt {
        Stmt::Return(Some(e)) => var_idx_kind(e).map(|l| ("m".into(), true, l)),
        Stmt::Let(name, e) => var_idx_kind(e).map(|l| (name.clone(), false, l)),
        _ => None,
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
        Mutation::ConstProp => "constant propagation",
        Mutation::EarlyExit => "early-exit fusion",
        Mutation::IdiomRewrite => "algorithmic idiom rewrite",
    }
}

impl Evolver {
    /// The --tend pass: heal every sick variant (healer-merge +
    /// mechanical repair first), then run the dead-code repairs on
    /// all bodies. Returns (healed, repaired) counts.
    fn tend_garden(&self, veldt: &mut Veldt) -> (usize, usize) {
        let snapshot: Vec<(String, u32, bool)> = veldt.functions.iter()
            .flat_map(|(n, vs)| vs.iter()
                .map(|e| (n.clone(), e.id, matches!(e.health, Health::Sick(_)))))
            .collect();
        let (mut healed, mut repaired) = (0usize, 0usize);
        for (name, id, was_sick) in snapshot {
            let mut entry = veldt.functions[&name].iter()
                .find(|e| e.id == id).cloned().unwrap();
            let mut changed = false;
            if was_sick && HealthChecker::heal(&mut entry, veldt) {
                healed += 1;
                changed = true;
            }
            let mut c = HealthChecker::repair_dead_tail(&mut entry.body);
            c |= HealthChecker::repair_dead_lets(&mut entry.body);
            if c { repaired += 1; changed = true; }
            let idiomed = Self::idiom_rewrite_body(&entry.body);
            if idiomed != entry.body {
                // --exact: only apply rewrites Z3 proves semantics-
                // preserving (same gate as `veldt rewrite --exact`).
                let proven = if std::env::args().any(|a| a == "--exact") {
                    #[cfg(feature = "smt")]
                    {
                        let params: Vec<(String, crate::ast::Type)> = entry.params.iter()
                            .map(|p| (p.name.clone(), p.typ.clone())).collect();
                        matches!(crate::smt::verify_bodies_exact(
                            &entry.body, &idiomed, &params), Ok(None))
                    }
                    #[cfg(not(feature = "smt"))]
                    { false }
                } else { true };
                if proven {
                    entry.body = idiomed;
                    // re-trial so fitness/efficiency reflect the new body
                    HealthChecker::check_at_birth_with(&mut entry, veldt, self.use_vm);
                    repaired += 1; changed = true;
                }
            }
            if changed {
                if let Some(vs) = veldt.functions.get_mut(&name) {
                    for e in vs.iter_mut() {
                        if e.id == id { *e = entry.clone(); break; }
                    }
                }
            }
        }
        (healed, repaired)
    }
}

/// strict-lint a candidate mutant body — returns only the
/// real-bug warnings (defects, not style). The body wraps as a
/// synthetic grow fn so params seed scope and tests get checked.
fn mutant_realbugs(name: &str, params: &[Param], body: &[Stmt],
                   tests: &[TestAssert]) -> Vec<String> {
    let wrapped = Stmt::Grow(GrowItem::Fn(
        name.to_string(), params.to_vec(), body.to_vec(), tests.to_vec()));
    crate::lint(&[wrapped]).into_iter()
        .filter(|w| is_realbug_warning(w))
        .collect()
}

/// the defect subset of strict-lint warnings — things a mutant
/// almost certainly shouldn't carry: arity mismatches, unbound
/// reads/assigns, null binds, fn shadowing, wrong-fn tests,
/// unreachable code, never-matching patterns
pub(crate) fn is_realbug_warning(w: &str) -> bool {
    w.contains("args, got")
        || w.contains("shadows fn")
        || w.contains("never returns a value")
        || w.contains("before it's bound")
        || w.contains("doesn't exercise")
        || w.contains("unreachable code after return")
        || w.contains("assigned but never declared")
        || w.contains("is unbound — never matches")
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
        HealthChecker::check_at_birth_with(&mut entry, &veldt, false);
        veldt.functions.get_mut("add").unwrap()[0] = entry;

        let initial_count = veldt.functions["add"].len();

        let mut evolver = Evolver::new();
        let events = evolver.evolve(&mut veldt, 1);

        // A Mutate event is the deterministic signal — survival is
        // stochastic now that trials test each variant's OWN body
        // (broken mutants die instead of riding the incumbent's
        // answers). 3 cycles gives enough attempts for near-certain
        // coverage of the mutation paths.
        assert!(!events.is_empty());
        assert!(events.iter().any(|e|
            matches!(e, EvolutionEvent::Mutate { .. })),
            "evolution produced no mutation events: {:?}", events);
        let _ = initial_count; // births are stochastic — not asserted
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
    fn hoist_skips_impure_lets() {
        // `let c = print(x)` inside a loop — hoisting would fire the
        // side effect once instead of per-iteration. Must not hoist.
        let body = vec![
            Stmt::While(Expr::BinOp(
                    Box::new(Expr::Var("i".into())), BinOp::Lt, Box::new(Expr::Var("n".into()))),
                vec![
                    Stmt::Let("c".into(), Expr::Call("print".into(), None,
                        vec![Expr::Var("x".into())])),
                    Stmt::Assign("i".into(), Expr::Int(1)),
                ]),
        ];
        let mut e = ev();
        let out = e.mutate_body(&body, Mutation::HoistInvariant);
        assert!(matches!(&out[0], Stmt::While(_, _)),
            "impure let was hoisted: {:?}", out);
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

#[cfg(test)]
mod asymptotic_proof {
    use super::*;

    /// THE headline proof: a loop-invariant hoist turns an O(n·cost)
    /// body into O(cost+n) — same output, measurably fewer steps.
    /// This is the asymptotic-improvement claim, demonstrated by the
    /// mutation itself, not simulated.
    #[test]
    fn invariant_hoist_yields_asymptotic_speedup() {
        use crate::interpreter::{Interpreter, FunctionDef, FunctionVariant, Value};

        // fn f(n, vals):
        //   let s = 0; let i = 0
        //   while i < n { let c = costly(vals); s = s + c; i = i + 1 }
        //   return s
        //   — costly() does an inner O(len) loop, so the seed is
        //   O(n·len); the hoist runs it once → O(len + n).
        let loop_body = vec![
            Stmt::Let("c".into(), Expr::Call("costly".into(), None,
                vec![Expr::Var("vals".into())])),
            Stmt::Assign("s".into(), Expr::BinOp(
                Box::new(Expr::Var("s".into())), BinOp::Add,
                Box::new(Expr::Var("c".into())))),
            Stmt::Assign("i".into(), Expr::BinOp(
                Box::new(Expr::Var("i".into())), BinOp::Add,
                Box::new(Expr::Int(1)))),
        ];
        let while_s = Stmt::While(Expr::BinOp(
            Box::new(Expr::Var("i".into())), BinOp::Lt,
            Box::new(Expr::Var("n".into()))), loop_body);
        let seed_body = vec![
            Stmt::Let("s".into(), Expr::Int(0)),
            Stmt::Let("i".into(), Expr::Int(0)),
            while_s,
            Stmt::Return(Some(Expr::Var("s".into()))),
        ];

        let mut e = Evolver::new();
        let hoisted = e.mutate_body(&seed_body, Mutation::HoistInvariant);
        // the `let c = costly(vals)` should be hoisted before the while
        let c_pos = hoisted.iter().position(|s| matches!(s, Stmt::Let(n, _) if n == "c"));
        let w_pos = hoisted.iter().position(|s| matches!(s, Stmt::While(_, _)));
        assert!(matches!((c_pos, w_pos), (Some(c), Some(w)) if c < w),
            "invariant let not hoisted: {:?}", hoisted);

        // costly(xs): a user fn whose inner loop counts real steps —
        // an O(len) call, not a ~free builtin
        let costly_body = vec![
            Stmt::Let("acc".into(), Expr::Int(0)),
            Stmt::For("x".into(), Expr::Var("xs".into()), vec![
                Stmt::Assign("acc".into(), Expr::BinOp(
                    Box::new(Expr::Var("acc".into())), BinOp::Add,
                    Box::new(Expr::Var("x".into())))),
            ]),
            Stmt::Return(Some(Expr::Var("acc".into()))),
        ];

        // run both on a big input — same output, but the hoisted one
        // runs costly() once, not n times
        let run = |body: &[Stmt]| -> (u64, Value) {
            let mut interp = Interpreter::with_step_limit(50_000_000);
            let params = vec![
                Param { name: "n".into(), typ: Type::Int },
                Param { name: "vals".into(), typ: Type::List(Box::new(Type::Int)) },
            ];
            interp.functions.insert("f".to_string(), vec![FunctionVariant {
                id: 1, fitness: 1.0,
                def: FunctionDef { params: std::rc::Rc::new(params),
                                   body: std::rc::Rc::new(body.to_vec()) },
            }]);
            interp.functions.insert("costly".to_string(), vec![FunctionVariant {
                id: 1, fitness: 1.0,
                def: FunctionDef {
                    params: std::rc::Rc::new(vec![
                        Param { name: "xs".into(),
                                typ: Type::List(Box::new(Type::Int)) }]),
                    body: std::rc::Rc::new(costly_body.clone()) },
            }]);
            let vals: Vec<Value> = (0..300).map(Value::Int).collect();
            let before = interp.step_count;
            let out = interp.call_function("f", Some(1),
                vec![Value::Int(300), Value::List(vals.into_iter().collect())]).unwrap();
            (interp.step_count - before, out)
        };
        let (seed_steps, seed_out) = run(&seed_body);
        let (hoist_steps, hoist_out) = run(&hoisted);
        assert_eq!(seed_out, hoist_out,
            "hoist changed the result — semantics broken");
        assert!(hoist_steps < seed_steps,
            "no speedup: seed {} vs hoisted {}", seed_steps, hoist_steps);
        // 300 iterations of costly(300) vs one — the hoisted variant is
        // asymptotically fewer steps, not a constant-factor shave
        assert!(seed_steps as f64 / hoist_steps.max(1) as f64 > 3.0,
            "not asymptotic: seed {} vs hoisted {} ({}x)",
            seed_steps, hoist_steps,
            seed_steps as f64 / hoist_steps.max(1) as f64);
    }

    #[test]
    fn evolve_discovers_and_selects_linear_scan_over_sort_reverse() {
        use crate::lexer::Lexer;
        use crate::parser::Parser;
        use crate::health::HealthChecker;
        use crate::interpreter::Value;

        let mut veldt = Veldt::new();
        let src = "grow fn max_val(xs: list<int>) test max_val([3, 1, 2]) == 3 {\n let s = xs.sort()\n let r = s.reverse()\n return r[0]\n}";
        let tokens = Lexer::new(src).tokenize().unwrap();
        let stmts = Parser::new(tokens).parse_program().unwrap();
        for s in stmts {
            if let Stmt::Grow(crate::ast::GrowItem::Fn(name, params, body, tests)) = s {
                veldt.grow_function(&name, params, body, tests, src);
            }
        }
        // Plant scaled list probe (scale=64) so measure_efficiency compares O(n) vs O(n log n)
        let big_list = Value::List((0..64).map(|i| Value::Int((i * 37) % 100)).collect());
        let max_expected = Value::Int(99);
        veldt.probes.insert("max_val".into(), vec![crate::veldt::Probe {
            args: vec![serde_json::to_string(&big_list).unwrap()],
            expected: serde_json::to_string(&max_expected).unwrap(),
            niche: "list:big".into(),
            born: 0,
            scale: 64,
        }]);
        let mut seed_entry = veldt.functions["max_val"][0].clone();
        HealthChecker::check_at_birth_with(&mut seed_entry, &veldt, false);
        veldt.functions.get_mut("max_val").unwrap()[0] = seed_entry.clone();

        let mut evolver = Evolver::new();
        evolver.evolve(&mut veldt, 1);

        let fittest = veldt.get_fittest_variant("max_val").unwrap();
        assert_ne!(fittest.id, seed_entry.id,
            "expected evolved O(n) scan to beat the O(n log n) sort.reverse[0] seed");
        assert!(fittest.efficiency > 2.0,
            "expected evolved O(n) scan efficiency > 2.0x over sort.reverse[0], got {:.2}x",
            fittest.efficiency);
    }

    fn parse_fn_body(src: &str) -> Vec<Stmt> {
        use crate::lexer::Lexer;
        use crate::parser::Parser;
        let tokens = Lexer::new(src).tokenize().unwrap();
        Parser::new(tokens).parse_program().unwrap()
    }

    #[test]
    fn idiom_rewrite_fires_on_let_binding_and_nested_bodies() {
        let mut evolver = Evolver::new();
        // let-binding form + surrounding prefix code — not a whole-body shape
        let body = parse_fn_body(
            "let n = xs.len()\nlet best = xs.sort().reverse()[0]\nreturn best");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("for"), "expected a scan loop, got:\n{}", src);
        assert!(!src.contains(".sort()"), "sort survived:\n{}", src);
        // nested inside an if arm
        let body = parse_fn_body(
            "if xs.len() > 0 {\n  return xs.sort()[0]\n}\nreturn 0");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("for") && !src.contains(".sort()"),
            "nested idiom not rewritten:\n{}", src);
    }

    #[test]
    fn idiom_rewrite_closes_accumulate_loops() {
        let mut evolver = Evolver::new();
        // for-accumulate: let t = 0; for x in xs { t += k } -> let t = 0 + k*xs.len()
        let body = parse_fn_body(
            "let total = 0\nfor x in xs {\n  total += rate\n}\nreturn total");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(!src.contains("for"), "for-loop survived:\n{}", src);
        assert!(src.contains("*") && src.contains("len"),
            "no closed form:\n{}", src);
        // while-accumulate: let i = 0; let t = 0; while i < n { t += k; i += 1 }
        let body = parse_fn_body(
            "let i = 0\nlet t = 0\nwhile i < n {\n  t += step\n  i += 1\n}\nreturn t");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(!src.contains("while"), "while survived:\n{}", src);
        // accumulate the loop var itself -> xs.sum() (NOT the closed-
        // form k*n path — that would be unsound)
        let body = parse_fn_body(
            "let total = 0\nfor x in xs {\n  total += x\n}\nreturn total");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("sum()") && !src.contains("for"),
            "sum-of-elements not rewritten:\n{}", src);
        // k derived from x (x * 2) -> must NOT rewrite at all
        let body = parse_fn_body(
            "let total = 0\nfor x in xs {\n  total += x * 2\n}\nreturn total");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("for"), "unsafe rewrite:\n{}", src);
    }

    #[test]
    fn idiom_rewrite_dedup_loop_to_unique() {
        let mut evolver = Evolver::new();
        let body = parse_fn_body(
            "let out = []\nfor x in xs {\n  if not (x in out) { out = out.push(x) }\n}\nreturn out");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("unique") && !src.contains("for"),
            "dedup loop not rewritten:\n{}", src);
        // wrong accumulator push (push y not x) must NOT rewrite
        let body = parse_fn_body(
            "let out = []\nfor x in xs {\n  if not (x in out) { out = out.push(y) }\n}\nreturn out");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("for"), "unsafe rewrite:\n{}", src);
    }

    #[test]
    fn idiom_rewrite_cse_dedups_shared_pure_expr() {
        let mut evolver = Evolver::new();
        // let dy = <same expr as dx> -> dy uses become dx, let dropped
        let body = parse_fn_body(
            "let dx = a + b\nlet dy = a + b\nreturn dx * dy");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("dx * dx") && !src.contains("let dy"),
            "CSE not applied:\n{}", src);
        // different expr -> no rewrite
        let body = parse_fn_body(
            "let dx = a + b\nlet dy = a - b\nreturn dx * dy");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("let dy"), "unsafe CSE:\n{}", src);
        // reassigned dup var -> must NOT dedup (aliasing would be wrong)
        let body = parse_fn_body(
            "let dx = a + b\nlet dy = a + b\ndy = c\nreturn dx + dy");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("let dy"), "unsafe CSE on reassigned var:\n{}", src);
        // impure expr (call) -> must NOT dedup
        let body = parse_fn_body(
            "let dx = rand()\nlet dy = rand()\nreturn dx + dy");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("let dy"), "impure CSE:\n{}", src);
    }

    #[test]
    fn idiom_rewrite_contains_membership_loop() {
        let mut evolver = Evolver::new();
        // for-form: for x in xs { if x == t { return true } } return false
        let body = parse_fn_body(
            "for x in xs {\n  if x == t { return true }\n}\nreturn false");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("contains") && !src.contains("for"),
            "membership for-loop not rewritten:\n{}", src);
        // while-form: let i=0; while i<xs.len() { if xs[i]==t {return true} i+=1 } return false
        let body = parse_fn_body(
            "let i = 0\nwhile i < xs.len() {\n  if xs[i] == t { return true }\n  i += 1\n}\nreturn false");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("contains") && !src.contains("while"),
            "membership while-loop not rewritten:\n{}", src);
        // tail returns -1 (linsearch shape) -> NOT contains
        let body = parse_fn_body(
            "for x in xs {\n  if x == t { return true }\n}\nreturn -1");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(!src.contains("contains("), "wrong tail:\n{}", src);
    }

    #[test]
    fn idiom_rewrite_count_occurrences() {
        let mut evolver = Evolver::new();
        let body = parse_fn_body(
            "let c = 0\nfor x in xs {\n  if x == t { c += 1 }\n}\nreturn c");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("count") && !src.contains("for"),
            "count loop not rewritten:\n{}", src);
        // counting a different var's occurrences vs the loop var itself — bail
        let body = parse_fn_body(
            "let c = 0\nfor x in xs {\n  if y == t { c += 1 }\n}\nreturn c");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("for"), "unsafe count rewrite:\n{}", src);
    }

    #[cfg(feature = "smt")]
    #[test]
    fn idiom_rewrites_are_provably_sound() {
        // Every idiom rewrite is a FIXED transform — prove semantics
        // preserved for each family via Z3 (Unsat = provably equivalent).
        let mut evolver = Evolver::new();
        let cases: &[(&str, &[(&str, crate::ast::Type)])] = &[
            // sort-extremum -> for-scan
            ("return xs.sort().reverse()[0]",
             &[("xs", crate::ast::Type::List(Box::new(crate::ast::Type::Int)))]),
            // accum loop -> closed form
            ("let t = 0\nfor x in xs { t += 1 }\nreturn t",
             &[("xs", crate::ast::Type::List(Box::new(crate::ast::Type::Int)))]),
            // +=x -> sum()
            ("let t = 0\nfor x in xs { t += x }\nreturn t",
             &[("xs", crate::ast::Type::List(Box::new(crate::ast::Type::Int)))]),
            // count -> xs.count(t)
            ("let c = 0\nfor x in xs {\n  if x == t { c += 1 }\n}\nreturn c",
             &[("xs", crate::ast::Type::List(Box::new(crate::ast::Type::Int))),
                ("t", crate::ast::Type::Int)]),
            // contains -> xs.contains(t)
            ("for x in xs {\n  if x == t { return true }\n}\nreturn false",
             &[("xs", crate::ast::Type::List(Box::new(crate::ast::Type::Int))),
                ("t", crate::ast::Type::Int)]),
        ];
        for (src, params) in cases {
            let before = parse_fn_body(src);
            let after = evolver.mutate_body(&before, Mutation::IdiomRewrite);
            let params: Vec<(String, crate::ast::Type)> = params.iter()
                .map(|(n, t)| (n.to_string(), t.clone())).collect();
            match crate::smt::verify_bodies_exact(&before, &after, &params) {
                Ok(None) => {}
                Ok(Some(cex)) => panic!("rewrite changed semantics: {} -> {:?}\ncex: {:?}",
                    src, crate::fmt::format_program(&after), cex),
                Err(why) => panic!("rewrite not provable: {} -> {:?}\n{}",
                    src, crate::fmt::format_program(&after), why),
            }
        }
    }

    #[test]
    fn idiom_rewrite_map_filter_loops() {
        let mut evolver = Evolver::new();
        // map: let out = []; for x in xs { out.append(x * 2) } return out
        let body = parse_fn_body(
            "let out = []\nfor x in xs {\n  out.append(x * 2)\n}\nreturn out");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("map"), "map loop not rewritten:\n{}", src);
        // filter: let out = []; for x in xs { if x > 2 { out.append(x) } } return out
        let body = parse_fn_body(
            "let out = []\nfor x in xs {\n  if x > 2 { out.append(x) }\n}\nreturn out");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("filter"), "filter loop not rewritten:\n{}", src);
        // bail: pushed expr uses the accumulator — must NOT rewrite
        let body = parse_fn_body(
            "let out = []\nfor x in xs {\n  out.append(out.len())\n}\nreturn out");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(!src.contains("map"), "unsafe map rewrite:\n{}", src);
    }

    #[test]
    fn idiom_rewrite_linsearch_to_index_of() {
        let mut evolver = Evolver::new();
        let body = parse_fn_body(
            "let i = 0\nwhile i < xs.len() {\n  if xs[i] == t { return i }\n  i += 1\n}\nreturn -1");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("index_of") && !src.contains("while"),
            "linsearch not rewritten:\n{}", src);
        // t referencing i must NOT rewrite
        let body = parse_fn_body(
            "let i = 0\nwhile i < xs.len() {\n  if xs[i] == i { return i }\n  i += 1\n}\nreturn -1");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("while"), "unsafe rewrite:\n{}", src);
    }

    #[test]
    fn idiom_rewrite_last_index_extrema() {
        let mut evolver = Evolver::new();
        // sort()[-1] -> MAX scan (>); sort()[0] -> min (<)
        let body = parse_fn_body("let s = xs.sort()\nreturn s[s.len() - 1]");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("for") && src.contains(">") && !src.contains(".sort()"),
            "sort()[len-1] not rewritten to max scan:\n{}", src);
        // sort().reverse()[-1] -> MIN scan (<)
        let body = parse_fn_body("let s = xs.sort()\nlet r = s.reverse()\nreturn r[-1]");
        let out = evolver.mutate_body(&body, Mutation::IdiomRewrite);
        let src = crate::fmt::format_program(&out);
        assert!(src.contains("for") && src.contains("<") && !src.contains(".sort()"),
            "sort().reverse()[-1] not rewritten to min scan:\n{}", src);
    }

    #[test]
    fn simplify_kills_double_reverse() {
        let mut evolver = Evolver::new();
        let body = parse_fn_body("return xs.reverse().reverse()");
        let out = evolver.mutate_body(&body, Mutation::Simplify);
        let src = crate::fmt::format_program(&out);
        assert!(!src.contains(".reverse()"), "double-reverse survived:\n{}", src);
        assert!(src.contains("xs"), "receiver lost:\n{}", src);
    }
}
