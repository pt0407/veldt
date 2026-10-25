// Veldt CLI — main entry point

mod agent;
mod ast;
mod evolve;
mod fmt;
mod health;
mod interpreter;
mod lexer;
mod llm;
mod parser;
mod server;
mod veldt;
mod visual;

use std::env;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage();
        return;
    }

    let command = &args[1];

    // Studio needs async runtime
    if command == "studio" {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let port: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(7777);
            server::start_server(port).await;
        });
        return;
    }

    match command.as_str() {
        "run" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt run <file.veldt>");
                process::exit(1);
            }
            if args[2] == "-e" {
                // veldt run -e "code" — inline eval via temp file
                let code = args[3..].iter().take_while(|a| a.as_str() != "--").cloned().collect::<Vec<_>>().join(" ");
                let tmp = std::env::temp_dir().join(format!("veldt-eval-{}.veldt", std::process::id()));
                std::fs::write(&tmp, &code).unwrap_or_else(|e| { eprintln!("eval temp write: {}", e); process::exit(1); });
                run_program(tmp.to_str().unwrap(), args.iter().any(|a| a == "--trace"), args.iter().any(|a| a == "--evolve"));
                let _ = std::fs::remove_file(&tmp);
            } else {
                run_program(&args[2], args.iter().any(|a| a == "--trace"), args.iter().any(|a| a == "--evolve"));
            }
        }
        "--version" | "-V" | "version" => {
            println!("veldt {}", env!("CARGO_PKG_VERSION"));
        }
        "new" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt new <dir>");
                process::exit(1);
            }
            scaffold(&args[2]);
        }
        "fmt" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt fmt <file.veldt> [--write]");
                process::exit(1);
            }
            fmt_file(&args[2], args.iter().any(|a| a == "--write" || a == "-w"));
        }
        "repl" => {
            start_repl();
        }
        "test" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt test <name>");
                process::exit(1);
            }
            test_family(&args[2]);
        }
        "adopt" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt adopt <name#N>");
                process::exit(1);
            }
            adopt_variant(&args[2]);
        }
        "export" => {
            let target = args.get(2).map(|s| s.as_str()).unwrap_or("veldt-export.json");
            export_garden(target);
        }
        "import" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt import <file>");
                process::exit(1);
            }
            import_garden(&args[2]);
        }
        "garden" => {
            let mut veldt = veldt::Veldt::new();
            if let Err(e) = veldt.load() {
                eprintln!("Error loading veldt: {}", e);
                process::exit(1);
            }
            let sick_only = args.iter().any(|a| a == "--sick");
            let obituaries = args.iter().any(|a| a == "--obituaries");
            if obituaries {
                veldt.print_obituaries();
            } else {
                println!("=== The Veldt ===");
                veldt.print_garden(sick_only);
            }
        }
        "lineage" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt lineage <name>");
                process::exit(1);
            }
            let mut veldt = veldt::Veldt::new();
            if let Err(e) = veldt.load() {
                eprintln!("Error loading veldt: {}", e);
                process::exit(1);
            }
            veldt.print_lineage(&args[2]);
        }
        "prune" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt prune <name#N>");
                process::exit(1);
            }
            let mut veldt = veldt::Veldt::new();
            if let Err(e) = veldt.load() {
                eprintln!("Error loading veldt: {}", e);
                process::exit(1);
            }
            // parse name#N
            let target = &args[2];
            if let Some(pos) = target.find('#') {
                let name = &target[..pos];
                let id: u32 = target[pos+1..].parse().unwrap_or(0);
                if veldt.prune(name, id) {
                    println!("Pruned {}#{}", name, id);
                    veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));
                } else {
                    println!("{}#{} not found", name, id);
                }
            } else {
                eprintln!("Usage: veldt prune <name#N>");
                process::exit(1);
            }
        }
        "agent" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt agent \"task\"");
                process::exit(1);
            }
            // optional trailing .veldt file acts as the "editor" source
            let file_arg = args[2..].iter().find(|a| a.ends_with(".veldt"));
            let source = file_arg
                .and_then(|f| std::fs::read_to_string(f).ok())
                .unwrap_or_default();
            let task: Vec<&str> = args[2..].iter()
                .filter(|a| !a.ends_with(".veldt"))
                .map(|s| s.as_str())
                .collect();
            let task = task.join(" ");
            if task.is_empty() {
                eprintln!("Usage: veldt agent \"task\" [file.veldt]");
                process::exit(1);
            }
            let mut veldt = veldt::Veldt::new();
            if let Err(e) = veldt.load() {
                eprintln!("Error loading veldt: {}", e);
                process::exit(1);
            }
            let resp = agent::respond(&task, &source, &mut veldt);
            if args.iter().any(|a| a == "--json") {
                println!("{}", serde_json::to_string(&serde_json::json!({
                    "steps": resp.steps.iter().map(|s| serde_json::json!({"kind": s.kind, "text": s.text})).collect::<Vec<_>>(),
                    "reply": resp.reply,
                    "code": resp.code,
                    "ran": resp.ran,
                    "output": resp.output,
                    "success": resp.success,
                    "events": resp.events,
                })).unwrap_or_else(|_| "{}".into()));
                veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));
                return;
            }
            for s in &resp.steps {
                let marker = match s.kind {
                    "plan" => ">", "act" => "*", "obs" => "-", "ok" => "+", "err" => "x",
                    _ => " ",
                };
                eprintln!("{} {}", marker, s.text);
            }
            println!("{}", resp.reply);
            if let Some(code) = &resp.code {
                println!("\n{}", code);
            }
            if let Some(out) = &resp.output {
                if !out.trim().is_empty() {
                    println!("{}", out.trim_end());
                }
            }
            veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));
        }
        "bench" => {
            let mut veldt = veldt::Veldt::new();
            if let Err(e) = veldt.load() {
                eprintln!("Error loading veldt: {}", e);
                process::exit(1);
            }
            let (report, _) = agent::bench_report(&veldt);
            println!("{}", report);
        }
        "clear" => {
            let mut veldt = veldt::Veldt::new();
            veldt.clear();
            veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));
            println!("The veldt has been cleared.");
        }
        "evolve" => {
            let cycles: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
            let mut veldt = veldt::Veldt::new();
            if let Err(e) = veldt.load() {
                eprintln!("Error loading veldt: {}", e);
                process::exit(1);
            }
            let mut evolver = evolve::Evolver::new();
            evolver.llm = args.iter().any(|a| a == "--llm");
            let events = evolver.evolve(&mut veldt, cycles);
            for event in &events {
                eprintln!("{}", event.format());
            }
            veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));
        }
        "visual" => {
            let mut veldt = veldt::Veldt::new();
            if let Err(e) = veldt.load() {
                eprintln!("Error loading veldt: {}", e);
                process::exit(1);
            }
            match visual::generate_visual(&veldt) {
                Ok(path) => {
                    println!("Visual generated at: {}", path);
                    visual::open_in_browser(&path);
                }
                Err(e) => {
                    eprintln!("Error generating visual: {}", e);
                    process::exit(1);
                }
            }
        }
        "help" | "--help" | "-h" => {
            print_usage();
        }
        _ => {
            eprintln!("Unknown command: {}", command);
            print_usage();
            process::exit(1);
        }
    }
}

