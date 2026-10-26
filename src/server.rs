// Veldt Studio IDE — web server

use crate::agent;
use crate::evolve::Evolver;
use crate::veldt::{Health, Veldt};
use axum::{
    extract::State,
    response::{Html, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

struct AppState {
    veldt: Mutex<Veldt>,
}

pub async fn start_server(port: u16) {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        eprintln!("Starting with empty veldt");
    }

    let state = Arc::new(AppState {
        veldt: Mutex::new(veldt),
    });

    let app = Router::new()
        .route("/", get(serve_ide))
        .route("/api/garden", get(get_garden))
        .route("/api/fuzz", post(fuzz_family))
        .route("/api/absorb", post(absorb))
        .route("/api/stats", get(stats))
        .route("/api/check", post(check_source))
        .route("/api/tournament", post(tournament))
        .route("/api/swarm", post(swarm))
        .route("/api/play", post(play_demo))
        .route("/api/game", post(game_step))
        .route("/api/call", post(call_fn))
        .route("/api/adopt", post(adopt_variant))
        .route("/api/verify", post(verify_family))
        .route("/api/quarantine", post(quarantine_family))
        .route("/api/compare", post(compare_engines))
        .route("/api/lineage", get(lineage_family))
        .route("/api/obituaries", get(obituaries))
        .route("/api/traces", get(traces))
        .route("/api/probes", get(probes))
        .route("/api/niches", get(niches))
        .route("/api/usage", get(usage))
        .route("/api/trend", get(trend))
        .route("/api/sources", get(sources))
        .route("/api/fitness", get(fitness))
        .route("/api/health", get(health))
        .route("/api/trust", get(trust))
        .route("/api/bare", get(bare))
        .route("/api/immigrants", get(immigrants))
        .route("/api/mutation", get(mutation))
        .route("/api/families", get(families))
        .route("/api/variant", get(variant))
        .route("/api/heal", get(heal))
        .route("/api/ages", get(ages))
        .route("/api/census", get(census))
        .route("/api/args", get(args))
        .route("/api/sizes", get(sizes))
        .route("/api/last_used", get(last_used))
        .route("/api/niche_scores", get(niche_scores))
        .route("/api/origins", get(origins))
        .route("/api/deaths", get(deaths))
        .route("/api/selection", get(selection))
        .route("/api/run", post(run_program))
        .route("/api/evolve", post(evolve))
        .route("/api/clear", post(clear_veldt))
        .route("/api/prune", post(prune_variant))
        .route("/api/save", post(save_file))
        .route("/api/files", get(list_files))
        .route("/api/agent", post(agent_task))
        .with_state(state);

    let addr = format!("127.0.0.1:{}", port);
    eprintln!("Veldt Studio running at http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

// --- API Handlers ---

#[derive(Deserialize)]
struct FuzzRequest { name: String }

async fn fuzz_family(
    State(state): State<Arc<AppState>>,
    Json(req): Json<FuzzRequest>,
) -> Json<serde_json::Value> {
    let mut veldt = { state.veldt.lock().unwrap().clone() };
    let words = vec!["fuzz", req.name.as_str()];
    let resp = agent::fuzz_intent(&words, &mut veldt);
    let report = resp.reply;
    { let mut v = state.veldt.lock().unwrap(); *v = veldt; }
    Json(serde_json::json!({ "report": report }))
}

#[derive(Deserialize)]
struct AdoptRequest { name: String, id: u32 }

async fn adopt_variant(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AdoptRequest>,
) -> Json<serde_json::Value> {
    let mut veldt = state.veldt.lock().unwrap();
    let found = veldt.functions.get_mut(&req.name)
        .map(|vs| vs.iter_mut().any(|v| {
            if v.id == req.id { v.trust = crate::veldt::Trust::Canonical; true } else { false }
        }))
        .unwrap_or(false);
    if found {
        veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));
        Json(serde_json::json!({"ok": true}))
    } else {
        Json(serde_json::json!({"ok": false, "error": format!("{}#{} not found", req.name, req.id)}))
    }
}

#[derive(Deserialize)]
struct VerifyRequest { name: String }

async fn verify_family(
    Json(req): Json<VerifyRequest>,
) -> Json<serde_json::Value> {
    let divergent = crate::verify_family_find_divergent(&req.name);
    Json(serde_json::json!({"verified": divergent.is_none(), "divergent": divergent}))
}

async fn quarantine_family(
    Json(req): Json<VerifyRequest>,
) -> Json<serde_json::Value> {
    let divergent = crate::verify_family_find_divergent(&req.name);
    let mut quarantined: Vec<u32> = Vec::new();
    if let Some(id) = divergent {
        let mut veldt = veldt::Veldt::new();
        if veldt.load().is_ok() {
            if let Some(vs) = veldt.functions.get_mut(&req.name) {
                for e in vs.iter_mut() {
                    if e.id == id {
                        e.health = crate::veldt::Health::Dying;
                        quarantined.push(id);
                    }
                }
            }
            veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));
        }
    }
    Json(serde_json::json!({"quarantined": quarantined}))
}

