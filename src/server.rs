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
    /// live game sessions — id -> the shared game-io channel
    /// (input queue + frame buffer + running flag)
    games: Mutex<std::collections::HashMap<u64, std::sync::Arc<crate::interpreter::GameIo>>>,
    next_game: std::sync::atomic::AtomicU64,
}

pub async fn start_server(port: u16) {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        eprintln!("Starting with empty veldt");
    }
    // --vm — every code-run through the fast engine for this session
    if std::env::args().any(|a| a == "--vm") {
        veldt.prefer_vm = true;
        eprintln!("[server] prefer_vm — all evaluate paths on the bytecode VM + JIT");
    }

    let state = Arc::new(AppState {
        veldt: Mutex::new(veldt),
        games: Mutex::new(std::collections::HashMap::new()),
        next_game: std::sync::atomic::AtomicU64::new(1),
    });

    let app = Router::new()
        .route("/", get(serve_ide))
        .route("/site/:name", get(serve_site))
        .route("/play/:name", get(serve_play))
        .route("/browse", get(serve_browse))
        .route("/examples/:name", get(serve_example))
        .route("/api/deploy", post(deploy_project))
        .route("/api/garden", get(get_garden))
        .route("/api/fuzz", post(fuzz_family))
        .route("/api/absorb", post(absorb))
        .route("/api/stats", get(stats))
        .route("/api/check", post(check_source))
        .route("/api/fix", post(fix_source))
        .route("/api/lint", get(garden_lint))
        .route("/api/tournament", post(tournament))
        .route("/api/swarm", post(swarm))
        .route("/api/play", post(play_demo))
        .route("/api/game", post(game_step))
        .route("/api/game/start", post(game_start))
        .route("/api/game/frame", get(game_frame))
        .route("/api/game/input", post(game_input))
        .route("/api/game/stop", post(game_stop))
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
        .route("/api/filter", get(filter_garden))
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
        .route("/api/mutation_rate", get(mutation_rate))
        .route("/api/turnover", get(turnover))
        .route("/api/diversity", get(diversity))
        .route("/api/pressure", get(pressure))
        .route("/api/builtin_usage", get(builtin_usage))
        .route("/api/champions", get(champions))
        .route("/api/summary", get(summary))
        .route("/api/why", get(why_variant))
        .route("/api/species", get(species))
        .route("/api/find", get(find_family))
        .route("/api/diff", get(diff_variants))
        .route("/api/obituary", get(obituary))
        .route("/api/trace", get(trace_family))
        .route("/api/probe", get(probe_family))
        .route("/api/family", get(family_detail))
        .route("/api/sick", get(sick))
        .route("/api/vars", get(vars))
        .route("/api/structs", get(structs))
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
        let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let tr: Vec<serde_json::Value> = veldt.traces.iter().map(|(n, ts)| serde_json::json!({
        "name": n, "count": ts.len(),
    })).collect();
    Json(serde_json::json!({"traces": tr}))
}

async fn probes() -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    Json(serde_json::json!({"trend": veldt.fitness_history}))
}

async fn sources() -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let bare: Vec<String> = veldt.functions.keys()
        .filter(|n| veldt.probes.get(*n).map(|p| p.is_empty()).unwrap_or(true))
        .cloned().collect();
    Json(serde_json::json!({"bare": bare}))
}

async fn immigrants() -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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

#[derive(Deserialize)]
struct FilterRequest {
    expr: Option<String>,
    sort: Option<String>,
}

async fn filter_garden(
    axum::extract::Query(req): axum::extract::Query<FilterRequest>,
) -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let expr = req.expr.unwrap_or_default();
    let or_groups: Vec<Vec<&str>> = expr.split("||")
        .map(|g| g.split("&&").map(|s| s.trim()).collect())
        .collect();
    let mut filtered = std::collections::HashMap::new();
    for (name, vs) in &veldt.functions {
        let keep: Vec<_> = vs.iter()
            .filter(|e| or_groups.iter().any(|g|
                g.iter().all(|c| crate::veldt::variant_matches_in(e, name, c))))
            .cloned().collect();
        if !keep.is_empty() { filtered.insert(name.clone(), keep); }
    }
    if let Some(field) = &req.sort {
        for vs in filtered.values_mut() {
            crate::veldt::sort_variants(vs, field);
        }
    }
    let mut out = serde_json::Map::new();
    for (name, vs) in &filtered {
        let keep: Vec<_> = vs.iter().map(|e| serde_json::json!({
            "id": e.id, "fitness": e.fitness, "age": e.age,
            "usage": e.usage_count, "trust": format!("{:?}", e.trust),
            "health": format!("{:?}", e.health), "origin": e.origin,
        })).collect();
        out.insert(name.clone(), serde_json::json!(keep));
    }
    Json(serde_json::json!({"expr": expr, "sort": req.sort, "families": out}))
}