fn scaffold(dir: &str) {
    let path = std::path::Path::new(dir);
    if path.exists() {
        eprintln!("Directory '{}' already exists", dir);
        process::exit(1);
    }
    std::fs::create_dir_all(path).unwrap_or_else(|e| {
        eprintln!("Cannot create '{}': {}", dir, e);
        process::exit(1);
    });
    std::fs::write(path.join("main.veldt"), r#"fn main() {
    print("hello — code that grows")
    let a = args()
    if a.len() > 0 {
        print("args: " + a.join(", "))
    }
}

main()
"#).unwrap();
    std::fs::write(path.join("README.md"),
        format!("# {}\n\nveldt app — `veldt run main.veldt` to start\n", dir)).unwrap();
    println!("Created {} — run: veldt run {}/main.veldt", dir, dir);
}

fn print_usage() {
    println!("Veldt v0.1.0 — a language where code is alive");
    println!();
    println!("Usage:");
    println!("  veldt run <file.veldt>       Run a Veldt program");
    println!("  veldt garden                 List all living code in the veldt");
    println!("  veldt garden --sick          List only sick variants");
    println!("  veldt garden --obituaries    Show recent deaths");
    println!("  veldt lineage <name>         Show all variants of a function lineage");
    println!("  veldt prune <name#N>         Manually kill a specific variant");
    println!("  veldt clear                  Clear the entire veldt");
    println!("  veldt evolve [N]             Run N evolution cycles (garden self-grows)");
    println!("  veldt agent \"task\"          Ask the Gardener (rule-based coding agent)");
    println!("  veldt bench                  Benchmark the Gardener's species + intents");
    println!("  veldt repl                   Interactive REPL (:help for commands)");
    println!("  veldt run -e \"code\"          Inline eval; \"-- a b\" sets args()");
    println!("  veldt studio [port]          Start Veldt Studio IDE (default port 7777)
  veldt repl                   Interactive shell (persistent env)
  veldt fmt <file>             Print canonical formatted source (--write to overwrite)
  veldt test <name>            Re-trial all variants of a function family
  veldt adopt <name#N>         Canonicalize a variant (dispatch picks it)
  veldt export [file]          Export the garden to a JSON file
  veldt import <file>          Import a garden JSON file
  veldt run --trace <file>     Trace every executed statement
  veldt run --evolve <file>    Run 1 evolve cycle after the program");
    println!("  veldt visual                 Open visual ecosystem viewer in browser");
}

fn start_repl() {
    use std::io::{BufRead, Write};
    let mut veldt = veldt::Veldt::new();
    let _ = veldt.load();
    let mut interp = interpreter::Interpreter::new();
    veldt.load_into_interpreter(&mut interp);

    println!("veldt {} — repl. :help for commands, ctrl-d to quit", env!("CARGO_PKG_VERSION"));
    let stdin = std::io::stdin();
    let mut buf = String::new();
    let mut depth: i64 = 0;
    loop {
        print!("{}", if buf.is_empty() { "veldt> " } else { "  ... " });
        std::io::stdout().flush().ok();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => { println!(); break; }
            Ok(_) => {}
        }
        let trimmed = line.trim();
        if buf.is_empty() {
            if trimmed.is_empty() { continue; }
            if trimmed.starts_with(':') {
                match trimmed {
                    ":quit" | ":q" => break,
                    ":garden" => { let _ = veldt.load(); veldt.print_lineage(
                        veldt.functions.keys().next().map(|s| s.as_str()).unwrap_or("(none)")); }
                    ":help" | _ => {
                        println!(":help  this help"); println!(":quit  exit");
                        println!(":garden  show first lineage"); println!("exprs print their value");
                    }
                }
                continue;
            }
        }
        buf.push_str(&line);
        depth += line.matches('{').count() as i64 - line.matches('}').count() as i64;
        if depth > 0 { continue; }
        depth = 0;
        let src = buf.clone();
        buf.clear();

        let mut lx = lexer::Lexer::new(&src);
        let tokens = match lx.tokenize() { Ok(t) => t, Err(e) => { eprintln!("lex: {}", e); continue; } };
        let mut p = parser::Parser::new_with_lines(tokens, lx.token_lines.clone());
        let stmts = match p.parse_program() { Ok(s) => s, Err(e) => { eprintln!("parse: {}", e); continue; } };
        // print value of trailing bare expressions
        let n = stmts.len();
        for (i, stmt) in stmts.iter().enumerate() {
            match interp.exec_stmt_pub(stmt) {
                Ok(_) => {
                    if i + 1 == n {
                        if let ast::Stmt::ExprStmt(e) = stmt {
                            if let Ok(v) = interp.eval_expr_in_scope(e) {
                                println!("{}", v);
                            }
                        }
                    }
                }
                Err(e) => {
                    if e.starts_with("__veldt_exit__:") {
                        println!("bye");
                        return;
                    }
                    eprintln!("{}", e);
                    break;
                }
            }
        }
    }
}