#[derive(Deserialize)]
struct CompareRequest { source: String }

async fn compare_engines(
    Json(req): Json<CompareRequest>,
) -> Json<serde_json::Value> {
    // run the source on both engines, report honest times
    let tmp = std::env::temp_dir().join("veldt_cmp.veldt");
    std::fs::write(&tmp, &req.source).unwrap_or(());
    let tw = std::time::Instant::now();
    let tw_out = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("run").arg(&tmp).output();
    let tw_ms = tw.elapsed().as_millis();
    let vm = std::time::Instant::now();
    let vm_out = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("run").arg("--vm").arg(&tmp).output();
    let vm_ms = vm.elapsed().as_millis();
    Json(serde_json::json!({
        "tw_ms": tw_ms, "vm_ms": vm_ms,
        "tw_out": tw_out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default(),
        "vm_out": vm_out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default(),
    }))
}

#[derive(Deserialize)]
struct LineageRequest { name: String }

async fn lineage_family(
    axum::extract::Query(req): axum::extract::Query<LineageRequest>,
) -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let variants: Vec<serde_json::Value> = veldt.functions.get(&req.name)
        .map(|vs| vs.iter().map(|e| serde_json::json!({
            "id": e.id, "health": format!("{:?}", e.health),
            "fitness": e.fitness, "age": e.age, "lifespan": e.lifespan,
            "usage": e.usage_count, "trust": format!("{:?}", e.trust),
            "origin": e.origin,
        })).collect())
        .unwrap_or_default();
    Json(serde_json::json!({"name": req.name, "variants": variants}))
}

async fn obituaries() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let obits: Vec<serde_json::Value> = veldt.obituaries.iter().map(|o| serde_json::json!({
        "name": o.name, "id": o.id, "age": o.age, "cause": o.cause_of_death,
        "health": format!("{:?}", o.health), "healing": o.healing_attempts.len(),
    })).collect();
    Json(serde_json::json!({"obituaries": obits}))
}

async fn traces() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let tr: Vec<serde_json::Value> = veldt.traces.iter().map(|(n, ts)| serde_json::json!({
        "name": n, "count": ts.len(),
    })).collect();
    Json(serde_json::json!({"traces": tr}))
}

async fn probes() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let pr: Vec<serde_json::Value> = veldt.probes.iter().map(|(n, ps)| serde_json::json!({
        "name": n, "count": ps.len(),
        "niches": ps.iter().map(|p| p.niche.clone()).collect::<Vec<_>>(),
    })).collect();
    Json(serde_json::json!({"probes": pr}))
}

async fn niches() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let mut niche_data: std::collections::HashMap<String, (usize, f64, f64)> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            for (n, s) in &e.niche_scores {
                let ent = niche_data.entry(n.clone()).or_default();
                ent.0 += 1; ent.1 += s; ent.2 = s.max(ent.2);
            }
        }
    }
    let ni: Vec<serde_json::Value> = niche_data.iter().map(|(n, (c, s, mx))| serde_json::json!({
        "niche": n, "count": c, "avg": s / (*c).max(1) as f64, "max": mx,
    })).collect();
    Json(serde_json::json!({"niches": ni}))
}