async fn families() -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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
    let mut veldt = Veldt::new();
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

async fn mutation_rate() -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let mut src_rate: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            let origin = e.origin.split(' ').next().unwrap_or("seed").to_string();
            let ent = src_rate.entry(origin).or_default();
            if e.health == crate::veldt::Health::Healthy { ent.0 += 1; }
            ent.1 += 1;
        }
    }
    let rates: Vec<serde_json::Value> = src_rate.iter().map(|(s, (h, t))| serde_json::json!({
        "source": s, "healthy": h, "total": t,
        "rate": *h as f64 / (*t).max(1) as f64,
    })).collect();
    Json(serde_json::json!({"mutation_rate": rates}))
}

async fn turnover() -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let ages: Vec<u64> = veldt.functions.values()
        .flat_map(|vs| vs.iter().map(|e| e.age)).collect();
    let avg_age = ages.iter().sum::<u64>() / ages.len().max(1) as u64;
    let dead_age: f64 = if !veldt.obituaries.is_empty() {
        veldt.obituaries.iter().map(|o| o.age as f64).sum::<f64>()
            / veldt.obituaries.len() as f64
    } else { 0.0 };
    let stale: Vec<u64> = veldt.functions.values()
        .flat_map(|vs| vs.iter().map(|e| veldt.run_count.saturating_sub(e.last_used)))
        .collect();
    let avg_stale = stale.iter().sum::<u64>() / stale.len().max(1) as u64;
    Json(serde_json::json!({
        "avg_age": avg_age, "avg_death_age": dead_age, "avg_staleness": avg_stale,
    }))
}

async fn diversity() -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let sizes: Vec<usize> = veldt.functions.values().map(|vs| vs.len()).collect();
    let avg_fam = sizes.iter().sum::<usize>() / sizes.len().max(1);
    let niche_counts: Vec<usize> = veldt.functions.values()
        .flat_map(|vs| vs.iter().map(|e| e.niche_scores.len())).collect();
    let avg_niches = niche_counts.iter().sum::<usize>() / niche_counts.len().max(1);
    let immigrants: usize = veldt.functions.values()
        .flat_map(|vs| vs.iter())
        .filter(|e| e.origin.starts_with("immigrant")).count();
    Json(serde_json::json!({
        "avg_family_size": avg_fam, "avg_niches": avg_niches,
        "immigrants": immigrants,
    }))
}

async fn pressure() -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let per_family: Vec<serde_json::Value> = veldt.functions.iter()
        .map(|(n, vs)| {
            let usage: u64 = vs.iter().map(|e| e.usage_count).sum();
            let peak: f64 = vs.iter().map(|e| e.fitness)
                .fold(f64::NEG_INFINITY, f64::max);
            serde_json::json!({
                "name": n, "probes": veldt.probes.get(n).map(|p| p.len()).unwrap_or(0),
                "usage": usage, "peak_fitness": peak,
            })
        }).collect();
    Json(serde_json::json!({"pressure": per_family}))
}

async fn builtin_usage() -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    const BUILTINS: &[&str] = &[
        "canvas", "grid_", "ui_", "bt_", "balance", "astar", "bfs", "los",
        "fov", "rand", "roll", "dice", "choice", "weighted", "noise", "fbm",
        "perlin", "poisson", "voronoi", "delaunay", "wfc", "maze", "flood",
        "neighbor", "lerp", "clamp", "dist", "aabb", "smoothstep", "bezier",
        "point_in_poly", "convex", "ease", "bounce", "line_points", "turn",
        "fsm", "book", "svc_", "robot", "edge", "game_loop", "dist_stats",
    ];
    let mut use_count: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            let src = crate::fmt::format_program(&e.body);
            for b in BUILTINS { if src.contains(b) { *use_count.entry(b).or_default() += 1; } }
        }
    }
    let mut usage_list: Vec<(&str, usize)> = use_count.into_iter().collect();
    usage_list.sort_by(|a, b| b.1.cmp(&a.1));
    Json(serde_json::json!({"builtin_usage": usage_list.iter()
        .map(|(b, c)| serde_json::json!({"builtin": b, "count": c}))
        .collect::<Vec<_>>()}))
}