fn fmt_file(path: &str, write: bool) {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => { eprintln!("Error reading {}: {}", path, e); process::exit(1); }
    };
    let mut lx = lexer::Lexer::new(&src);
    let tokens = match lx.tokenize() { Ok(t) => t, Err(e) => { eprintln!("{}", e); process::exit(1); } };
    let mut p = parser::Parser::new_with_lines(tokens, lx.token_lines.clone());
    let stmts = match p.parse_program() { Ok(s) => s, Err(e) => { eprintln!("{}", e); process::exit(1); } };
    let out = fmt::format_program(&stmts);
    if write {
        match std::fs::write(path, &out) {
            Ok(_) => println!("Formatted {}", path),
            Err(e) => eprintln!("Write failed: {}", e),
        }
    } else {
        print!("{}", out);
    }
}

fn test_family(name: &str) {
    let mut veldt = veldt::Veldt::new();
    if let Err(e) = veldt.load() { eprintln!("Error loading veldt: {}", e); process::exit(1); }
    let ids: Vec<u32> = veldt.functions.get(name).map(|vs| vs.iter().map(|e| e.id).collect()).unwrap_or_default();
    if ids.is_empty() { println!("No variants of '{}'", name); return; }
    println!("=== Trials for {} ===", name);
    for id in ids {
        let mut e = veldt.functions[name].iter().find(|e| e.id == id).cloned().unwrap();
        health::HealthChecker::check_at_birth(&mut e, &veldt);
        if let Some(vs) = veldt.functions.get_mut(name) {
            for v in vs.iter_mut() { if v.id == id { *v = e.clone(); break; } }
        }
        let health = match &e.health {
            veldt::Health::Healthy => "healthy".to_string(),
            veldt::Health::Sick(m) => format!("sick: {}", m),
            veldt::Health::Dying => "dying".into(),
        };
        println!("  {}#{} — {} | fitness: {:.2} | niches: {:?}",
            name, id, health, e.fitness, e.niche_scores);
    }
    veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));
}