async fn usage() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let mut use_data: Vec<serde_json::Value> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().map(|e| serde_json::json!({
            "variant": format!("{}#{}", n, e.id), "calls": e.usage_count,
        })).collect::<Vec<_>>())
        .collect();
    use_data.sort_by(|a, b| b["calls"].as_u64().unwrap_or(0)
        .cmp(&a["calls"].as_u64().unwrap_or(0)));
    Json(serde_json::json!({"usage": use_data}))
}

async fn trend() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    Json(serde_json::json!({"trend": veldt.fitness_history}))
}

async fn sources() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let mut src_data: std::collections::HashMap<String, (usize, f64, usize)> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            let origin = e.origin.split(' ').next().unwrap_or("seed").to_string();
            let ent = src_data.entry(origin).or_default();
            ent.0 += 1; ent.1 += e.fitness;
            if e.health == crate::veldt::Health::Healthy { ent.2 += 1; }
        }
    }
    let src: Vec<serde_json::Value> = src_data.iter().map(|(s, (n, f, h))| serde_json::json!({
        "source": s, "count": n, "avg_fitness": f / (*n).max(1) as f64,
        "healthy": h, "success_rate": *h as f64 / (*n).max(1) as f64,
    })).collect();
    Json(serde_json::json!({"sources": src}))
}

async fn fitness() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let mut fit_data: Vec<serde_json::Value> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().map(|e| serde_json::json!({
            "variant": format!("{}#{}", n, e.id), "fitness": e.fitness,
            "health": format!("{:?}", e.health),
        })).collect::<Vec<_>>())
        .collect();
    fit_data.sort_by(|a, b| b["fitness"].as_f64().unwrap_or(0.0)
        .partial_cmp(&a["fitness"].as_f64().unwrap_or(0.0))
        .unwrap_or(std::cmp::Ordering::Equal));
    Json(serde_json::json!({"fitness": fit_data}))
}

async fn health() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let mut healthy = 0usize; let mut sick = 0usize; let mut dying = 0usize;
    for vs in veldt.functions.values() {
        for e in vs {
            match e.health {
                crate::veldt::Health::Healthy => healthy += 1,
                crate::veldt::Health::Sick(_) => sick += 1,
                crate::veldt::Health::Dying => dying += 1,
            }
        }
    }
    Json(serde_json::json!({"healthy": healthy, "sick": sick, "dying": dying}))
}

async fn trust() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let mut seed = 0usize; let mut estab = 0usize; let mut canon = 0usize;
    for vs in veldt.functions.values() {
        for e in vs {
            match e.trust {
                crate::veldt::Trust::Seedling => seed += 1,
                crate::veldt::Trust::Established => estab += 1,
                crate::veldt::Trust::Canonical => canon += 1,
            }
        }
    }
    Json(serde_json::json!({"seedling": seed, "established": estab, "canonical": canon}))
}

async fn bare() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let bare: Vec<String> = veldt.functions.keys()
        .filter(|n| veldt.probes.get(*n).map(|p| p.is_empty()).unwrap_or(true))
        .cloned().collect();
    Json(serde_json::json!({"bare": bare}))
}

async fn immigrants() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let imm: Vec<serde_json::Value> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter()
            .filter(|e| e.origin.starts_with("immigrant"))
            .map(|e| serde_json::json!({
                "variant": format!("{}#{}", n, e.id), "origin": e.origin,
                "fitness": e.fitness, "health": format!("{:?}", e.health),
            })).collect::<Vec<_>>())
        .collect();
    Json(serde_json::json!({"immigrants": imm}))
}

async fn mutation() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let mut src_count: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            *src_count.entry(e.origin.clone()).or_default() += 1;
        }
    }
    let mut src: Vec<(String, usize)> = src_count.into_iter().collect();
    src.sort_by(|a, b| b.1.cmp(&a.1));
    Json(serde_json::json!({"sources": src.iter()
        .map(|(s, c)| serde_json::json!({"source": s, "count": c}))
        .collect::<Vec<_>>()}))
}

async fn families() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let fams: Vec<serde_json::Value> = veldt.functions.iter()
        .map(|(n, vs)| serde_json::json!({
            "name": n, "variants": vs.len(),
            "probes": veldt.probes.get(n).map(|p| p.len()).unwrap_or(0),
        })).collect();
    Json(serde_json::json!({"families": fams}))
}