#[derive(Deserialize)]
struct CallRequest {
    name: String,
    variant: Option<u32>,
    args: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    vm: bool,
}

async fn call_fn(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CallRequest>,
) -> Json<serde_json::Value> {
    let veldt = { state.veldt.lock().unwrap().clone() };
    let mut interp = crate::interpreter::Interpreter::new();
    interp.caps = crate::interpreter::Caps::sandboxed();
    veldt.load_into_interpreter(&mut interp);
    let args: Vec<crate::interpreter::Value> = (req.args.unwrap_or_default())
        .iter().filter_map(|a| serde_json::from_value(a.clone()).ok()).collect();
    if req.vm {
        // fast-engine call — jit_call dispatches natively when compiled
        let mut v = crate::vm::Vm::new();
        let _ = v.load_garden(&veldt);
        match v.jit_call(&req.name, req.variant, args) {
            Ok(val) => return Json(serde_json::json!({"ok": true, "result": serde_json::to_value(&val).unwrap_or(serde_json::Value::Null)})),
            Err(e) => return Json(serde_json::json!({"ok": false, "error": e})),
        }
    }
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

// === 2D game sessions — decoupled server loop + browser viewport ===

#[derive(Deserialize)]
struct GameStartRequest { source: String, name: Option<String> }

/// /api/game/start — spawn a live game session. The veldt program
/// runs in its own thread with a GameIo attached; frames land in the
/// shared buffer, browser input flows back into `keys()`.
async fn game_start(
    State(state): State<Arc<AppState>>,
    Json(req): Json<GameStartRequest>,
) -> Json<serde_json::Value> {
    let id = state.next_game.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let io = crate::interpreter::GameIo::new();
    state.games.lock().unwrap().insert(id, io.clone());
    // snapshot the garden — the session's grown fns resolve without
    // holding the veldt lock for the session's lifetime
    let mut veldt = state.veldt.lock().unwrap().clone();
    let cleanup = Arc::clone(&state);
    std::thread::spawn(move || {
        let _ = crate::agent::execute_source_game(&req.source, &mut veldt, io.clone());
        io.running.store(false, std::sync::atomic::Ordering::Relaxed);
        // leave the last frame readable; drop the session after a beat
        std::thread::sleep(std::time::Duration::from_secs(30));
        cleanup.games.lock().unwrap().remove(&id);
    });
    Json(serde_json::json!({ "ok": true, "id": id,
        "name": req.name.unwrap_or_default() }))
}

/// /api/game/frame?id=N — latest emitted scene JSON, or running:false
async fn game_frame(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let id: u64 = q.get("id").and_then(|s| s.parse().ok()).unwrap_or(0);
    let games = state.games.lock().unwrap();
    match games.get(&id) {
        Some(io) => {
            let frame = io.frame.lock().unwrap().clone();
            Json(serde_json::json!({
                "ok": true, "id": id,
                "running": io.running.load(std::sync::atomic::Ordering::Relaxed),
                "frame": frame,
            }))
        }
        None => Json(serde_json::json!({ "ok": false, "error": "no such session", "running": false })),
    }
}

#[derive(Deserialize)]
struct GameInputRequest { id: u64, keys: Vec<String> }

/// /api/game/input — replace the session's held-key set (the browser
/// POSTs the current keydown set each poll).
async fn game_input(
    State(state): State<Arc<AppState>>,
    Json(req): Json<GameInputRequest>,
) -> Json<serde_json::Value> {
    let games = state.games.lock().unwrap();
    match games.get(&req.id) {
        Some(io) => {
            *io.keys.lock().unwrap() = req.keys;
            Json(serde_json::json!({ "ok": true }))
        }
        None => Json(serde_json::json!({ "ok": false, "error": "no such session" })),
    }
}

#[derive(Deserialize)]
struct GameStopRequest { id: u64 }

/// /api/game/stop — end the session (game_running() → false next tick)
async fn game_stop(
    State(state): State<Arc<AppState>>,
    Json(req): Json<GameStopRequest>,
) -> Json<serde_json::Value> {
    let games = state.games.lock().unwrap();
    match games.get(&req.id) {
        Some(io) => {
            io.running.store(false, std::sync::atomic::Ordering::Relaxed);
            Json(serde_json::json!({ "ok": true }))
        }
        None => Json(serde_json::json!({ "ok": false, "error": "no such session" })),
    }
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
struct CheckRequest { source: String, strict: Option<bool> }

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
            let warnings = if req.strict.unwrap_or(false) {
                crate::lint(&stmts)
            } else {
                Vec::new()
            };
            Json(serde_json::json!({"ok": true, "warnings": warnings}))
        }
        Err(e) => Json(serde_json::json!({"ok": false, "error": e})),
    }
}