fn adopt_variant(target: &str) {
    if let Some(pos) = target.find('#') {
        let name = &target[..pos];
        let id: u32 = target[pos+1..].parse().unwrap_or(0);
        let mut veldt = veldt::Veldt::new();
        if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
        if let Some(variants) = veldt.functions.get_mut(name) {
            let mut found = false;
            for v in variants.iter_mut() {
                if v.id == id { v.trust = veldt::Trust::Canonical; found = true; }
            }
            if found {
                veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));
                println!("Adopted {}#{} — now Canonical, dispatch prefers it", name, id);
            } else { println!("{}#{} not found", name, id); }
        } else { println!("No lineage '{}'", name); }
    } else { eprintln!("Usage: veldt adopt <name#N>"); }
}

fn export_garden(path: &str) {
    let mut veldt = veldt::Veldt::new();
    if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
    match std::fs::copy(veldt.garden_path.join("veldt.json"), path) {
        Ok(_) => println!("Garden exported to {}", path),
        Err(e) => eprintln!("Export failed: {}", e),
    }
}

fn import_garden(path: &str) {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let dest = std::path::PathBuf::from(home).join(".veldt").join("garden").join("veldt.json");
    if let Some(parent) = dest.parent() { let _ = std::fs::create_dir_all(parent); }
    match std::fs::copy(path, &dest) {
        Ok(_) => println!("Garden imported from {}", path),
        Err(e) => eprintln!("Import failed: {}", e),
    }
}