#[derive(Deserialize)]
struct VariantRequest { name: String, id: u32 }

async fn variant(
    axum::extract::Query(req): axum::extract::Query<VariantRequest>,
) -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let v = veldt.functions.get(&req.name)
        .and_then(|vs| vs.iter().find(|e| e.id == req.id));
    match v {
        Some(e) => Json(serde_json::json!({
            "name": req.name, "id": e.id,
            "fitness": e.fitness, "health": format!("{:?}", e.health),
            "age": e.age, "lifespan": e.lifespan, "usage": e.usage_count,
            "trust": format!("{:?}", e.trust), "origin": e.origin,
            "niche_scores": e.niche_scores,
            "source": crate::fmt::format_program(&e.body),
        })),
        None => Json(serde_json::json!({"error": format!("{}#{} not found", req.name, req.id)})),
    }
}

async fn heal() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let heal_data: Vec<serde_json::Value> = veldt.obituaries.iter()
        .filter(|o| !o.healing_attempts.is_empty())
        .map(|o| serde_json::json!({
            "variant": format!("{}#{}", o.name, o.id), "age": o.age,
            "attempts": o.healing_attempts, "cause": o.cause_of_death,
        })).collect();
    Json(serde_json::json!({"healing": heal_data}))
}

async fn ages() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let age_data: Vec<serde_json::Value> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().map(|e| serde_json::json!({
            "variant": format!("{}#{}", n, e.id), "age": e.age,
            "lifespan": e.lifespan,
        })).collect::<Vec<_>>())
        .collect();
    Json(serde_json::json!({"ages": age_data}))
}

async fn census() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let total: usize = veldt.functions.values().map(|vs| vs.len()).sum();
    Json(serde_json::json!({
        "lineages": veldt.functions.len(), "variants": total,
        "probes": veldt.probes.values().map(|v| v.len()).sum::<usize>(),
        "deaths": veldt.obituaries.len(), "runs": veldt.run_count,
        "structs": veldt.structs.len(), "variables": veldt.variables.len(),
    }))
}

async fn args() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let arg_data: Vec<serde_json::Value> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().map(|e| serde_json::json!({
            "variant": format!("{}#{}", n, e.id), "params": e.params.len(),
        })).collect::<Vec<_>>())
        .collect();
    Json(serde_json::json!({"args": arg_data}))
}

async fn sizes() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let size_data: Vec<serde_json::Value> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().map(|e| serde_json::json!({
            "variant": format!("{}#{}", n, e.id),
            "size": crate::fmt::format_program(&e.body).len(),
        })).collect::<Vec<_>>())
        .collect();
    Json(serde_json::json!({"sizes": size_data}))
}

async fn last_used() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let stale_data: Vec<serde_json::Value> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().map(|e| serde_json::json!({
            "variant": format!("{}#{}", n, e.id),
            "last_used": e.last_used,
            "staleness": veldt.run_count.saturating_sub(e.last_used),
        })).collect::<Vec<_>>())
        .collect();
    Json(serde_json::json!({"last_used": stale_data}))
}

async fn niche_scores() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let niche_data: Vec<serde_json::Value> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().map(|e| serde_json::json!({
            "variant": format!("{}#{}", n, e.id),
            "niches": e.niche_scores,
        })).collect::<Vec<_>>())
        .collect();
    Json(serde_json::json!({"niche_scores": niche_data}))
}

async fn origins() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let origin_data: Vec<serde_json::Value> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().map(|e| serde_json::json!({
            "variant": format!("{}#{}", n, e.id), "origin": e.origin,
        })).collect::<Vec<_>>())
        .collect();
    Json(serde_json::json!({"origins": origin_data}))
}

async fn deaths() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let mut cause_count: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for o in &veldt.obituaries {
        *cause_count.entry(o.cause_of_death.clone()).or_default() += 1;
    }
    let mut causes: Vec<(String, usize)> = cause_count.into_iter().collect();
    causes.sort_by(|a, b| b.1.cmp(&a.1));
    Json(serde_json::json!({"causes": causes.iter()
        .map(|(c, n)| serde_json::json!({"cause": c, "count": n}))
        .collect::<Vec<_>>()}))
}

