// Veldt Studio IDE — web server

use crate::ast::GrowItem;
use crate::evolve::{EvolutionEvent, Evolver};
use crate::health::HealthChecker;
use crate::interpreter::Interpreter;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::veldt::{Health, Veldt};
use axum::{
    extract::State,
    http::StatusCode,
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
    let mut output = String::new();
    let mut events = Vec::new();
    let mut success = true;

    // Lex
    let mut lexer = Lexer::new(&req.source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(e) => {
            return Json(RunResponse {
                output: String::new(),
                events: vec![format!("Lex error: {}", e)],
                success: false,
            });
        }
    };

    // Parse
    let mut parser = Parser::new(tokens);
    let stmts = match parser.parse_program() {
        Ok(s) => s,
        Err(e) => {
            return Json(RunResponse {
                output: String::new(),
                events: vec![format!("Parse error: {}", e)],
                success: false,
            });
        }
    };

    // Load veldt and run
    let mut veldt = {
        let v = state.veldt.lock().unwrap();
        v.clone()
    };

    let mut interp = Interpreter::new();
    veldt.load_into_interpreter(&mut interp);

    // Run with output capture
    let run_result = run_with_capture(&mut interp, &stmts, &mut output);

    if let Err(e) = run_result {
        output.push_str(&format!("Runtime error: {}\n", e));
        success = false;
    }

    // Record usage
    veldt.record_usage(&interp.usage_log);

    // Process grows
    let grows = interp.collect_grows(&stmts);
    for item in &grows {
        match item {
            GrowItem::Fn(name, params, body, tests) => {
                let source = format!("fn {}(...) {{ ... }}", name);
                let id = veldt.grow_function(name, params.clone(), body.clone(), tests.clone(), &source);
                let mut entry = veldt.functions[name].iter()
                    .find(|e| e.id == id).cloned().unwrap();
                HealthChecker::check_at_birth(&mut entry, &veldt);
                if let Some(variants) = veldt.functions.get_mut(name) {
                    for e in variants.iter_mut() {
                        if e.id == id { *e = entry.clone(); break; }
                    }
                }
                let health_str = match &entry.health {
                    Health::Healthy => "healthy".into(),
                    Health::Sick(e) => format!("sick: {}", e),
                    Health::Dying => "dying".into(),
                };
                events.push(format!("[BIRTH] {}#{} — {} (trial: {:.2})", name, id, health_str, entry.trial_score));
            }
            GrowItem::Let(name, expr) => {
                if let Ok(val) = interp.eval_expr_in_scope(expr) {
                    veldt.grow_variable(name, &val);
                    events.push(format!("[GROW] ${} planted", name));
                }
            }
            GrowItem::Struct(name, fields) => {
                veldt.grow_struct(name, fields.clone());
                events.push(format!("[GROW] struct {} planted", name));
            }
        }
    }

    // Age and reap
    let obituaries = veldt.age_and_reap();
    for obit in &obituaries {
        events.push(format!("[OBITUARY] {}#{} — {} (age {})", obit.name, obit.id, obit.cause_of_death, obit.age));
    }

    // Save veldt
    veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));

    // Update shared state
    {
        let mut v = state.veldt.lock().unwrap();
        *v = veldt;
    }

    Json(RunResponse { output, events, success })
}

fn run_with_capture(interp: &mut Interpreter, stmts: &[crate::ast::Stmt], output: &mut String) -> Result<(), String> {
    interp.print_buffer = Some(String::new());
    let result = interp.run(stmts);
    if let Some(buf) = interp.print_buffer.take() {
        output.push_str(&buf);
    }
    result
}

#[derive(Deserialize)]
struct EvolveRequest {
    cycles: usize,
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
        "run_count": veldt.run_count,
    })
}

// --- IDE HTML ---

async fn serve_ide() -> Html<&'static str> {
    Html(IDE_HTML)
}

const IDE_HTML: &str = include_str!("ide.html");
