// The Veldt ecosystem — where code lives, ages, competes, and dies

use crate::ast::*;
use crate::interpreter::{FunctionVariant, FunctionDef, Interpreter, Value};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Health {
    Healthy,
    Sick(String),  // error message
    Dying,
}

/// Graduated trust: how safe a variant is to depend on.
/// Seedling (newborn) -> Established (survived >=3 cycles) -> Canonical (adopted by user).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Trust { Seedling, Established, Canonical }
impl Default for Trust { fn default() -> Self { Trust::Seedling } }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionEntry {
    pub id: u32,
    pub name: String,
    pub source: String,       // serialized source for re-parsing
    pub params: Vec<Param>,
    pub body: Vec<Stmt>,
    pub tests: Vec<TestAssert>,
    pub born: u64,
    pub age: u64,
    pub health: Health,
    pub lifespan: f64,
    pub usage_count: u64,
    pub last_used: u64,       // run number when last called
    pub fitness: f64,
    pub trial_score: f64,
    pub healing_attempts: Vec<String>,
    // provenance: which variants this one descends from, and how it was born
    #[serde(default)]
    pub parents: Vec<u32>,
    #[serde(default)]
    pub origin: String,  // "seed" | "mutation(op) of f#2" | "crossover f#2 x f#4" | "healing"
    // per-niche probe scores: niche -> fraction of probes passed
    #[serde(default)]
    pub niche_scores: HashMap<String, f64>,
    // efficiency: incumbent-steps / this-steps on big inputs; >1 means faster
    #[serde(default)]
    pub efficiency: f64,
    // graduated trust: seedling -> established -> canonical
    #[serde(default)]
    pub trust: Trust,
}