async fn selection() -> Json<serde_json::Value> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let live_peak: f64 = veldt.functions.values()
        .flat_map(|vs| vs.iter().map(|e| e.fitness))
        .fold(f64::NEG_INFINITY, f64::max);
    let dead_age: f64 = if !veldt.obituaries.is_empty() {
        veldt.obituaries.iter().map(|o| o.age as f64).sum::<f64>()
            / veldt.obituaries.len() as f64
    } else { 0.0 };
    let (mut h, mut s, mut d) = (0usize, 0usize, 0usize);
    for o in &veldt.obituaries {
        match o.health {
            crate::veldt::Health::Healthy => h += 1,
            crate::veldt::Health::Sick(_) => s += 1,
            crate::veldt::Health::Dying => d += 1,
        }
    }
    Json(serde_json::json!({
        "live_peak": live_peak, "dead_avg_age": dead_age,
        "death_health": {"healthy": h, "sick": s, "dying": d},
    }))
}

#[derive(Deserialize)]
struct CallRequest { name: String, variant: Option<u32>, args: Option<Vec<serde_json::Value>> }

async fn call_fn(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CallRequest>,
) -> Json<serde_json::Value> {
    let mut veldt = { state.veldt.lock().unwrap().clone() };
    let mut interp = crate::interpreter::Interpreter::new();
    interp.caps = crate::interpreter::Caps::sandboxed();
    veldt.load_into_interpreter(&mut interp);
    let args: Vec<crate::interpreter::Value> = (req.args.unwrap_or_default())
        .iter().filter_map(|a| serde_json::from_value(a.clone()).ok()).collect();
    match interp.call_function(&req.name, req.variant, args) {
        Ok(v) => Json(serde_json::json!({"ok": true, "result": serde_json::to_value(&v).unwrap_or(serde_json::Value::Null)})),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e})),
    }
}

#[derive(Deserialize)]
struct PlayRequest { demo: String }

async fn play_demo(
    Json(req): Json<PlayRequest>,
) -> Json<serde_json::Value> {
    let path = format!("examples/{}.veldt", req.demo);
    if !std::path::Path::new(&path).exists() {
        return Json(serde_json::json!({ "ok": false, "error": format!("no demo '{}'", req.demo) }));
    }
    // run the demo and capture stdout (the canvas_print output)
    let out = crate::run_program_capture(&path);
    Json(serde_json::json!({ "ok": true, "output": out }))
}

#[derive(Deserialize)]
struct GameRequest {
    // POST {source, ticks} — run `source` for `ticks` game_loop ticks,
    // return the program's output. The IDE drives a live game panel.
    source: String,
    ticks: Option<i64>,
}

async fn game_step(
    Json(req): Json<GameRequest>,
) -> Json<serde_json::Value> {
    // write the source to a temp file and run it — the game program's
    // stdout is its canvas state.
    let tmp = std::env::temp_dir().join(format!("veldt_game_{}.veldt", std::process::id()));
    let src = format!("{}\n# tick limit {}\n", req.source, req.ticks.unwrap_or(0));
    if let Err(e) = std::fs::write(&tmp, src) {
        return Json(serde_json::json!({ "ok": false, "error": format!("temp write: {}", e) }));
    }
    let out = crate::run_program_capture(tmp.to_str().unwrap_or(""));
    let _ = std::fs::remove_file(&tmp);
    Json(serde_json::json!({ "ok": true, "output": out }))
}

#[derive(Deserialize)]
struct SwarmRequest { url: String }

async fn swarm(
    Json(req): Json<SwarmRequest>,
) -> Json<serde_json::Value> {
    let url = req.url.trim_end_matches('/').to_string();
    crate::merge_garden(&format!("{}/api/garden", url));
    crate::push_garden(&url);
    Json(serde_json::json!({ "ok": true, "peer": url }))
}

#[derive(Deserialize)]
struct TournamentRequest { url: String }

