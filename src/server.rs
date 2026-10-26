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
