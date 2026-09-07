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

pub struct Veldt {
    pub functions: HashMap<String, Vec<FunctionEntry>>,  // lineage -> variants
    pub variables: HashMap<String, VariableEntry>,
    pub structs: HashMap<String, StructEntry>,
    pub obituaries: Vec<Obituary>,
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
        self.obituaries.clear();
        self.run_count = 0;
    }

    // === Loading veldt into interpreter ===

    pub fn load_into_interpreter(&self, interp: &mut Interpreter) {
        // Load functions as variants
        for (name, variants) in &self.functions {
            for entry in variants {
                if matches!(entry.health, Health::Healthy | Health::Sick(_)) {
                    let variant = FunctionVariant {
                        id: entry.id,
                        def: FunctionDef {
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
        // Load variables
        for (name, entry) in &self.variables {
            if let Ok(val) = deserialize_value(&entry.value) {
                interp.vars.insert(name.clone(), val);
            }
        }
        // Load structs
        for (name, entry) in &self.structs {
            interp.structs.insert(name.clone(), entry.fields.clone());
        }
    }

    // === Growing code ===

    pub fn grow_function(&mut self, name: &str, params: Vec<Param>, body: Vec<Stmt>, tests: Vec<TestAssert>, source: &str) -> u32 {
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
                let competition_penalty = if let Some(fittest_id) = fittest_map.get(name) {
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
                entry.lifespan = (base + usage_bonus - dormancy_penalty - competition_penalty).max(0.0);

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

        // remove dead entries
        for (name, id) in to_remove {
            let mut should_remove = false;
            if let Some(variants) = self.functions.get_mut(&name) {
                variants.retain(|e| e.id != id);
                should_remove = variants.is_empty();
            }
            if should_remove {
                self.functions.remove(&name);
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

    pub fn print_garden(&self, sick_only: bool) {
        if self.functions.is_empty() && self.variables.is_empty() && self.structs.is_empty() {
            println!("The veldt is empty.");
            return;
        }

        // Functions
        let mut func_count = 0;
        for (name, variants) in &self.functions {
            for entry in variants {
                let show = if sick_only {
                    matches!(entry.health, Health::Sick(_) | Health::Dying)
                } else {
                    true
                };
                if show {
                    let health_str = match &entry.health {
                        Health::Healthy => "healthy".to_string(),
                        Health::Sick(e) => format!("sick: {}", e),
                        Health::Dying => "dying".into(),
                    };
                    println!("  {}#{} — {} | age: {} | lifespan: {:.1} | usage: {} | fitness: {:.2}",
                        name, entry.id, health_str, entry.age, entry.lifespan, entry.usage_count, entry.fitness);
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
                    println!("  {}#{}{} — {} | age: {} | usage: {} | fitness: {:.2}",
                        name, entry.id, marker, health_str, entry.age, entry.usage_count, entry.fitness);
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
    run_count: u64,
}

// === Value serialization ===

fn serialize_value(val: &Value) -> String {
    serde_json::to_string(val).unwrap_or_else(|_| "\"null\"".into())
}

fn deserialize_value(s: &str) -> Result<Value, String> {
    serde_json::from_str(s).map_err(|e| e.to_string())
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
