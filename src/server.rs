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
        "builtin_usage": use_count.iter().map(|(b, c)| serde_json::json!({(*b): c})).collect::<Vec<_>>(),
        "fitness_by_source": fit_by_source.iter().map(|(s, fits)| serde_json::json!({
            s.clone(): fits.iter().sum::<f64>() / fits.len().max(1) as f64,
        })).collect::<Vec<_>>(),
        "mutation_success": mut_rate.iter().map(|(s, (h, t))| serde_json::json!({
            s.clone(): *h as f64 / (*t).max(1) as f64,
        })).collect::<Vec<_>>(),
        "avg_niche_scores": niche_sum.iter().map(|(n, (s, c))| serde_json::json!({
            n.clone(): *s / ((*c).max(1) as f64),
        })).collect::<Vec<_>>(),
        "probe_coverage": v.probes.iter().map(|(n, p)| serde_json::json!({n.clone(): p.len()})).collect::<Vec<_>>(),
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