async fn tournament(
    Json(req): Json<TournamentRequest>,
) -> Json<serde_json::Value> {
    crate::tournament(&req.url, None);
    Json(serde_json::json!({ "ok": true, "url": req.url }))
}

async fn stats(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let v = { state.veldt.lock().unwrap().clone() };
    let (mut healthy, mut sick, mut dying) = (0usize, 0usize, 0usize);
    let (mut seed, mut estab, mut canon) = (0usize, 0usize, 0usize);
    let mut total = 0usize;
    for vs in v.functions.values() {
        for e in vs {
            total += 1;
            match e.health {
                crate::veldt::Health::Healthy => healthy += 1,
                crate::veldt::Health::Sick(_) => sick += 1,
                crate::veldt::Health::Dying => dying += 1,
            }
            match e.trust {
                crate::veldt::Trust::Seedling => seed += 1,
                crate::veldt::Trust::Established => estab += 1,
                crate::veldt::Trust::Canonical => canon += 1,
            }
        }
    }
    // histograms matching `veldt stats`
    const BUILTINS: &[&str] = &[
        "canvas", "grid_", "ui_", "bt_", "balance", "astar", "bfs", "los",
        "fov", "rand", "roll", "dice", "choice", "weighted", "noise", "fbm",
        "perlin", "poisson", "voronoi", "delaunay", "wfc", "maze", "flood",
        "neighbor", "lerp", "clamp", "dist", "aabb", "smoothstep", "bezier",
        "point_in_poly", "convex", "ease", "bounce", "line_points", "turn",
        "fsm", "book", "svc_", "robot", "edge", "game_loop", "dist_stats",
    ];
    let mut use_count: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut fit_by_source: std::collections::HashMap<String, Vec<f64>> = std::collections::HashMap::new();
    let mut mut_rate: std::collections::HashMap<String, (usize, usize)> = std::collections::HashMap::new();
    let mut niche_sum: std::collections::HashMap<String, (f64, usize)> = std::collections::HashMap::new();
    for vs in v.functions.values() {
        for e in vs {
            let src = crate::fmt::format_program(&e.body);
            for b in BUILTINS { if src.contains(b) { *use_count.entry(b).or_default() += 1; } }
            let origin = e.origin.split(' ').next().unwrap_or("seed").to_string();
            fit_by_source.entry(origin.clone()).or_default().push(e.fitness);
            let ent = mut_rate.entry(origin).or_default();
            if e.health == crate::veldt::Health::Healthy { ent.0 += 1; }
            ent.1 += 1;
            for (n, s) in &e.niche_scores {
                let ent = niche_sum.entry(n.clone()).or_default();
                ent.0 += s; ent.1 += 1;
            }
        }
    }
    Json(serde_json::json!({
        "lineages": v.functions.len(), "variants": total,
        "healthy": healthy, "sick": sick, "dying": dying,
        "seedling": seed, "established": estab, "canonical": canon,
        "probes": v.probes.values().map(|p| p.len()).sum::<usize>(),
        "deaths": v.obituaries.len(), "runs": v.run_count,
        "builtin_usage": use_count.iter().map(|(b, c)| serde_json::json!({*b: c})).collect::<Vec<_>>(),
        "fitness_by_source": fit_by_source.iter().map(|(s, fits)| serde_json::json!({
            s.as_str(): fits.iter().sum::<f64>() / fits.len().max(1) as f64,
        })).collect::<Vec<_>>(),
        "mutation_success": mut_rate.iter().map(|(s, (h, t))| serde_json::json!({
            s.as_str(): *h as f64 / (*t).max(1) as f64,
        })).collect::<Vec<_>>(),
        "avg_niche_scores": niche_sum.iter().map(|(n, (s, c))| serde_json::json!({
            n.as_str(): *s / (*c).max(1) as f64,
        })).collect::<Vec<_>>(),
        "probe_coverage": v.probes.iter().map(|(n, p)| serde_json::json!({n.as_str(): p.len()})).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
struct CheckRequest { source: String }

async fn check_source(
    Json(req): Json<CheckRequest>,
) -> Json<serde_json::Value> {
    // parse + lint the posted source — returns warnings JSON
    let mut lx = crate::lexer::Lexer::new(&req.source);
    let res = lx.tokenize().map_err(|e| format!("lex: {}", e))
        .and_then(|t| {
            let mut p = crate::parser::Parser::new_with_lines(t, lx.token_lines.clone());
            p.parse_program().map_err(|e| format!("parse: {}", e))
        });
    match res {
        Ok(stmts) => {
            let warnings = crate::lint(&stmts);
            Json(serde_json::json!({"ok": true, "warnings": warnings}))
        }
        Err(e) => Json(serde_json::json!({"ok": false, "error": e})),
    }
}

async fn absorb(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    // bake live call traces into the probe pool — the IDE's
    // "learn from usage" button
    crate::absorb_traces(None);
    let n = { let v = state.veldt.lock().unwrap(); v.probes.values().map(|p| p.len()).sum::<usize>() };
    Json(serde_json::json!({ "absorbed": n }))
}

async fn get_garden(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let veldt = state.veldt.lock().unwrap();
    Json(garden_json(&veldt))
}

#[derive(Deserialize)]
struct RunRequest {
    source: String,
    #[serde(default)]
    sandbox: bool,
}

#[derive(Serialize)]
struct RunResponse {
    output: String,
    events: Vec<String>,
    success: bool,
}

async fn run_program(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RunRequest>,
) -> Json<RunResponse> {
    let mut veldt = {
        let v = state.veldt.lock().unwrap();
        v.clone()
    };

    let result = agent::execute_source_opts(&req.source, &mut veldt,
        if req.sandbox { crate::interpreter::Caps::sandboxed() }
        else { crate::interpreter::Caps::allow_all() });

    veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));

    {
        let mut v = state.veldt.lock().unwrap();
        *v = veldt;
    }

    Json(RunResponse {
        output: result.output,
        events: result.events,
        success: result.success,
    })
}

#[derive(Deserialize)]
struct AgentRequest {
    task: String,
    #[serde(default)]
    source: String,
}

#[derive(Serialize)]
struct AgentApiResponse {
    steps: Vec<agent::AgentStep>,
    reply: String,
    code: Option<String>,
    ran: bool,
    output: Option<String>,
    success: Option<bool>,
    events: Vec<String>,
    garden: serde_json::Value,
}

async fn agent_task(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AgentRequest>,
) -> Json<AgentApiResponse> {
    let mut veldt = {
        let v = state.veldt.lock().unwrap();
        v.clone()
    };

    let resp = agent::respond(&req.task, &req.source, &mut veldt);

    veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));
    let garden = garden_json(&veldt);

    {
        let mut v = state.veldt.lock().unwrap();
        *v = veldt;
    }

    Json(AgentApiResponse {
        steps: resp.steps,
        reply: resp.reply,
        code: resp.code,
        ran: resp.ran,
        output: resp.output,
        success: resp.success,
        events: resp.events,
        garden,
    })
}