async fn fix_source(
    Json(req): Json<CheckRequest>,
) -> Json<serde_json::Value> {
    // mechanical dead-code repair on the posted source — the
    // IDE's one-click fix (same passes as veldt fix / --garden --fix)
    let mut lx = crate::lexer::Lexer::new(&req.source);
    let res = lx.tokenize().map_err(|e| format!("lex: {}", e))
        .and_then(|t| {
            let mut p = crate::parser::Parser::new_with_lines(t, lx.token_lines.clone());
            p.parse_program().map_err(|e| format!("parse: {}", e))
        });
    match res {
        Ok(mut stmts) => {
            let changed = crate::health::HealthChecker::repair_program(&mut stmts);
            Json(serde_json::json!({
                "ok": true, "changed": changed,
                "source": if changed { crate::fmt::format_program(&stmts) }
                          else { req.source },
            }))
        }
        Err(e) => Json(serde_json::json!({"ok": false, "error": e})),
    }
}

async fn garden_lint(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    // strict-lint every variant body — per-family warning counts
    // with the defect-class split, for the IDE's code-health view
    let veldt = state.veldt.lock().unwrap();
    let mut per_fam = Vec::new();
    let (mut total, mut bugs) = (0usize, 0usize);
    for (name, vs) in &veldt.functions {
        let (mut fw, mut fb) = (0usize, 0usize);
        for e in vs {
            let wrapped = crate::ast::Stmt::Grow(crate::ast::GrowItem::Fn(
                name.clone(), e.params.clone(),
                e.body.clone(), e.tests.clone()));
            for w in crate::lint(&[wrapped]) {
                if w.contains("defined but never called")
                    || w.contains("no top-level statements") { continue; }
                fw += 1;
                if crate::evolve::is_realbug_warning(&w) { fb += 1; }
            }
        }
        if fw > 0 {
            per_fam.push(serde_json::json!({
                "family": name, "warnings": fw, "defects": fb}));
        }
        total += fw;
        bugs += fb;
    }
    per_fam.sort_by(|a, b| b["warnings"].as_u64().cmp(&a["warnings"].as_u64()));
    Json(serde_json::json!({
        "warnings": total, "defects": bugs, "families": per_fam}))
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
    #[serde(default)]
    vm: bool,
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

    let result = agent::execute_source_opts_full(&req.source, &mut veldt,
        if req.sandbox { crate::interpreter::Caps::sandboxed() }
        else { crate::interpreter::Caps::allow_all() }, req.vm);

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
    #[serde(default)]
    vm: bool,
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

    veldt.prefer_vm = req.vm;
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
    let veldt = {
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

async fn champions() -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let champs: Vec<serde_json::Value> = veldt.functions.iter()
        .filter_map(|(n, vs)| vs.iter()
            .max_by(|a, b| a.fitness.partial_cmp(&b.fitness)
                .unwrap_or(std::cmp::Ordering::Equal))
            .map(|e| serde_json::json!({
                "family": n, "champion": format!("{}#{}", n, e.id),
                "fitness": e.fitness, "health": format!("{:?}", e.health),
                "trust": format!("{:?}", e.trust),
            })))
        .collect();
    Json(serde_json::json!({"champions": champs}))
}

async fn summary() -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let total: usize = veldt.functions.values().map(|vs| vs.len()).sum();
    let (mut h, mut s, mut d) = (0usize, 0usize, 0usize);
    let (mut seed, mut estab, mut canon) = (0usize, 0usize, 0usize);
    for vs in veldt.functions.values() {
        for e in vs {
            match e.health {
                crate::veldt::Health::Healthy => h += 1,
                crate::veldt::Health::Sick(_) => s += 1,
                crate::veldt::Health::Dying => d += 1,
            }
            match e.trust {
                crate::veldt::Trust::Seedling => seed += 1,
                crate::veldt::Trust::Established => estab += 1,
                crate::veldt::Trust::Canonical => canon += 1,
            }
        }
    }
    let mut fam: Vec<(&String, f64)> = veldt.functions.iter()
        .map(|(n, vs)| (n, vs.iter().map(|e| e.fitness)
            .fold(f64::NEG_INFINITY, f64::max))).collect();
    fam.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Json(serde_json::json!({
        "lineages": veldt.functions.len(), "variants": total,
        "healthy": h, "sick": s, "dying": d,
        "seedling": seed, "established": estab, "canonical": canon,
        "probes": veldt.probes.values().map(|v| v.len()).sum::<usize>(),
        "deaths": veldt.obituaries.len(), "runs": veldt.run_count,
        "top_families": fam.iter().take(5).map(|(n, f)| serde_json::json!({"name": n, "peak": f})).collect::<Vec<_>>(),
        "fitness_trend": veldt.fitness_history,
    }))
}