/// A probe: an ecosystem-generated test. args produced by typed generation;
/// expected = the answer of the family's fittest variant at generation time
/// (consensus ground truth). Probes accumulate — selection pressure grows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Probe {
    pub args: Vec<String>,    // serialized Values
    pub expected: String,     // serialized Value
    pub niche: String,        // input signature, e.g. "int:neg|list:len4+"
    pub born: u64,
    // input scale: 0 = normal, 1 = big (amplifies step-count differences)
    #[serde(default)]
    pub scale: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub args: Vec<String>,    // serialized Values
    pub result: String,       // serialized Value — what the user actually got
    pub variant: u32,         // which variant produced this trace
    pub at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableEntry {
    pub name: String,
    pub value: String,        // serialized value
    pub born: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructEntry {
    pub name: String,
    pub fields: Vec<(String, Type)>,
    pub born: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Obituary {
    pub name: String,
    pub id: u32,
    pub born: u64,
    pub age: u64,
    pub health: Health,
    pub healing_attempts: Vec<String>,
    pub last_used: u64,
    pub cause_of_death: String,
    pub timestamp: u64,
}

#[derive(Clone)]
pub struct Veldt {
    pub functions: HashMap<String, Vec<FunctionEntry>>,  // lineage -> variants
    pub variables: HashMap<String, VariableEntry>,
    pub structs: HashMap<String, StructEntry>,
    pub obituaries: Vec<Obituary>,
    // ecosystem-generated tests, per function family
    pub probes: HashMap<String, Vec<Probe>>,
    // real call traces: observed input/output evidence per family
    pub traces: HashMap<String, Vec<Trace>>,
    pub fitness_history: Vec<f64>,
    pub run_count: u64,
    pub garden_path: PathBuf,
}

fn now_ts() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

impl Veldt {
    pub fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let garden_path = PathBuf::from(home).join(".veldt").join("garden");
        Veldt {
            functions: HashMap::new(),
            variables: HashMap::new(),
            structs: HashMap::new(),
            obituaries: Vec::new(),
            probes: HashMap::new(),
            traces: HashMap::new(),
            fitness_history: Vec::new(),
            run_count: 0,
            garden_path,
        }
    }

    pub fn with_path(path: PathBuf) -> Self {
        Veldt {
            functions: HashMap::new(),
            variables: HashMap::new(),
            structs: HashMap::new(),
            obituaries: Vec::new(),
            probes: HashMap::new(),
            traces: HashMap::new(),
            fitness_history: Vec::new(),
            run_count: 0,
            garden_path: path,
        }
    }

    // === Persistence ===

    pub fn load(&mut self) -> Result<(), String> {
        let index_path = self.garden_path.join("veldt.json");
        if !index_path.exists() {
            return Ok(()); // empty veldt
        }
        let data = std::fs::read_to_string(&index_path)
            .map_err(|e| format!("Failed to read veldt: {}", e))?;
        let saved: SavedVeldt = serde_json::from_str(&data)
            .map_err(|e| format!("Failed to parse veldt: {}", e))?;
        self.functions = saved.functions;
        self.variables = saved.variables;
        self.structs = saved.structs;
        self.obituaries = saved.obituaries;
        self.probes = saved.probes;
        self.traces = saved.traces;
        self.fitness_history = saved.fitness_history;
        self.run_count = saved.run_count;
        Ok(())
    }

    pub fn save(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.garden_path)
            .map_err(|e| format!("Failed to create garden dir: {}", e))?;
        let saved = SavedVeldt {
            functions: self.functions.clone(),
            variables: self.variables.clone(),
            structs: self.structs.clone(),
            obituaries: self.obituaries.clone(),
            probes: self.probes.clone(),
            traces: self.traces.clone(),
            fitness_history: self.fitness_history.clone(),
            run_count: self.run_count,
        };
        let data = serde_json::to_string_pretty(&saved)
            .map_err(|e| format!("Failed to serialize veldt: {}", e))?;
        let index_path = self.garden_path.join("veldt.json");
        std::fs::write(&index_path, data)
            .map_err(|e| format!("Failed to write veldt: {}", e))?;
        Ok(())
    }

    pub fn clear(&mut self) {
        self.functions.clear();
        self.variables.clear();
        self.structs.clear();
        self.traces.clear();
        self.obituaries.clear();
        self.probes.clear();
        self.run_count = 0;
    }

    // === Loading veldt into interpreter ===

    /// Merge interpreter call traces into the persistent store.
    /// Only keeps traces for families that exist in the garden; caps at
    /// 200 traces per family (most recent).
    pub fn merge_traces(&mut self, traces: &HashMap<String, Vec<(Vec<String>, String)>>) {
        for (name, ts) in traces {
            if ts.is_empty() { continue; }
            let entry = self.traces.entry(name.clone()).or_insert_with(Vec::new);
            for (args, result) in ts {
                entry.push(Trace {
                    args: args.clone(),
                    result: result.clone(),
                    variant: 0,
                    at: now_ts(),
                });
            }
            if entry.len() > 200 {
                let excess = entry.len() - 200;
                entry.drain(0..excess);
            }
        }
    }

    pub fn load_into_interpreter(&self, interp: &mut Interpreter) {
        // Load functions as variants
        for (name, variants) in &self.functions {
            for entry in variants {
                if matches!(entry.health, Health::Healthy | Health::Sick(_)) {
                    let variant = FunctionVariant {
                        id: entry.id,
                        fitness: entry.fitness,
                        def: FunctionDef {
                            params: entry.params.clone(),
                            body: std::rc::Rc::new(entry.body.clone()),
                        },
                    };
                    interp.functions.entry(name.clone())
                        .or_insert_with(Vec::new)
                        .push(variant);
                }
            }
        }
        // Load variables
        for (name, entry) in &self.variables {
            if let Ok(val) = deserialize_value(&entry.value) {
                interp.vars.borrow_mut().insert(name.clone(), val);
            }
        }
        // Load structs
        for (name, entry) in &self.structs {
            interp.structs.insert(name.clone(), entry.fields.clone());
        }
    }

    // === Growing code ===

    pub fn grow_function(&mut self, name: &str, params: Vec<Param>, body: Vec<Stmt>, tests: Vec<TestAssert>, source: &str) -> u32 {
        self.grow_function_full(name, params, body, tests, source, Vec::new(), "seed")
    }

    pub fn grow_function_full(&mut self, name: &str, params: Vec<Param>, body: Vec<Stmt>, tests: Vec<TestAssert>, source: &str, parents: Vec<u32>, origin: &str) -> u32 {
        let id = self.functions.get(name)
            .map(|v| v.iter().map(|e| e.id).max().unwrap_or(0) + 1)
            .unwrap_or(1);

        let entry = FunctionEntry {
            id,
            name: name.to_string(),
            source: source.to_string(),
            params,
            body,
            tests,
            born: now_ts(),
            age: 0,
            health: Health::Healthy,  // will be updated by health check
            lifespan: 100.0,
            usage_count: 0,
            last_used: 0,
            fitness: 0.0,
            trial_score: 0.0,
            healing_attempts: Vec::new(),
            parents,
            origin: origin.to_string(),
            niche_scores: HashMap::new(),
            efficiency: 0.0,
            trust: Trust::Seedling,
        };

        self.functions.entry(name.to_string())
            .or_insert_with(Vec::new)
            .push(entry);

        id
    }

    pub fn grow_variable(&mut self, name: &str, value: &Value) {
        let entry = VariableEntry {
            name: name.to_string(),
            value: serialize_value(value),
            born: now_ts(),
        };
        self.variables.insert(name.to_string(), entry);
    }

    pub fn grow_struct(&mut self, name: &str, fields: Vec<(String, Type)>) {
        // supersede: replace if exists
        let entry = StructEntry {
            name: name.to_string(),
            fields,
            born: now_ts(),
        };
        self.structs.insert(name.to_string(), entry);
    }

    // === Fitness and competition ===

    pub fn calculate_fitness(&self, entry: &FunctionEntry) -> f64 {
        let health_score = match &entry.health {
            Health::Healthy => 1.0,
            Health::Sick(_) => 0.1,
            Health::Dying => 0.0,
        };
        let usage_score = (entry.usage_count as f64).ln_1p(); // log scale
        let recency_score = if entry.last_used == self.run_count {
            1.0
        } else if entry.last_used == 0 {
            0.0
        } else {
            let runs_since = (self.run_count - entry.last_used) as f64;
            1.0 / (1.0 + runs_since)
        };
        (health_score * usage_score * recency_score) + entry.trial_score
    }

    pub fn get_fittest_variant(&self, name: &str) -> Option<&FunctionEntry> {
        let variants = self.functions.get(name)?;
        variants.iter()
            .filter(|v| matches!(v.health, Health::Healthy | Health::Sick(_)))
            .max_by(|a, b| {
                let fa = self.calculate_fitness(a);
                let fb = self.calculate_fitness(b);
                fa.partial_cmp(&fb).unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    // === Lifespan calculation ===

    pub fn calculate_lifespan(&self, entry: &FunctionEntry) -> f64 {
        let base = match &entry.health {
            Health::Healthy => 100.0,
            Health::Sick(_) => 10.0,
            Health::Dying => 1.0,
        };
        let usage_bonus = (entry.usage_count as f64).ln_1p() * 10.0;
        let dormancy_penalty = if entry.last_used == 0 {
            0.0
        } else {
            let runs_since = (self.run_count - entry.last_used) as f64;
            runs_since * 2.0
        };
        // competition penalty: if there's a fitter variant in the same lineage
        let competition_penalty = if let Some(fittest) = self.get_fittest_variant(&entry.name) {
            if fittest.id != entry.id {
                let fittest_fitness = self.calculate_fitness(fittest);
                let my_fitness = self.calculate_fitness(entry);
                (fittest_fitness - my_fitness).max(0.0) * 5.0
            } else {
                0.0
            }
        } else {
            0.0
        };
        let lifespan = base + usage_bonus - dormancy_penalty - competition_penalty;
        lifespan.max(0.0)
    }

    // === Aging and death ===

    pub fn age_and_reap(&mut self) -> Vec<Obituary> {
        self.run_count += 1;
        let run_count = self.run_count;

        // First pass: calculate fitness for all entries (immutable borrow)
        let mut fitness_map: HashMap<(String, u32), f64> = HashMap::new();
        for (name, variants) in &self.functions {
            for entry in variants {
                fitness_map.insert((name.clone(), entry.id), self.calculate_fitness(entry));
            }
        }

        // find peak fitness across the whole garden this run
        let peak = fitness_map.values().copied()
            .fold(f64::NEG_INFINITY, f64::max);
        if peak.is_finite() { self.fitness_history.push(peak); }
        if self.fitness_history.len() > 200 { self.fitness_history.remove(0); }

        // Find fittest variant per lineage
        let mut fittest_map: HashMap<String, u32> = HashMap::new();
        for (name, variants) in &self.functions {
            let fittest = variants.iter()
                .max_by(|a, b| {
                    let fa = fitness_map.get(&(name.clone(), a.id)).unwrap_or(&0.0);
                    let fb = fitness_map.get(&(name.clone(), b.id)).unwrap_or(&0.0);
                    fa.partial_cmp(fb).unwrap_or(std::cmp::Ordering::Equal)
                });
            if let Some(f) = fittest {
                fittest_map.insert(name.clone(), f.id);
            }
        }

        // Niche champions: for each family, for each niche present in the probe
        // pool, find the variant with strictly-best niche score. Specialists
        // (champions of >=1 niche) get protected from competition.
        let mut niche_champions: HashMap<(String, u32), Vec<String>> = HashMap::new();
        for (name, probes) in &self.probes {
            let niches: std::collections::HashSet<&String> =
                probes.iter().map(|p| &p.niche).collect();
            for niche in niches {
                let mut best_id = None;
                let mut best_score = f64::MIN;
                if let Some(variants) = self.functions.get(name) {
                    for e in variants {
                        if !matches!(e.health, Health::Healthy) { continue; }
                        let s = e.niche_scores.get(niche).copied().unwrap_or(0.0);
                        if s > best_score {
                            best_score = s;
                            best_id = Some(e.id);
                        }
                    }
                }
                if let (Some(id), true) = (best_id, best_score > 0.0) {
                    niche_champions.entry((name.clone(), id))
                        .or_insert_with(Vec::new)
                        .push(niche.clone());
                }
            }
        }

        // Trust promotion: healthy variants that survive >=3 cycles earn
        // Established status; Canonical comes only from `adopt`.
        for variants in self.functions.values_mut() {
            for e in variants.iter_mut() {
                if e.trust == Trust::Seedling && matches!(e.health, Health::Healthy) && e.age >= 3 {
                    e.trust = Trust::Established;
                }
            }
        }

        // Second pass: age and check death (mutable borrow)
        let mut new_obituaries = Vec::new();
        let mut to_remove: Vec<(String, u32)> = Vec::new();

        for (name, variants) in &mut self.functions {
            for entry in variants.iter_mut() {
                let aging_mult = match &entry.health {
                    Health::Healthy => 1.0,
                    Health::Sick(_) => 3.0,
                    Health::Dying => 5.0,
                };
                entry.age = (entry.age as f64 + aging_mult) as u64;

                // Use pre-computed fitness
                entry.fitness = *fitness_map.get(&(name.clone(), entry.id)).unwrap_or(&0.0);

                // Recalculate lifespan using pre-computed fitness
                let base = match &entry.health {
                    Health::Healthy => 100.0,
                    Health::Sick(_) => 10.0,
                    Health::Dying => 1.0,
                };
                let usage_bonus = (entry.usage_count as f64).ln_1p() * 10.0;
                let dormancy_penalty = if entry.last_used == 0 {
                    0.0
                } else {
                    let runs_since = (run_count.saturating_sub(entry.last_used)) as f64;
                    runs_since * 2.0
                };
                let mut competition_penalty = if let Some(fittest_id) = fittest_map.get(name) {
                    if *fittest_id != entry.id {
                        let fittest_fitness = *fitness_map.get(&(name.clone(), *fittest_id)).unwrap_or(&0.0);
                        let my_fitness = entry.fitness;
                        (fittest_fitness - my_fitness).max(0.0) * 5.0
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };
                // niche protection: specialists face reduced competition
                if niche_champions.contains_key(&(name.clone(), entry.id)) {
                    competition_penalty *= 0.2;
                }
                // Efficiency survival weight: a variant that's measurably
                // faster than the incumbent (eff > 1) is genuinely better —
                // let it survive long enough to earn Established trust.
                // Fixes the gap where an O(n) variant died to an O(n log n)
                // incumbent before its efficiency could protect it.
                let eff_bonus = if entry.efficiency > 1.0 {
                    competition_penalty *= (1.0 / entry.efficiency).clamp(0.05, 1.0);
                    (entry.efficiency.ln() * 20.0).max(0.0)
                } else { 0.0 };
                entry.lifespan = (base + usage_bonus + eff_bonus
                    - dormancy_penalty - competition_penalty).max(0.0);

                // check death
                if entry.age as f64 >= entry.lifespan {
                    let cause = if entry.lifespan < 10.0 {
                        "lifespan exceeded (sickness)".to_string()
                    } else if let Some(fittest_id) = fittest_map.get(name) {
                        if *fittest_id != entry.id {
                            format!("outcompeted by {}#{} — higher fitness", name, fittest_id)
                        } else {
                            "lifespan exceeded".into()
                        }
                    } else {
                        "lifespan exceeded".into()
                    };

                    let obit = Obituary {
                        name: name.clone(),
                        id: entry.id,
                        born: entry.born,
                        age: entry.age,
                        health: entry.health.clone(),
                        healing_attempts: entry.healing_attempts.clone(),
                        last_used: entry.last_used,
                        cause_of_death: cause,
                        timestamp: now_ts(),
                    };
                    new_obituaries.push(obit);
                    to_remove.push((name.clone(), entry.id));
                }
            }
        }

        // Hard pruning: cap healthy variants per family at 8. Weakest
        // non-champions die with a competitive cause of death.
        for (name, variants) in self.functions.iter_mut() {
            let healthy: Vec<u32> = variants.iter()
                .filter(|e| matches!(e.health, Health::Healthy)).map(|e| e.id).collect();
            if healthy.len() > 8 {
                let mut ranked: Vec<(u32, f64, bool)> = variants.iter()
                    .filter(|e| matches!(e.health, Health::Healthy))
                    .map(|e| (e.id, e.fitness, niche_champions.contains_key(&(name.clone(), e.id))))
                    .collect();
                ranked.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
                let excess = healthy.len() - 8;
                let mut culled = 0;
                for (id, fit, champ) in &ranked {
                    if culled >= excess { break; }
                    if *champ { continue; }
                    let obit = Obituary {
                        name: name.clone(), id: *id,
                        born: variants.iter().find(|e| e.id == *id).map(|e| e.born).unwrap_or(0),
                        age: variants.iter().find(|e| e.id == *id).map(|e| e.age).unwrap_or(0),
                        health: Health::Healthy,
                        healing_attempts: vec![],
                        last_used: 0,
                        cause_of_death: format!("outcompeted — family at capacity (fitness {:.2})", fit),
                        timestamp: now_ts(),
                    };
                    new_obituaries.push(obit);
                    to_remove.push((name.clone(), *id));
                    culled += 1;
                }
            }
        }

        // remove dead entries (hard-culled too)
        for (name, id) in &to_remove {
            let mut should_remove = false;
            if let Some(variants) = self.functions.get_mut(name) {
                variants.retain(|e| e.id != *id);
                should_remove = variants.is_empty();
            }
            if should_remove {
                self.functions.remove(name);
            }
        }

        // store obituaries (keep last 100)
        self.obituaries.extend(new_obituaries.clone());
        if self.obituaries.len() > 100 {
            let start = self.obituaries.len() - 100;
            self.obituaries = self.obituaries[start..].to_vec();
        }

        new_obituaries
    }

    // === Usage tracking ===

    pub fn record_usage(&mut self, usage_log: &[(String, u32)]) {
        for (name, variant_id) in usage_log {
            if let Some(variants) = self.functions.get_mut(name) {
                for entry in variants.iter_mut() {
                    if entry.id == *variant_id {
                        entry.usage_count += 1;
                        entry.last_used = self.run_count;
                        break;
                    }
                }
            }
        }
    }

    // === Pruning ===

    pub fn prune(&mut self, name: &str, id: u32) -> bool {
        let mut should_remove = false;
        let mut pruned = false;
        if let Some(variants) = self.functions.get_mut(name) {
            let before = variants.len();
            variants.retain(|e| e.id != id);
            pruned = variants.len() < before;
            should_remove = variants.is_empty();
        }
        if should_remove {
            self.functions.remove(name);
        }
        pruned
    }

    // === Display ===

    pub fn print_garden(&self, sick_only: bool, healthy_only: bool) {
        if self.functions.is_empty() && self.variables.is_empty() && self.structs.is_empty() {
            println!("The veldt is empty.");
            return;
        }

        // Functions
        let mut func_count = 0;
        for (name, variants) in &self.functions {
            // champion marker — the variant `call_best` would dispatch to
            let best_id = variants.iter()
                .max_by(|a, b| a.fitness.partial_cmp(&b.fitness)
                    .unwrap_or(std::cmp::Ordering::Equal))
                .map(|e| e.id);
            for entry in variants {
                let show = if sick_only {
                    matches!(entry.health, Health::Sick(_) | Health::Dying)
                } else if healthy_only {
                    matches!(entry.health, Health::Healthy)
                } else {
                    true
                };
                if show {
                    let health_str = match &entry.health {
                        Health::Healthy => "healthy".to_string(),
                        Health::Sick(e) => format!("sick: {}", e),
                        Health::Dying => "dying".into(),
                    };
                    let marker = if Some(entry.id) == best_id { " *" } else { "" };
                    println!("  {}#{}{} — {} | age: {} | lifespan: {:.1} | usage: {} | fitness: {:.2}",
                        name, entry.id, marker, health_str, entry.age, entry.lifespan, entry.usage_count, entry.fitness);
                    func_count += 1;
                }
            }
        }
        if func_count == 0 && sick_only {
            println!("  (no sick functions)");
        }

        if !sick_only {
            // Variables
            for name in self.variables.keys() {
                println!("  ${} (immortal)", name);
            }
            // Structs
            for name in self.structs.keys() {
                println!("  struct {} (immortal)", name);
            }
        }
    }

    pub fn print_lineage(&self, name: &str) {
        match self.functions.get(name) {
            Some(variants) => {
                println!("Lineage: {} ({} variant{})", name, variants.len(),
                    if variants.len() == 1 { "" } else { "s" });
                for entry in variants {
                    let health_str = match &entry.health {
                        Health::Healthy => "healthy".to_string(),
                        Health::Sick(e) => format!("sick: {}", e),
                        Health::Dying => "dying".into(),
                    };
                    let fittest = self.get_fittest_variant(name);
                    let is_fittest = fittest.map(|f| f.id == entry.id).unwrap_or(false);
                    let marker = if is_fittest { " *" } else { "" };
                    let trust_str = match entry.trust {
                        Trust::Canonical => " [canonical]",
                        Trust::Established => " [estab]",
                        Trust::Seedling => "",
                    };
                    let eff_str = if entry.efficiency > 0.0 {
                        format!(" | eff: {:.2}x", entry.efficiency)
                    } else { String::new() };
                    println!("  {}#{}{}{} — {} | age: {} | usage: {} | fitness: {:.2}{} | born: {}",
                        name, entry.id, marker, trust_str, health_str, entry.age, entry.usage_count, entry.fitness, eff_str, entry.origin);
                }
            }
            None => println!("Lineage '{}' not found in veldt", name),
        }
    }

    pub fn print_obituaries(&self) {
        if self.obituaries.is_empty() {
            println!("No deaths recorded.");
            return;
        }
        for obit in self.obituaries.iter().rev().take(20) {
            let health_str = match &obit.health {
                Health::Healthy => "healthy".to_string(),
                Health::Sick(e) => format!("sick ({})", e),
                Health::Dying => "dying".into(),
            };
            let last_used_str = if obit.last_used == 0 {
                "never".to_string()
            } else {
                format!("{} runs ago", self.run_count.saturating_sub(obit.last_used))
            };
            println!("[OBITUARY] {}#{} — died at age {} ({})", obit.name, obit.id, obit.age, health_str);
            println!("  Born: {}", obit.born);
            println!("  Last used: {}", last_used_str);
            println!("  Cause of death: {}", obit.cause_of_death);
            println!();
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SavedVeldt {
    functions: HashMap<String, Vec<FunctionEntry>>,
    variables: HashMap<String, VariableEntry>,
    structs: HashMap<String, StructEntry>,
    obituaries: Vec<Obituary>,
    #[serde(default)]
    probes: HashMap<String, Vec<Probe>>,
    run_count: u64,

    #[serde(default)]
    pub traces: HashMap<String, Vec<Trace>>,
    #[serde(default)]
    pub fitness_history: Vec<f64>,}

// === Value serialization ===

fn serialize_value(val: &Value) -> String {
    serde_json::to_string(val).unwrap_or_else(|_| "\"null\"".into())
}

pub(crate) fn deserialize_value(s: &str) -> Result<Value, String> {
    serde_json::from_str(s).map_err(|e| e.to_string())
}

/// Evaluate a `field op value` clause against a variant — the
/// garden --filter / evolve --filter query language.
/// fields: fitness age usage size args lifespan last_used niches
///         health trust origin calls
/// ops: > < >= <= == != ~ (contains)
pub fn variant_matches(e: &FunctionEntry, clause: &str) -> bool {
    variant_matches_in(e, "", clause)
}

/// Evaluate a clause against a variant inside a named family —
/// the `family` field matches the family name.
pub fn variant_matches_in(e: &FunctionEntry, family: &str, clause: &str) -> bool {
    // `!`/`not` prefix negates the clause
    let (negate, clause) = if let Some(c) = clause.strip_prefix('!') {
        (true, c.trim_start())
    } else if let Some(c) = clause.strip_prefix("not ") {
        (true, c)
    } else { (false, clause) };

    let parts: Vec<&str> = clause.split_whitespace().collect();
    if parts.len() < 3 { return false; }
    let (field, op, val) = (parts[0], parts[1], parts[2..].join(" "));

    let num_val = val.parse::<f64>().ok();
    let field_num: Option<f64> = match field {
        "fitness" => Some(e.fitness),
        "age" => Some(e.age as f64),
        "usage" => Some(e.usage_count as f64),
        "size" => Some(crate::fmt::format_program(&e.body).len() as f64),
        "args" => Some(e.params.len() as f64),
        "lifespan" => Some(e.lifespan),
        "last_used" => Some(e.last_used as f64),
        "niches" => Some(e.niche_scores.len() as f64),
        "born" => Some(e.born as f64),
        "trial_score" => Some(e.trial_score),
        "efficiency" => Some(e.efficiency),
        "parents" => Some(e.parents.len() as f64),
        _ => None,
    };
    if let (Some(fv), Some(v)) = (field_num, num_val) {
        let r = match op {
            ">" => fv > v, "<" => fv < v,
            ">=" => fv >= v, "<=" => fv <= v,
            "==" => (fv - v).abs() < f64::EPSILON,
            "!=" => (fv - v).abs() >= f64::EPSILON,
            _ => false,
        };
        return if negate { !r } else { r };
    }

    let r = match field {
        _ if field.starts_with("niche:") => {
            // niche:<name> — per-niche score query: niche:int:big > 0.5
            let niche = &field[6..];
            let score = e.niche_scores.get(niche).copied().unwrap_or(0.0);
            match (op, num_val) {
                (">", Some(v)) => score > v, ("<", Some(v)) => score < v,
                (">=", Some(v)) => score >= v, ("<=", Some(v)) => score <= v,
                ("==", Some(v)) => (score - v).abs() < f64::EPSILON,
                ("!=", Some(v)) => (score - v).abs() >= f64::EPSILON,
                _ => false,
            }
        }
        "family" => {
            match op { "==" => family == val, "!=" => family != val, "~" => family.contains(&val), _ => false }
        }
        "health" => {
            let h = match &e.health {
                Health::Healthy => "healthy",
                Health::Sick(_) => "sick",
                Health::Dying => "dying",
            };
            match op { "==" => h == val, "!=" => h != val, _ => false }
        }
        "trust" => {
            let t = match e.trust {
                Trust::Seedling => "seedling",
                Trust::Established => "established",
                Trust::Canonical => "canonical",
                _ => "unknown",
            };
            match op { "==" => t == val, "!=" => t != val, _ => false }
        }
        "origin" => {
            match op { "~" => e.origin.contains(&val), "==" => e.origin == val, _ => false }
        }
        "calls" => {
            let src = crate::fmt::format_program(&e.body);
            match op { "~" => src.contains(&val), _ => false }
        }
        _ => false,
    };
    if negate { !r } else { r }
}

/// Sort a family's variants by a named field — the garden --sort DSL.
/// fields: fitness age usage size args lifespan last_used niches
///         fit_rate usage_rate size_rate efficiency niche_score id
///         origin health trust (desc for numerics, asc for id/origin/health/trust)
pub fn sort_variants(vs: &mut Vec<FunctionEntry>, field: &str) {
    vs.sort_by(|a, b| {
        let ord = match field {
            "fitness" => b.fitness.partial_cmp(&a.fitness),
            "age" => b.age.cmp(&a.age),
            "usage" => b.usage_count.cmp(&a.usage_count),
            "size" => crate::fmt::format_program(&b.body).len()
                .cmp(&crate::fmt::format_program(&a.body).len()),
            "args" => b.params.len().cmp(&a.params.len()),
            "lifespan" => b.lifespan.partial_cmp(&a.lifespan),
            "last_used" => b.last_used.cmp(&a.last_used),
            "niches" => b.niche_scores.len().cmp(&a.niche_scores.len()),
            "fit_rate" => (b.fitness / (b.age as f64 + 1.0))
                .partial_cmp(&(a.fitness / (a.age as f64 + 1.0))),
            "usage_rate" => (b.usage_count as f64 / (b.age as f64 + 1.0))
                .partial_cmp(&(a.usage_count as f64 / (a.age as f64 + 1.0))),
            "size_rate" => (crate::fmt::format_program(&b.body).len() as f64 / (b.age as f64 + 1.0))
                .partial_cmp(&(crate::fmt::format_program(&a.body).len() as f64 / (a.age as f64 + 1.0))),
            "efficiency" => (b.fitness / (crate::fmt::format_program(&b.body).len() as f64 + 1.0))
                .partial_cmp(&(a.fitness / (crate::fmt::format_program(&a.body).len() as f64 + 1.0))),
            "niche_score" => {
                let ascore = a.niche_scores.values().sum::<f64>() / a.niche_scores.len().max(1) as f64;
                let bscore = b.niche_scores.values().sum::<f64>() / b.niche_scores.len().max(1) as f64;
                bscore.partial_cmp(&ascore)
            }
            _ if field.starts_with("niche:") => {
                // niche:<name> — sort by per-niche score
                let niche = &field[6..];
                let ascore = a.niche_scores.get(niche).copied().unwrap_or(0.0);
                let bscore = b.niche_scores.get(niche).copied().unwrap_or(0.0);
                bscore.partial_cmp(&ascore)
            }
            "id" => a.id.cmp(&b.id),
            "born" => b.born.cmp(&a.born),
            "trial_score" => b.trial_score.partial_cmp(&a.trial_score),
            "efficiency" => b.efficiency.partial_cmp(&a.efficiency),
            "parents" => b.parents.len().cmp(&a.parents.len()),
            "origin" => a.origin.cmp(&b.origin),
            "health" => {
                let h = |e: &FunctionEntry| match &e.health {
                    Health::Healthy => 0,
                    Health::Sick(_) => 1,
                    Health::Dying => 2,
                };
                h(a).cmp(&h(b))
            }
            "trust" => {
                let t = |e: &FunctionEntry| match e.trust {
                    Trust::Canonical => 0,
                    Trust::Established => 1,
                    Trust::Seedling => 2,
                    _ => 3,
                };
                t(a).cmp(&t(b))
            }
            _ => return std::cmp::Ordering::Equal,
        };
        ord.unwrap_or(std::cmp::Ordering::Equal)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grow_and_load() {
        let mut veldt = Veldt::with_path(PathBuf::from("/tmp/veldt_test_garden"));
        veldt.clear();

        // Grow a function
        let id = veldt.grow_function("greet", vec![], vec![], vec![], "fn greet() { print(\"hi\") }");
        assert_eq!(id, 1);

        // Grow another variant
        let id2 = veldt.grow_function("greet", vec![], vec![], vec![], "fn greet() { print(\"hello\") }");
        assert_eq!(id2, 2);

        // Check lineage
        let variants = veldt.functions.get("greet").unwrap();
        assert_eq!(variants.len(), 2);
    }

    #[test]
    fn test_grow_variable() {
        let mut veldt = Veldt::with_path(PathBuf::from("/tmp/veldt_test_garden"));
        veldt.clear();
        veldt.grow_variable("greeting", &Value::Str("hello".into()));
        assert!(veldt.variables.contains_key("greeting"));
    }

    #[test]
    fn test_grow_struct_supersede() {
        let mut veldt = Veldt::with_path(PathBuf::from("/tmp/veldt_test_garden"));
        veldt.clear();
        veldt.grow_struct("Point", vec![("x".into(), Type::Int), ("y".into(), Type::Int)]);
        assert_eq!(veldt.structs.get("Point").unwrap().fields.len(), 2);
        // supersede
        veldt.grow_struct("Point", vec![("x".into(), Type::Int), ("y".into(), Type::Int), ("z".into(), Type::Int)]);
        assert_eq!(veldt.structs.get("Point").unwrap().fields.len(), 3);
    }

    #[test]
    fn test_fitness_calculation() {
        let mut veldt = Veldt::with_path(PathBuf::from("/tmp/veldt_test_garden"));
        veldt.clear();
        veldt.run_count = 5;

        let entry = FunctionEntry {
            id: 1, name: "test".into(), source: "".into(),
            params: vec![], body: vec![], tests: vec![],
            born: 0, age: 0, health: Health::Healthy,
            lifespan: 100.0, usage_count: 10, last_used: 5,
            fitness: 0.0, trial_score: 0.5, healing_attempts: vec![],
            parents: vec![], origin: "seed".into(), niche_scores: HashMap::new(),
            efficiency: 0.0, trust: Trust::Seedling,
        };
        let fitness = veldt.calculate_fitness(&entry);
        assert!(fitness > 0.5); // trial_score + usage
    }

    #[test]
    fn test_aging_and_death() {
        let mut veldt = Veldt::with_path(PathBuf::from("/tmp/veldt_test_garden"));
        veldt.clear();

        // Grow a sick function
        veldt.grow_function("broken", vec![], vec![], vec![], "fn broken() { }");
        let entry = veldt.functions.get_mut("broken").unwrap().get_mut(0).unwrap();
        entry.health = Health::Sick("type error".into());

        // Sick base lifespan = 10, aging multiplier = 3 per run
        // After 4 runs: age = 12 > lifespan = 10 → death
        let mut total_obits = Vec::new();
        for _ in 0..5 {
            let obits = veldt.age_and_reap();
            total_obits.extend(obits);
        }
        assert!(!total_obits.is_empty(), "Sick function should have died after several runs");
        assert!(veldt.functions.get("broken").is_none(), "Dead function should be removed");
    }

    #[test]
    fn test_competition() {
        let mut veldt = Veldt::with_path(PathBuf::from("/tmp/veldt_test_garden"));
        veldt.clear();
        veldt.run_count = 5;

        // Two variants, one used more
        veldt.grow_function("sort", vec![], vec![], vec![], "fn sort() { }");
        veldt.grow_function("sort", vec![], vec![], vec![], "fn sort() { }");

        let v1 = veldt.functions.get_mut("sort").unwrap().get_mut(0).unwrap();
        v1.usage_count = 20;
        v1.last_used = 5;
        v1.trial_score = 0.8;

        let v2 = veldt.functions.get_mut("sort").unwrap().get_mut(1).unwrap();
        v2.usage_count = 1;
        v2.last_used = 1;
        v2.trial_score = 0.2;

        let fittest = veldt.get_fittest_variant("sort").unwrap();
        assert_eq!(fittest.id, 1); // v1 should be fitter
    }

    #[test]
    fn test_prune() {
        let mut veldt = Veldt::with_path(PathBuf::from("/tmp/veldt_test_garden"));
        veldt.clear();
        veldt.grow_function("test", vec![], vec![], vec![], "fn test() { }");
        assert!(veldt.prune("test", 1));
        assert!(veldt.functions.get("test").is_none());
    }

    #[test]
    fn test_save_and_load() {
        let path = PathBuf::from("/tmp/veldt_test_save");
        let mut veldt = Veldt::with_path(path.clone());
        veldt.clear();
        veldt.grow_function("greet", vec![], vec![], vec![], "fn greet() { }");
        veldt.save().unwrap();

        let mut veldt2 = Veldt::with_path(path.clone());
        veldt2.load().unwrap();
        assert!(veldt2.functions.contains_key("greet"));
    }
}