fn run_program(path: &str, trace: bool, evolve_after: bool) {
    // Read source file
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading file '{}': {}", path, e);
            process::exit(1);
        }
    };

    // Lex
    let mut lexer = lexer::Lexer::new(&source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Lex error: {}", e);
            process::exit(1);
        }
    };

    // Parse
    let lines = lexer.token_lines.clone();
    let mut parser = parser::Parser::new_with_lines(tokens, lines);
    let stmts = match parser.parse_program() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Parse error: {}", e);
            process::exit(1);
        }
    };
    let parser_lines = parser.statement_lines();

    // Load veldt from disk
    let mut veldt = veldt::Veldt::new();
    if let Err(e) = veldt.load() {
        eprintln!("Warning: could not load veldt: {}", e);
    }

    // Create interpreter and load veldt state into it
    let mut interp = interpreter::Interpreter::new();
    interp.trace = trace;
    interp.trace_calls = true;
    // script argv: everything after '--' in the veldt args
    let all_args: Vec<String> = env::args().collect();
    if let Some(pos) = all_args.iter().position(|a| a == "--") {
        interp.argv = all_args[pos+1..].to_vec();
    }
    veldt.load_into_interpreter(&mut interp);

    // Run the program
    let run_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || interp.run_with_lines(&stmts, &parser_lines)))
        .unwrap_or_else(|_| Err("interpreter panicked".into()));
    if let Err(e) = run_res {
        if let Some(code) = e.strip_prefix("__veldt_exit__")
            .and_then(|s| s.trim_start_matches(':').parse::<i32>().ok()) {
            process::exit(code);
        }
        let line = interp.current_line;
        if line > 0 {
            eprintln!("Runtime error (line {}): {}", line, e);
        } else {
            eprintln!("Runtime error: {}", e);
        }
        for (n, l) in interp.call_stack.iter().rev() {
            eprintln!("  called from line {} in {}", l, n);
        }
        // Still save veldt — the program may have grown code before failing
    }

    // Record usage from this run
    veldt.record_usage(&interp.usage_log);

    // Process grow statements
    let grows = interp.collect_grows(&stmts);
    for item in &grows {
        match item {
            ast::GrowItem::Fn(name, params, body, tests) => {
                let source = format!("fn {}(...) {{ ... }}", name);
                let id = veldt.grow_function(name, params.clone(), body.clone(), tests.clone(), &source);
                // Health check at birth — clone entry to avoid borrow conflict
                let mut entry = veldt.functions[name].iter()
                    .find(|e| e.id == id).cloned().unwrap();
                health::HealthChecker::check_at_birth(&mut entry, &veldt);
                // Put it back
                if let Some(variants) = veldt.functions.get_mut(name) {
                    for e in variants.iter_mut() {
                        if e.id == id {
                            *e = entry.clone();
                            break;
                        }
                    }
                }

                let health_str = match &entry.health {
                    veldt::Health::Healthy => "healthy".to_string(),
                    veldt::Health::Sick(e) => format!("sick: {}", e),
                    veldt::Health::Dying => "dying".into(),
                };
                eprintln!("[BIRTH] {}#{} — {} (trial score: {:.2})", name, id, health_str, entry.trial_score);
            }
            ast::GrowItem::Let(name, expr) => {
                // Evaluate the expression in the current interpreter scope
                if let Ok(val) = interp.eval_expr_in_scope(expr) {
                    veldt.grow_variable(name, &val);
                    eprintln!("[GROW] ${} — immortal variable planted", name);
                }
            }
            ast::GrowItem::Struct(name, fields) => {
                veldt.grow_struct(name, fields.clone());
                eprintln!("[GROW] struct {} — immortal struct planted", name);
            }
        }
    }

    // Heal sick functions
    let mut healed = Vec::new();
    // Heal sick functions — clone entries to avoid borrow conflicts
    let sick_entries: Vec<(String, u32)> = veldt.functions.iter()
        .flat_map(|(name, variants)| {
            variants.iter()
                .filter(|e| matches!(e.health, veldt::Health::Sick(_)))
                .map(move |e| (name.clone(), e.id))
        })
        .collect();

    for (name, id) in sick_entries {
        let mut entry = veldt.functions[&name].iter()
            .find(|e| e.id == id).cloned().unwrap();
        let was_healed = health::HealthChecker::heal(&mut entry, &veldt);
        if was_healed {
            healed.push(format!("{}#{}", name, id));
            // Put the healed entry back
            if let Some(variants) = veldt.functions.get_mut(&name) {
                for e in variants.iter_mut() {
                    if e.id == id {
                        *e = entry.clone();
                        break;
                    }
                }
            }
        }
    }
    for h in &healed {
        eprintln!("[HEALED] {} — hybrid merge successful", h);
    }

    // Age and reap
    let obituaries = veldt.age_and_reap();
    for obit in &obituaries {
        let health_str = match &obit.health {
            veldt::Health::Healthy => "healthy".to_string(),
            veldt::Health::Sick(e) => format!("sick ({})", e),
            veldt::Health::Dying => "dying".into(),
        };
        let last_used_str = if obit.last_used == 0 {
            "never".to_string()
        } else {
            format!("{} runs ago", veldt.run_count.saturating_sub(obit.last_used))
        };
        eprintln!("[OBITUARY] {}#{} — died at age {} ({})", obit.name, obit.id, obit.age, health_str);
        eprintln!("  Last used: {}", last_used_str);
        eprintln!("  Cause of death: {}", obit.cause_of_death);
    }

    // Auto-evolve: piggyback one cycle on program runs
    if evolve_after {
        let mut evolver = evolve::Evolver::new();
        for event in evolver.evolve(&mut veldt, 1) {
            eprintln!("{}", event.format());
        }
    }

    // Merge observed call traces into the garden (ground truth for trials)
    veldt.merge_traces(&interp.traces);

    // Save veldt to disk
    if let Err(e) = veldt.save() {
        eprintln!("Error saving veldt: {}", e);
    }
}