async fn why_variant(
    axum::extract::Query(req): axum::extract::Query<VariantRequest>,
) -> Json<serde_json::Value> {
    let task = format!("why {}#{}", req.name, req.id);
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let answer = crate::agent::respond(&task, "", &mut veldt);
    Json(serde_json::json!({"diagnosis": answer}))
}

async fn species() -> Json<serde_json::Value> {
    let specs: Vec<serde_json::Value> = crate::agent::catalog().iter()
        .map(|t| serde_json::json!({
            "name": t.canonical, "desc": t.desc,
            "keywords": t.keywords, "needs": t.needs,
        })).collect();
    Json(serde_json::json!({"species": specs}))
}

#[derive(Deserialize)]
struct FindRequest { q: String }

async fn find_family(
    axum::extract::Query(req): axum::extract::Query<FindRequest>,
) -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let hits: Vec<serde_json::Value> = veldt.functions.keys()
        .filter(|n| n.contains(&req.q))
        .map(|n| serde_json::json!({
            "name": n, "variants": veldt.functions[n].len(),
            "probes": veldt.probes.get(n).map(|p| p.len()).unwrap_or(0),
        })).collect();
    Json(serde_json::json!({"matches": hits}))
}

#[derive(Deserialize)]
struct DiffRequest { a: String, b: String }

async fn diff_variants(
    axum::extract::Query(req): axum::extract::Query<DiffRequest>,
) -> Json<serde_json::Value> {
    let parse = |s: &str| s.split_once('#')
        .and_then(|(n, i)| i.parse::<u32>().ok().map(|id| (n.to_string(), id)));
    let (a_name, a_id) = match parse(&req.a) { Some(x) => x, None =>
        return Json(serde_json::json!({"error": "invalid 'a' — expected <fam>#<id>"})) };
    let (b_name, b_id) = match parse(&req.b) { Some(x) => x, None =>
        return Json(serde_json::json!({"error": "invalid 'b' — expected <fam>#<id>"})) };
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let a_body = veldt.functions.get(&a_name)
        .and_then(|vs| vs.iter().find(|e| e.id == a_id))
        .map(|e| crate::fmt::format_program(&e.body));
    let b_body = veldt.functions.get(&b_name)
        .and_then(|vs| vs.iter().find(|e| e.id == b_id))
        .map(|e| crate::fmt::format_program(&e.body));
    Json(serde_json::json!({"a": a_body, "b": b_body}))
}

async fn obituary(
    axum::extract::Query(req): axum::extract::Query<VariantRequest>,
) -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let obit = veldt.obituaries.iter()
        .find(|o| o.name == req.name && o.id == req.id);
    match obit {
        Some(o) => Json(serde_json::json!({
            "name": o.name, "id": o.id, "age": o.age,
            "health": format!("{:?}", o.health), "cause": o.cause_of_death,
            "last_used": o.last_used, "healing_attempts": o.healing_attempts,
        })),
        None => Json(serde_json::json!({"error": format!("{}#{} not in obituaries", req.name, req.id)})),
    }
}

#[derive(Deserialize)]
struct TraceRequest { name: String }

async fn trace_family(
    axum::extract::Query(req): axum::extract::Query<TraceRequest>,
) -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let tr: Vec<serde_json::Value> = veldt.traces.get(&req.name)
        .map(|ts| ts.iter().map(|t| serde_json::json!({
            "input": format!("{:?}", t.args),
            "output": format!("{:?}", t.result),
            "timestamp": t.at,
        })).collect())
        .unwrap_or_default();
    Json(serde_json::json!({"name": req.name, "traces": tr}))
}