#[derive(Deserialize)]
struct EvolveRequest {
    cycles: usize,
    #[serde(default)]
    llm: bool,
}

#[derive(Serialize)]
struct EvolveResponse {
    events: Vec<String>,
    garden: serde_json::Value,
}

async fn evolve(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EvolveRequest>,
) -> Json<EvolveResponse> {
    let mut veldt = {
        let v = state.veldt.lock().unwrap();
        v.clone()
    };

    let mut evolver = Evolver::new();
    evolver.llm = req.llm;
    let events = evolver.evolve(&mut veldt, req.cycles);
    let event_strings: Vec<String> = events.iter().map(|e| e.format()).collect();

    veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));

    let garden = garden_json(&veldt);

    {
        let mut v = state.veldt.lock().unwrap();
        *v = veldt;
    }

    Json(EvolveResponse { events: event_strings, garden })
}

async fn clear_veldt(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let mut veldt = {
        let v = state.veldt.lock().unwrap();
        let mut new_v = v.clone();
        new_v.clear();
        new_v
    };
    veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));

    {
        let mut v = state.veldt.lock().unwrap();
        *v = veldt;
    }

    Json(serde_json::json!({"status": "cleared"}))
}

#[derive(Deserialize)]
struct PruneRequest {
    name: String,
    id: u32,
}