async fn probe_family(
    axum::extract::Query(req): axum::extract::Query<TraceRequest>,
) -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let pr: Vec<serde_json::Value> = veldt.probes.get(&req.name)
        .map(|ps| ps.iter().map(|p| serde_json::json!({
            "input": format!("{:?}", p.args),
            "expected": format!("{:?}", p.expected),
            "niche": p.niche,
        })).collect())
        .unwrap_or_default();
    Json(serde_json::json!({"name": req.name, "probes": pr}))
}

async fn family_detail(
    axum::extract::Query(req): axum::extract::Query<TraceRequest>,
) -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let vs = match veldt.functions.get(&req.name) {
        Some(v) => v,
        None => return Json(serde_json::json!({"error": format!("{} not found", req.name)})),
    };
    let sorted_dist = |f: fn(&crate::veldt::FunctionEntry) -> f64| {
        let mut v: Vec<f64> = vs.iter().map(f).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v
    };
    let variants: Vec<serde_json::Value> = vs.iter().map(|e| serde_json::json!({
        "id": e.id, "fitness": e.fitness, "health": format!("{:?}", e.health),
        "age": e.age, "usage": e.usage_count, "trust": format!("{:?}", e.trust),
        "origin": e.origin, "born": e.born, "lifespan": e.lifespan,
        "last_used": e.last_used, "trial_score": e.trial_score,
        "efficiency": e.efficiency, "parents": e.parents.len(),
        "healing_attempts": e.healing_attempts.len(),
        "niches": e.niche_scores,
    })).collect();
    let peak: f64 = vs.iter().map(|e| e.fitness).fold(f64::NEG_INFINITY, f64::max);
    let avg: f64 = vs.iter().map(|e| e.fitness).sum::<f64>() / vs.len().max(1) as f64;
    // health / trust / origin breakdowns
    let healthy = vs.iter().filter(|e| matches!(e.health, crate::veldt::Health::Healthy)).count();
    let sick_n = vs.iter().filter(|e| matches!(e.health, crate::veldt::Health::Sick(_))).count();
    let dying = vs.len() - healthy - sick_n;
    let seedling = vs.iter().filter(|e| matches!(e.trust, crate::veldt::Trust::Seedling)).count();
    let established = vs.iter().filter(|e| matches!(e.trust, crate::veldt::Trust::Established)).count();
    let canonical = vs.len() - seedling - established;
    let mut origins: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for e in vs.iter() { *origins.entry(e.origin.clone()).or_insert(0) += 1; }
    // niche avg scores
    let mut niche_tot: std::collections::HashMap<String, (f64, usize)> = std::collections::HashMap::new();
    for e in vs.iter() {
        for (n, s) in &e.niche_scores {
            let ent = niche_tot.entry(n.clone()).or_insert((0.0, 0));
            ent.0 += s; ent.1 += 1;
        }
    }
    let niche_avgs: serde_json::Map<String, serde_json::Value> = niche_tot.iter()
        .map(|(n, (t, c))| (n.clone(), serde_json::json!(t / *c as f64))).collect();
    // ages / turnover / deaths
    let min_age = vs.iter().map(|e| e.age).min().unwrap_or(0);
    let max_age = vs.iter().map(|e| e.age).max().unwrap_or(0);
    let avg_age = vs.iter().map(|e| e.age as f64).sum::<f64>() / vs.len().max(1) as f64;
    let deaths = veldt.obituaries.iter().filter(|o| o.name == req.name).count();
    let traces = veldt.traces.get(&req.name).map(|t| t.len()).unwrap_or(0);
    // champion / weakest
    let champ = vs.iter().max_by(|a, b| a.fitness.partial_cmp(&b.fitness)
        .unwrap_or(std::cmp::Ordering::Equal));
    let weak = vs.iter().min_by(|a, b| a.fitness.partial_cmp(&b.fitness)
        .unwrap_or(std::cmp::Ordering::Equal));
    // lifecycle / workload / diversity / pressure / proven-risky — CLI parity
    let growing = vs.iter().filter(|e| (e.age as f64) < e.lifespan).count();
    let overworked = vs.iter().filter(|e| e.usage_count as f64 > e.lifespan).count();
    let unique_origins: std::collections::HashSet<_> = vs.iter().map(|e| e.origin.as_str()).collect();
    let unique_niches: std::collections::HashSet<_> = vs.iter()
        .flat_map(|e| e.niche_scores.keys().map(|s| s.as_str())).collect();
    let mean_fit = avg;
    let fit_std = if vs.len() > 1 {
        (vs.iter().map(|e| (e.fitness - mean_fit).powi(2)).sum::<f64>() / vs.len() as f64).sqrt()
    } else { 0.0 };
    let proven = vs.iter().filter(|e| matches!(e.health, crate::veldt::Health::Healthy)
        && matches!(e.trust, crate::veldt::Trust::Established | crate::veldt::Trust::Canonical)).count();
    let risky = vs.iter().filter(|e| matches!(e.health, crate::veldt::Health::Sick(_))
        && matches!(e.trust, crate::veldt::Trust::Seedling)).count();
    let healed = vs.iter().filter(|e| !e.healing_attempts.is_empty()).count();
    let healed_ok = vs.iter().filter(|e| !e.healing_attempts.is_empty()
        && matches!(e.health, crate::veldt::Health::Healthy)).count();
    let immigrants = vs.iter().filter(|e| e.origin.contains("immigrant")).count();
    let llm_bred = vs.iter().filter(|e| e.origin.contains("llm")).count();
    let fit_rate_avg = vs.iter().map(|e| e.fitness / (e.age as f64 + 1.0)).sum::<f64>()
        / vs.len().max(1) as f64;
    let death_causes: std::collections::HashMap<String, usize> = veldt.obituaries.iter()
        .filter(|o| o.name == req.name)
        .fold(std::collections::HashMap::new(), |mut m, o| {
            *m.entry(o.cause_of_death.clone()).or_insert(0) += 1; m
        });
    Json(serde_json::json!({
        "name": req.name, "variants": variants,
        "probes": veldt.probes.get(&req.name).map(|p| p.len()).unwrap_or(0),
        "traces": traces, "deaths": deaths,
        "peak_fitness": peak, "avg_fitness": avg,
        "healthy": healthy, "sick": sick_n, "dying": dying,
        "seedling": seedling, "established": established, "canonical": canonical,
        "origins": origins, "niche_averages": niche_avgs,
        "min_age": min_age, "max_age": max_age, "avg_age": avg_age,
        "champion": champ.map(|e| serde_json::json!({"id": e.id, "fitness": e.fitness})),
        "weakest": weak.map(|e| serde_json::json!({"id": e.id, "fitness": e.fitness})),
        "growing": growing, "aging": vs.len() - growing,
        "overworked": overworked, "underused": vs.len() - overworked,
        "unique_origins": unique_origins.len(), "unique_niches": unique_niches.len(),
        "fitness_stddev": fit_std, "rivals": vs.len().saturating_sub(1),
        "proven": proven, "risky": risky,
        "healed": healed, "healed_healthy": healed_ok,
        "immigrants": immigrants, "llm_bred": llm_bred,
        "fit_rate_avg": fit_rate_avg, "death_causes": death_causes,
        "fitness_dist": sorted_dist(|e| e.fitness),
        "age_dist": sorted_dist(|e| e.age as f64),
        "usage_dist": sorted_dist(|e| e.usage_count as f64),
        "size_dist": sorted_dist(|e| crate::fmt::format_program(&e.body).len() as f64),
        "eff_dist": sorted_dist(|e| e.efficiency),
    }))
}

async fn sick() -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let sick_list: Vec<serde_json::Value> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter()
            .filter(|e| matches!(e.health, crate::veldt::Health::Sick(_)))
            .map(|e| {
                let fails = if let crate::veldt::Health::Sick(f) = &e.health {
                    vec![serde_json::json!({"error": f})]
                } else { Vec::new() };
                serde_json::json!({
                    "variant": format!("{}#{}", n, e.id), "fitness": e.fitness,
                    "fails": fails,
                })
            }).collect::<Vec<_>>())
        .collect();
    Json(serde_json::json!({"sick": sick_list}))
}

async fn vars() -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let var_data: Vec<serde_json::Value> = veldt.variables.iter()
        .map(|(n, v)| serde_json::json!({"name": n, "value": format!("{:?}", v)}))
        .collect();
    Json(serde_json::json!({"variables": var_data}))
}

async fn structs() -> Json<serde_json::Value> {
    let mut veldt = Veldt::new();
    if veldt.load().is_err() {
        return Json(serde_json::json!({"error": "garden load failed"}));
    }
    let struct_data: Vec<serde_json::Value> = veldt.structs.iter()
        .map(|(n, s)| serde_json::json!({"name": n, "fields": s.fields.len()}))
        .collect();
    Json(serde_json::json!({"structs": struct_data}))
}