async fn prune_variant(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PruneRequest>,
) -> Json<serde_json::Value> {
    let mut veldt = {
        let v = state.veldt.lock().unwrap();
        v.clone()
    };

    let pruned = veldt.prune(&req.name, req.id);
    veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));

    {
        let mut v = state.veldt.lock().unwrap();
        *v = veldt;
    }

    Json(serde_json::json!({"pruned": pruned}))
}

#[derive(Deserialize)]
struct SaveRequest {
    filename: String,
    source: String,
}

async fn save_file(
    Json(req): Json<SaveRequest>,
) -> Json<serde_json::Value> {
    let path = if req.filename.ends_with(".veldt") {
        req.filename.clone()
    } else {
        format!("{}.veldt", req.filename)
    };

    match std::fs::write(&path, &req.source) {
        Ok(_) => Json(serde_json::json!({"status": "saved", "path": path})),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

async fn list_files() -> Json<serde_json::Value> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir("examples") {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with(".veldt") {
                    files.push(name.to_string());
                }
            }
        }
    }
    Json(serde_json::json!({"files": files}))
}

// --- Helpers ---

fn garden_json(veldt: &Veldt) -> serde_json::Value {
    serde_json::json!({
        "functions": veldt.functions.iter().map(|(name, variants)| {
            serde_json::json!({
                "name": name,
                "variants": variants.iter().map(|e| {
                    let health_str = match &e.health {
                        Health::Healthy => "healthy",
                        Health::Sick(_) => "sick",
                        Health::Dying => "dying",
                    };
                    let health_detail = match &e.health {
                        Health::Healthy => "".to_string(),
                        Health::Sick(e) => e.clone(),
                        Health::Dying => "".into(),
                    };
                    serde_json::json!({
                        "id": e.id,
                        "health": health_str,
                        "health_detail": health_detail,
                        "age": e.age,
                        "lifespan": (e.lifespan as i64),
                        "usage": e.usage_count,
                        "fitness": (e.fitness * 100.0) as i64,
                        "trial_score": (e.trial_score * 100.0) as i64,
                        "last_used": e.last_used,
                        "healing_attempts": e.healing_attempts.len(),
                        "origin": e.origin,
                        "parents": e.parents,
                        "niche_scores": e.niche_scores.iter().map(|(k, v)| {
                            serde_json::json!({"niche": k, "score": (v * 100.0) as i64})
                        }).collect::<Vec<_>>(),
                        "source": e.source,
                        "body_source": crate::fmt::format_program(&e.body).trim_end().to_string(),
                        "efficiency": (e.efficiency * 100.0) as i64,
                        "trust": match e.trust {
                            crate::veldt::Trust::Canonical => "canonical",
                            crate::veldt::Trust::Established => "established",
                            crate::veldt::Trust::Seedling => "seedling",
                        },
                    })
                }).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
        "variables": veldt.variables.keys().collect::<Vec<_>>(),
        "structs": veldt.structs.iter().map(|(name, entry)| {
            serde_json::json!({
                "name": name,
                "fields": entry.fields.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
        "obituaries": veldt.obituaries.iter().rev().take(20).map(|o| {
            let health_str = match &o.health {
                Health::Healthy => "healthy",
                Health::Sick(_) => "sick",
                Health::Dying => "dying",
            };
            serde_json::json!({
                "name": o.name,
                "id": o.id,
                "age": o.age,
                "health": health_str,
                "cause": o.cause_of_death,
                "last_used": o.last_used,
            })
        }).collect::<Vec<_>>(),
        "traces": veldt.traces.iter().map(|(name, ts)| {
            serde_json::json!({"name": name, "count": ts.len()})
        }).collect::<Vec<_>>(),
        "probes": veldt.probes.iter().map(|(name, probes)| {
            serde_json::json!({
                "name": name,
                "count": probes.len(),
                "niches": probes.iter().map(|p| p.niche.clone())
                    .collect::<std::collections::HashSet<_>>().into_iter().collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
        "run_count": veldt.run_count,
    })
}

// --- IDE HTML ---

async fn serve_ide() -> Html<&'static str> {
    Html(IDE_HTML)
}

const IDE_HTML: &str = include_str!("ide.html");