// --- IDE HTML ---

async fn serve_ide() -> Html<&'static str> {
    Html(IDE_HTML)
}

const IDE_HTML: &str = include_str!("ide.html");
const SITE_HTML: &str = include_str!("site.html");
const PLAY_HTML: &str = include_str!("play.html");

/// /play/<name> — the real-time game viewport. The page fetches the
/// source, starts a /api/game session, streams input in, and paints
/// the latest frame at ~20fps.
async fn serve_play() -> Html<&'static str> {
    Html(PLAY_HTML)
}

/// /browse — index of deployed programs: every examples/*.veldt with
/// links to its /site (watch) and /play (real-time) view.
async fn serve_browse() -> Html<String> {
    let mut names: Vec<String> = std::fs::read_dir("examples")
        .map(|rd| rd.flatten()
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .filter(|n| n.ends_with(".veldt"))
            .map(|n| n.trim_end_matches(".veldt").to_string())
            .collect())
        .unwrap_or_default();
    names.sort();
    names.dedup();
    let items = names.iter().map(|n| format!(
        "<li><a href='/play/{n}'>{n}</a><span class='links'><a href='/site/{n}'>site</a><a href='/examples/{n}.veldt'>src</a></span></li>",
        n = n)).collect::<Vec<_>>().join("\n");
    Html(format!(r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>veldt — deployed</title>
<style>
body{{background:#050807;color:#b8c8be;font-family:'JetBrains Mono',monospace;padding:40px;max-width:600px;margin:auto}}
h1{{font-family:'Cormorant Garamond',serif;color:#8ec5a0;font-size:32px}}
em{{color:#c9a96e}}
ul{{list-style:none;padding:0}}
li{{display:flex;justify-content:space-between;align-items:center;padding:9px 0;border-bottom:1px solid #1a2721}}
li a{{color:#6fae7f;text-decoration:none;font-size:15px}}
li a:hover{{color:#8ec5a0}}
.links a{{font-size:11px;color:#5a6b62;margin-left:14px;border:1px solid #1a2721;padding:2px 8px;border-radius:3px}}
.links a:hover{{color:#c9a96e;border-color:#2a3d33}}
.count{{color:#5a6b62;font-size:12px;margin-bottom:20px}}
</style></head><body>
<h1>Veldt <em>Deployed</em></h1>
<div class="count">{count} programs · click to play live</div>
<ul>{items}</ul></body></html>"#, count = names.len(), items = items))
}

/// /site/<name> — the deployed-program page. The page reads the
/// program name from its own URL and runs it via /api/play.
async fn serve_site() -> Html<&'static str> {
    Html(SITE_HTML)
}

/// /examples/<file> — raw program source (View Source + IDE openFile).
async fn serve_example(
    axum::extract::Path(name): axum::extract::Path<String>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    // path hygiene — no traversal
    if !name.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '_' || c == '-') {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    let path = format!("examples/{}", name);
    match std::fs::read_to_string(&path) {
        Ok(src) => ([("content-type", "text/plain; charset=utf-8")], src).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "no such file").into_response(),
    }
}

#[derive(Deserialize)]
struct DeployRequest { name: String, source: String }

/// /api/deploy — "vercel for veldt": POST a program, it's live at
/// /site/<name> on this server. The garden is shared, so organisms
/// the program uses resolve against the server's veldt.
async fn deploy_project(
    Json(req): Json<DeployRequest>,
) -> Json<serde_json::Value> {
    // name hygiene — file-path safe
    let clean: String = req.name.trim_end_matches(".veldt")
        .chars().filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if clean.is_empty() {
        return Json(serde_json::json!({ "ok": false, "error": "need a name (a-z 0-9 _ -)" }));
    }
    let path = format!("examples/{}.veldt", clean);
    if let Err(e) = std::fs::write(&path, &req.source) {
        return Json(serde_json::json!({ "ok": false, "error": format!("write: {}", e) }));
    }
    Json(serde_json::json!({
        "ok": true,
        "site": format!("/site/{}", clean),
        "file": path,
    }))
}
