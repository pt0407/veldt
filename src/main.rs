// Veldt CLI — main entry point

mod jit;
mod agent;
mod ast;
mod evolve;
mod fmt;
mod health;
mod interpreter;
mod lexer;
mod llm;
mod lsp;
mod parser;
mod server;
mod veldt;
mod vm;
mod visual;

use std::env;
use std::process;
use crate::veldt::Health;

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
                if args.iter().any(|a| a == "--vm") {
                    run_program_vm(tmp.to_str().unwrap());
                } else {
                    run_program(tmp.to_str().unwrap(), args.iter().any(|a| a == "--trace"), args.iter().any(|a| a == "--evolve"));
                }
                let _ = std::fs::remove_file(&tmp);
            } else if args.iter().any(|a| a == "--vm") {
                // Phase 11: bytecode VM executor (computational subset)
                let file = args[2..].iter().find(|a| a.ends_with(".veldt"))
                    .cloned().unwrap_or_else(|| args[2].clone());
                run_program_vm(&file);
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
                eprintln!("Usage: veldt fmt <file.veldt> [--write|--check]");
                process::exit(1);
            }
            if args.iter().any(|a| a == "--check") {
                fmt_check(&args[2]);
            } else {
                fmt_file(&args[2], args.iter().any(|a| a == "--write" || a == "-w"));
            }
        }
        "lsp" => { lsp::run_lsp(); }
        "watch" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt watch <file.veldt>");
                process::exit(1);
            }
            watch_file(&args[2]);
        }
        "repl" => {
            start_repl();
        }
        "test" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt test <name>|--all");
                process::exit(1);
            }
            if args[2] == "--all" {
                // re-trial every family — the CI sweep
                let mut veldt = veldt::Veldt::new();
                if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
                let names: Vec<String> = veldt.functions.keys().cloned().collect();
                for name in &names {
                    test_family(name);
                }
            } else {
                test_family(&args[2]);
            }
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
            if args.iter().any(|a| a == "--merge") {
                merge_garden_json(&read_garden_json(&args[2]));
            } else {
                import_garden(&args[2]);
            }
        }
        "registry" => {
            let port: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(7878);
            run_registry(port);
        }
        "push" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt push <url>   (posts garden to a registry)");
                process::exit(1);
            }
            push_garden(&args[2]);
        }
        "pull" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt pull <url> [--merge]  (fetch garden from a registry)");
                process::exit(1);
            }
            let url = format!("{}/api/garden", args[2].trim_end_matches('/'));
            if args.iter().any(|a| a == "--merge") {
                merge_garden(&url);
            } else {
                import_garden(&url);
            }
        }
        "swarm" => {
            // veldt swarm <peer> — bidirectional genome exchange:
            // pull their garden (merge immigrants), push ours back.
            if args.len() < 3 {
                eprintln!("Usage: veldt swarm <url>  (gossip: pull theirs, push ours)");
                process::exit(1);
            }
            let url = args[2].trim_end_matches('/').to_string();
            eprintln!("[SWARM] gossiping with {}", url);
            merge_garden(&format!("{}/api/garden", url));
            push_garden(&url);
            eprintln!("[SWARM] sync complete — both gardens merged");
        }
        "play" => {
            // veldt play <demo> — run a bundled game from examples/
            if args.len() < 3 {
                eprintln!("Usage: veldt play <demo>  (e.g. veldt play dungeon)");
                process::exit(1);
            }
            let path = format!("examples/{}.veldt", args[2]);
            if !std::path::Path::new(&path).exists() {
                eprintln!("No demo '{}' — try: dungeon", args[2]);
                process::exit(1);
            }
            run_program(&path, args.iter().any(|a| a == "--trace"),
                args.iter().any(|a| a == "--evolve"));
        }
        "tournament" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt tournament <url> [name]  (pull, trial, crown winners, push back)");
                process::exit(1);
            }
            tournament(&args[2], args.get(3).map(|s| s.as_str()));
        }
        "fuzz" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt fuzz <name>|--all  (boundary corpus vs every variant)");
                process::exit(1);
            }
            if args[2] == "--all" || args[2] == "--ci" {
                fuzz_all();
                // --ci: exit nonzero if any family diverged — CI gate
                if args[2] == "--ci" {
                    let mut veldt = veldt::Veldt::new();
                    let _ = veldt.load();
                    let bad = veldt.functions.values().flatten()
                        .filter(|e| e.niche_scores.values().any(|&s| s < 1.0))
                        .count();
                    if bad > 0 {
                        eprintln!("CI gate: {} variants have failing niches", bad);
                        process::exit(1);
                    }
                }
            } else {
                fuzz_family(&args[2]);
            }
        }
        "check" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt check <file.veldt>");
                process::exit(1);
            }
            // parse-only — no exec; exits 1 on any lex/parse error
            let source = std::fs::read_to_string(&args[2])
                .unwrap_or_else(|e| { eprintln!("Error reading {}: {}", args[2], e); process::exit(1); });
            let json_out = args.iter().any(|a| a == "--json");
            let strict = args.iter().any(|a| a == "--strict");
            let report_err = |kind: &str, e: &dyn std::fmt::Display| {
                if json_out {
                    println!("{}", serde_json::json!({"file": args[2], "ok": false, "error": format!("{}: {}", kind, e)}));
                } else {
                    eprintln!("{}: {}", kind, e);
                }
                process::exit(1);
            };
            let mut lx = lexer::Lexer::new(&source);
            match lx.tokenize() {
                Err(e) => report_err("Lex error", &e),
                Ok(toks) => {
                    let mut p = parser::Parser::new_with_lines(toks, lx.token_lines.clone());
                    match p.parse_program() {
                        Err(e) => report_err("Parse error", &e),
                        Ok(stmts) => {
                            if strict {
                                let warnings = lint(&stmts);
                                if json_out {
                                    println!("{}", serde_json::json!({"file": args[2], "ok": true, "statements": stmts.len(), "warnings": warnings}));
                                } else {
                                    for w in &warnings { eprintln!("  ⚠ {}", w); }
                                    println!("{} — ok ({} statements, {} warnings)", args[2], stmts.len(), warnings.len());
                                }
                            } else if json_out {
                                println!("{}", serde_json::json!({"file": args[2], "ok": true, "statements": stmts.len()}));
                            } else {
                                println!("{} — ok ({} statements)", args[2], stmts.len());
                            }
                        }
                    }
                }
            }
        }
        "diff" => {
            if args.len() < 4 {
                eprintln!("Usage: veldt diff <name#a> <name#b>");
                process::exit(1);
            }
            diff_variants(&args[2], &args[3]);
        }
        "absorb" => {
            absorb_traces(args.get(2).map(|s| s.as_str()));
        }
        "find" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt find <substr> [--json]  (find local families by name)");
                process::exit(1);
            }
            let mut veldt = veldt::Veldt::new();
            if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
            let needle = args[2].to_lowercase();
            let hits: Vec<_> = veldt.functions.iter()
                .filter(|(n, _)| n.to_lowercase().contains(&needle)).collect();
            if args.iter().any(|a| a == "--json") {
                println!("{}", serde_json::json!({
                    "matches": hits.iter().map(|(n, vs)| serde_json::json!({
                        "name": n, "variants": vs.len(),
                        "peak": vs.iter().map(|e| e.fitness).fold(f64::NEG_INFINITY, f64::max),
                    })).collect::<Vec<_>>(),
                }));
            } else {
                if hits.is_empty() {
                    println!("No families matching '{}'", args[2]);
                }
                for (name, vs) in hits {
                    let top = vs.iter().map(|e| e.fitness).fold(f64::NEG_INFINITY, f64::max);
                    println!("{} — {} variants, peak fitness {:.2}", name, vs.len(), top);
                }
            }
        }
        "search" => {
            if args.len() < 4 {
                eprintln!("Usage: veldt search <url> <name>  (find families in a registry)");
                process::exit(1);
            }
            search_registry(&args[2], &args[3]);
        }
        "garden" => {
            let mut veldt = veldt::Veldt::new();
            if let Err(e) = veldt.load() {
                eprintln!("Error loading veldt: {}", e);
                process::exit(1);
            }
            let sick_only = args.iter().any(|a| a == "--sick");
            let healthy_only = args.iter().any(|a| a == "--healthy");
            let obituaries = args.iter().any(|a| a == "--obituaries");
            let trust_filter = args.iter().position(|a| a == "--trust")
                .and_then(|p| args.get(p + 1));
            if let Some(t) = trust_filter {
                // ladder view — show only variants at a given trust tier
                let want = match t.to_lowercase().as_str() {
                    "seedling" | "s" => 0, "established" | "e" => 1,
                    "canonical" | "c" => 2, _ => { eprintln!("--trust seedling|established|canonical"); process::exit(1); }
                };
                for (name, vs) in &veldt.functions {
                    let tier: Vec<_> = vs.iter().filter(|e| match e.trust {
                        veldt::Trust::Seedling => want == 0,
                        veldt::Trust::Established => want == 1,
                        veldt::Trust::Canonical => want == 2,
                    }).collect();
                    if !tier.is_empty() {
                        println!("{}:", name);
                        for e in tier { println!("  #{} — fitness {:.2}  origin: {}", e.id, e.fitness, e.origin); }
                    }
                }
            } else if args.iter().any(|a| a == "--json") {
                // machine-readable census — registry/tooling consumers
                let fams: Vec<serde_json::Value> = veldt.functions.iter().map(|(name, vs)| {
                    serde_json::json!({
                        "name": name,
                        "variants": vs.iter().map(|e| serde_json::json!({
                            "id": e.id,
                            "health": format!("{:?}", e.health),
                            "fitness": e.fitness,
                            "trust": format!("{:?}", e.trust),
                            "niche_scores": e.niche_scores,
                            "origin": e.origin,
                        })).collect::<Vec<_>>(),
                        "probes": veldt.probes.get(name).map(|p| p.len()).unwrap_or(0),
                    })
                }).collect();
                println!("{}", serde_json::json!({
                    "families": fams,
                    "deaths": veldt.obituaries.len(),
                    "runs": veldt.run_count,
                }));
            } else if obituaries {
                veldt.print_obituaries();
            } else if let Some(fam) = args.iter().position(|a| a == "--family")
                .and_then(|p| args.get(p + 1)) {
                // --family <name> — flat census of one lineage
                let Some(vs) = veldt.functions.get(fam) else {
                    eprintln!("No family '{}'", fam); process::exit(1);
                };
                println!("=== The Veldt ({}) ===", fam);
                for e in vs {
                    let mark = if e.fitness == vs.iter().map(|x| x.fitness)
                        .fold(f64::NEG_INFINITY, f64::max) { " *" } else { "" };
                    println!("  {}#{}{} — {:?} | age: {} | fitness: {:.2} | origin: {}",
                        fam, e.id, mark, e.health, e.age, e.fitness, e.origin);
                }
            } else if args.iter().any(|a| a == "--games") {
                // --games — only families whose variants call game builtins
                const GAME_BUILTINS: &[&str] = &[
                    "canvas", "grid_", "ui_", "bt_", "balance", "astar", "bfs",
                    "los", "fov", "rand", "roll", "dice", "choice", "weighted",
                    "noise", "fbm", "perlin", "poisson", "voronoi", "delaunay",
                    "wfc", "maze", "flood", "neighbor", "lerp", "clamp", "dist",
                    "aabb", "smoothstep", "bezier", "point_in_poly", "convex",
                    "ease", "bounce", "line_points", "turn", "fsm", "book",
                    "svc_", "robot", "edge", "game_loop", "dist_stats",
                ];
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter().filter(|e| {
                        let src = crate::fmt::format_program(&e.body);
                        GAME_BUILTINS.iter().any(|b| src.contains(b))
                    }).cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (game families) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(min) = args.iter().position(|a| a == "--fit")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<f64>().ok()) {
                // --fit <min> — only variants at or above a fitness threshold
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter().filter(|e| e.fitness >= min).cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (fitness >= {}) ===", min);
                filtered.print_garden(sick_only, healthy_only);
            } else {
                println!("=== The Veldt ===");
                veldt.print_garden(sick_only, healthy_only);
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
            if args.iter().any(|a| a == "--json") {
                let vs = veldt.functions.get(&args[2]);
                match vs {
                    Some(list) => println!("{}", serde_json::json!({
                        "name": args[2],
                        "variants": list.iter().map(|e| serde_json::json!({
                            "id": e.id, "fitness": e.fitness,
                            "health": format!("{:?}", e.health),
                            "trust": format!("{:?}", e.trust),
                            "origin": e.origin,
                            "probes": veldt.probes.get(&args[2]).map(|p| p.len()).unwrap_or(0),
                        })).collect::<Vec<_>>(),
                    })),
                    None => println!("{{\"error\":\"no lineage '{}'\"}}", args[2]),
                }
            } else {
                veldt.print_lineage(&args[2]);
            }
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
        "help" => {
            print_help(args.get(2).map(|s| s.as_str()));
        }
        "docs" => {
            print_docs(args.get(2).map(|s| s.as_str()));
        }
        "perf" => {
            // Phase 3 micro-bench: ops/sec on three workloads
            let mut veldt = veldt::Veldt::new();
            let _ = veldt.load();
            let cases = [
                ("arith loop", "let i = 0\nlet a = 0\nwhile i < 50000 {\n    a += i * 2\n    i += 1\n}"),
                ("fn recursion", "fn f(n: int) { if n < 2 { return n } return f(n-1) + f(n-2) }\nlet _ = f(16)"),
                ("list ops", "let xs = []\nlet i = 0\nwhile i < 2000 {\n    xs = xs.push(i)\n    i += 1\n}\nlet _ = xs.len()"),
            ];
            for (name, src) in cases {
                let t = std::time::Instant::now();
                let r = agent::execute_source(src, &mut veldt);
                let ms = t.elapsed().as_millis();
                println!("{:<16} {:>6}ms {}", name, ms, if r.success { "ok" } else { "FAIL" });
            }
        }
        "stats" => {
            let mut veldt = veldt::Veldt::new();
            if let Err(e) = veldt.load() {
                eprintln!("Error loading veldt: {}", e);
                process::exit(1);
            }
            if args.iter().any(|a| a == "--json") {
                let mut total = 0usize;
                let (mut healthy, mut sick, mut dying) = (0usize, 0usize, 0usize);
                let (mut seed, mut estab, mut canon) = (0usize, 0usize, 0usize);
                for vs in veldt.functions.values() {
                    for e in vs {
                        total += 1;
                        match e.health {
                            Health::Healthy => healthy += 1,
                            Health::Sick(_) => sick += 1,
                            Health::Dying => dying += 1,
                        }
                        match e.trust {
                            veldt::Trust::Seedling => seed += 1,
                            veldt::Trust::Established => estab += 1,
                            veldt::Trust::Canonical => canon += 1,
                        }
                    }
                }
                println!("{}", serde_json::json!({
                    "lineages": veldt.functions.len(), "variants": total,
                    "healthy": healthy, "sick": sick, "dying": dying,
                    "seedling": seed, "established": estab, "canonical": canon,
                    "probes": veldt.probes.values().map(|v| v.len()).sum::<usize>(),
                    "deaths": veldt.obituaries.len(), "runs": veldt.run_count,
                }));
            } else {
                print_stats(&veldt);
            }
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
            // std:telemetry — absorb live call traces into probes first,
            // so production evidence participates in this selection round.
            absorb_traces(None);
            let mut veldt = veldt::Veldt::new();
            if let Err(e) = veldt.load() {
                eprintln!("Error loading veldt: {}", e);
                process::exit(1);
            }
            let dry = args.iter().any(|a| a == "--dry");
            if dry { eprintln!("[EVOLVE] dry run — changes not persisted"); }
            let fam_scope = args.iter().position(|a| a == "--family")
                .and_then(|p| args.get(p + 1));
            if let Some(f) = fam_scope { eprintln!("[EVOLVE] scoped to family: {}", f); }
            let mut evolver = evolve::Evolver::new();
            evolver.llm = args.iter().any(|a| a == "--llm");
            if let Some(pos) = args.iter().position(|a| a == "--seed") {
                if let Some(n) = args.get(pos + 1).and_then(|s| s.parse::<u64>().ok()) {
                    evolver.set_seed(n);
                }
            }
            // --until F: stop early when any variant reaches fitness F;
            // --until-fam name:F — scoped to one family.
            let until = args.iter().position(|a| a == "--until")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<f64>().ok());
            let until_fam = args.iter().position(|a| a == "--until-fam")
                .and_then(|p| args.get(p + 1))
                .and_then(|s| s.split_once(':')
                    .and_then(|(n, f)| Some((n.to_string(), f.parse::<f64>().ok()?))));
            let until_healthy = args.iter().position(|a| a == "--until-healthy")
                .and_then(|p| args.get(p + 1));
            let watch = args.iter().any(|a| a == "--watch");
            if watch { eprintln!("[EVOLVE] watch mode — Ctrl-C to stop"); }
            let mut run_cycles = 0usize;
            while run_cycles < cycles || watch {
                let events = evolver.evolve_scoped(&mut veldt, 1,
                    fam_scope.map(|s| s.as_str()));
                for event in &events { eprintln!("{}", event.format()); }
                run_cycles += 1;
                if let Some(target) = until {
                    let peak = veldt.functions.values()
                        .flat_map(|vs| vs.iter().map(|e| e.fitness))
                        .fold(f64::NEG_INFINITY, f64::max);
                    if peak >= target {
                        eprintln!("[EVOLVE] target fitness {} reached (peak {:.2}) — stopping after {} cycles",
                            target, peak, run_cycles);
                        break;
                    }
                }
                if let Some((fam, target)) = &until_fam {
                    let peak = veldt.functions.get(fam)
                        .map(|vs| vs.iter().map(|e| e.fitness).fold(f64::NEG_INFINITY, f64::max))
                        .unwrap_or(f64::NEG_INFINITY);
                    if peak >= *target {
                        eprintln!("[EVOLVE] {} hit target {} (peak {:.2}) — stopping after {} cycles",
                            fam, target, peak, run_cycles);
                        break;
                    }
                }
                if let Some(fam) = until_healthy {
                    let has_healthy = veldt.functions.get(fam)
                        .map(|vs| vs.iter().any(|e| e.health == veldt::Health::Healthy))
                        .unwrap_or(false);
                    if has_healthy {
                        eprintln!("[EVOLVE] {} has a healthy variant — stopping after {} cycles",
                            fam, run_cycles);
                        break;
                    }
                }
            }
            if dry {
                eprintln!("[EVOLVE] dry run complete — garden unchanged");
            } else {
                veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));
            }
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

fn print_docs(topic: Option<&str>) {
    const DOCS: &[(&str, &str, &str)] = &[
        ("len", "len(x) -> int", "len(\"abc\") == 3, len([1,2]) == 2"),
        ("args", "args() -> list<str>", "veldt run f.veldt -- a b → args() == [\"a\",\"b\"]"),
        ("print", "print(x)", "print(\"hi\") — stdout"),
        ("eprint", "eprint(x)", "stderr output"),
        ("input", "input([prompt]) -> str", "read a line from stdin"),
        ("exit", "exit([code])", "terminate process with code"),
        ("format", "format(\"%0 is %1\", a, b) -> str", "%N positional, %% literal"),
        ("fetch", "fetch(url[, headers]) -> dict{status,body}", "fetch(\"https://..\")"),
        ("post", "post(url, body[, headers]) -> dict{status,body}", "post(u, \"{}\")"),
        ("serve", "serve(port, fn(method, path, headers, body))", "blocks; handler returns str"),
        ("match_path", "match_path(pat, path) -> dict|null", "match_path(\"/u/:id\", \"/u/4\") → {id: \"4\"}"),
        ("dict", "dict() -> dict", "empty dict"),
        ("get/put/has", "d.get(k) d.put(k,v) d.has(k)", "immutable-style; put returns new dict"),
        ("re_match", "re_match(s, pat) -> bool", "regex match"),
        ("re_captures", "re_captures(s, pat) -> list|null", "capture groups"),
        ("re_find", "re_find(s, pat) -> str|null", "first match"),
        ("re_replace", "re_replace(s, pat, rep) -> str", "replace all"),
        ("re_split", "re_split(s, pat) -> list", "split by pattern"),
        ("env", "env(name) -> str|null", "read env var"),
        ("game", "canvas_* grid_* ui_* bt_* balance_* astar* bfs los fov rand* roll* turn fsm_next noise* fbm perlin poisson* voronoi delaunay wfc maze* flood* neighbor* lerp clamp dist aabb smoothstep bezier point_in_poly convex_hull ease_* bounce line_points weighted_choice choice randf rand_range rand_shuffle rand_gauss rand_bool rand_sign rand_permute book_* match_order svc_* robot_* edge_*", "game pillar — see 'help game'"),
        ("now", "now() -> float ms", "epoch millis"),
        ("date", "date([fmt]) -> str", "%Y %m %d %H %M %S tokens"),
        ("sleep", "sleep(ms)", "block"),
        ("rand", "rand() -> float", "[0,1)"),
        ("spawn", "spawn(\"fn\", [args]) -> task_id", "concurrent thread"),
        ("join", "join(task_id) -> value", "wait for task"),
        ("pmap", "pmap(\"fn\", list) -> list", "parallel map over garden fn"),
        ("call_best", "call_best(\"name\", args) -> value", "dispatch fittest variant"),
        ("sandbox_run", "sandbox_run(\"name\", args) -> value", "call fn with all capabilities denied (Stage 0)"),
        ("shadow_call", "shadow_call(\"name\", args) -> value", "all variants run sandboxed; champion's result returns, divergences logged (Stage 1)"),
        ("read_file", "read_file(path) -> str", ""),
        ("write_file", "write_file(path, content)", ""),
    ];
    match topic {
        None => {
            println!("veldt docs <builtin> — {} documented. Categories via: veldt help builtins", DOCS.len());
            for (name, sig, _) in DOCS { println!("  {:<14} {}", name, sig); }
        }
        Some(t) => {
            let t = t.trim_start_matches('(');
            let mut found = false;
            for (name, sig, ex) in DOCS {
                if name.contains(t) || sig.contains(t) {
                    println!("{:<14} {}", name, sig);
                    if !ex.is_empty() { println!("               eg {}", ex); }
                    found = true;
                }
            }
            if !found { println!("No docs for '{}' — try `veldt docs` for the list", t); }
        }
    }
}

fn print_help(topic: Option<&str>) {
    let text = match topic.unwrap_or("") {
        "run" => "veldt run <file.veldt> [-- a b c]  — run a script; args() returns the -- tail\n  veldt run -e \"code\" — inline eval",
        "repl" => "veldt repl — interactive loop\n  :help :garden :quit — commands",
        "agent" => "veldt agent \"task\" [--json] — the Gardener (rule-based coding agent)\n  write/show/test/adopt/improve/suggest/bench/evolve/run — say \"help\" for verbs",
        "evolve" => "veldt evolve [N] [--llm] [--seed N] [--until F|--until-fam name:F]\n  live call traces auto-absorb into probes first — evidence feeds selection",
        "garden" => "veldt garden [--sick|--obituaries|--json] — census · veldt lineage <name> · veldt stats [--json]\n  veldt test <name>|--all · veldt adopt <name#N> · veldt fuzz <name>|--all\n  veldt find <substr> · veldt diff <a#n> <b#n> · veldt absorb · veldt tournament <url>",
        "builtins" => "len type str int float print eprint input read_stdin args exit\n  abs max min round floor ceil sqrt pow rand date now sleep env\n  fetch post serve match_path read_file write_file ls exists\n  dict get put has keys values items merge delete get_or\n  re_match re_find re_replace re_split re_captures format\n  canvas_* grid_* ui_* bt_* balance_* astar* bfs los fov turn fsm_next\n  rand* roll dice choice weighted_choice noise2 fbm perlin poisson_disc\n  voronoi delaunay wfc maze_gen flood_fill neighbors lerp clamp dist aabb\n  smoothstep bezier point_in_poly convex_hull ease_* bounce line_points",
        "strings" => "s.len() s.trim() s.ltrim() s.rtrim() s.to_lower() s.to_upper()\n  s.split(sep) s.replace(a,b) s.contains(x) s.starts_with(p) s.ends_with(p)\n  s.chars() s.repeat(n) s.pad(n[,ch]) s.split_at(i) s[a:b]",
        "lists" => "xs.len() xs.push(v) xs.pop() xs.contains(v) xs.join(sep)\n  xs.enumerate() xs.zip(ys) xs.flatten() xs.take(n) xs.skip(n)\n  xs.sort() xs.sort_by(f) xs.group_by(f) xs.min_by(f) xs.max_by(f)\n  xs.reverse() xs.index_of(v) xs[a:b] xs[-1]",
        "types" => "int float str bool list dict set null fn — types are advisory docs\n  `x in xs` membership · `??` null-coalesce · `?.` safe access · `|>` pipe",
        "registry" => "veldt registry [port] — host a garden pool · veldt push <url> · veldt pull <url> [--merge]\n  veldt search <url> <name> · veldt tournament <url> — pull, trial, push winners back\n  veldt import <file|url> [--merge] · veldt export [file] — garden JSON",
        "sandbox" => "veldt run --sandbox <file> · veldt watch <file> --sandbox — Stage-0 caps (fs/net/proc denied)\n  sandbox_run(\"fn\", args) builtin · shadow_call(\"fn\", args) — all variants run sandboxed\n  Trials/probes/fuzz already run sandboxed — untrusted code can't touch the host",
        _ => "veldt help <topic> — run, repl, agent, evolve, garden, builtins, strings, lists, types, registry, sandbox",
    };
    println!("{}", text);
}

fn print_stats(veldt: &veldt::Veldt) {
    let mut total = 0usize;
    let mut healthy = 0usize;
    let mut sick = 0usize;
    let mut dying = 0usize;
    let mut best: Vec<(String, f64, usize)> = Vec::new();
    for (name, variants) in &veldt.functions {
        let top = variants.iter().map(|e| e.fitness)
            .fold(f64::NEG_INFINITY, f64::max);
        best.push((name.clone(), if variants.is_empty() { 0.0 } else { top }, variants.len()));
        for e in variants {
            total += 1;
            match e.health {
                Health::Healthy => healthy += 1,
                Health::Sick(_) => sick += 1,
                Health::Dying => dying += 1,
            }
        }
    }
    best.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let probe_count: usize = veldt.probes.values().map(|v| v.len()).sum();
    let (mut seed, mut estab, mut canon) = (0usize, 0usize, 0usize);
    for vs in veldt.functions.values() {
        for e in vs {
            match e.trust {
                veldt::Trust::Seedling => seed += 1,
                veldt::Trust::Established => estab += 1,
                veldt::Trust::Canonical => canon += 1,
            }
        }
    }
    println!("garden: {} lineages, {} variants ({} healthy, {} sick, {} dying)",
        veldt.functions.len(), total, healthy, sick, dying);
    println!("trust ladder: {} seedling → {} established → {} canonical",
        seed, estab, canon);
    println!("probes: {} across {} families | deaths: {} | runs: {}",
        probe_count, veldt.probes.len(), veldt.obituaries.len(), veldt.run_count);
    // builtin usage — which builtins living variants call (mutation targeting)
    const BUILTINS: &[&str] = &[
        "canvas", "grid", "ui_", "bt_", "balance", "astar", "bfs", "los", "fov",
        "rand", "roll", "dice", "choice", "weighted", "noise", "fbm", "perlin",
        "poisson", "voronoi", "delaunay", "wfc", "maze", "flood", "neighbor",
        "lerp", "clamp", "dist", "aabb", "smoothstep", "bezier", "point_in_poly",
        "convex", "ease", "bounce", "line_points", "turn", "fsm", "book", "svc",
        "robot", "edge", "game_loop", "dist_stats",
    ];
    let mut use_count: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            let src = crate::fmt::format_program(&e.body);
            for b in BUILTINS {
                if src.contains(b) { *use_count.entry(b).or_default() += 1; }
            }
        }
    }
    let mut top: Vec<_> = use_count.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    if !top.is_empty() {
        let line: Vec<String> = top.iter().take(12)
            .map(|(b, c)| format!("{}×{}", b, c)).collect();
        println!("builtin usage (living variants): {}", line.join(" "));
    }
    // fitness per mutation source — which operators produce FIT variants
    let mut fit_by_source: std::collections::HashMap<String, Vec<f64>> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            let src = e.origin.split(' ').next().unwrap_or("seed").to_string();
            fit_by_source.entry(src).or_default().push(e.fitness);
        }
    }
    let mut src_fit: Vec<(String, f64, usize)> = fit_by_source.into_iter()
        .map(|(s, fits)| {
            let avg = fits.iter().sum::<f64>() / fits.len().max(1) as f64;
            (s, avg, fits.len())
        }).collect();
    src_fit.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !src_fit.is_empty() {
        let line: Vec<String> = src_fit.iter().take(8)
            .map(|(s, f, n)| format!("{}→{:.2}({})", s, f, n)).collect();
        println!("fitness by mutation source: {}", line.join(" "));
    }
    // probe coverage per family — selection-pressure depth
    let mut cov: Vec<(&String, usize)> = veldt.probes.iter()
        .map(|(n, p)| (n, p.len())).collect();
    cov.sort_by(|a, b| b.1.cmp(&a.1));
    if !cov.is_empty() {
        let line: Vec<String> = cov.iter().take(10)
            .map(|(n, c)| format!("{}:{}", n, c)).collect();
        println!("probe coverage: {}", line.join(" "));
    }
    // avg niche scores across the garden — which niches drive fitness
    let mut niche_sum: std::collections::HashMap<String, (f64, usize)> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            for (n, s) in &e.niche_scores {
                let ent = niche_sum.entry(n.clone()).or_default();
                ent.0 += s; ent.1 += 1;
            }
        }
    }
    let mut niches: Vec<(String, f64)> = niche_sum.into_iter()
        .map(|(n, (s, c))| (n, s / c.max(1) as f64)).collect();
    niches.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !niches.is_empty() {
        let line: Vec<String> = niches.iter().take(8)
            .map(|(n, a)| format!("{}:{:.2}", n, a)).collect();
        println!("avg niche scores: {}", line.join(" "));
    }
    // mutation success rate — which operators produce healthy variants
    let mut mut_rate: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            let src = e.origin.split(' ').next().unwrap_or("seed").to_string();
            let ent = mut_rate.entry(src).or_default();
            if e.health == Health::Healthy { ent.0 += 1; }
            ent.1 += 1;
        }
    }
    let mut rates: Vec<(String, usize, usize)> = mut_rate.into_iter()
        .map(|(s, (h, t))| (s, h, t)).collect();
    rates.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));
    if !rates.is_empty() {
        let line: Vec<String> = rates.iter().take(8)
            .map(|(s, h, t)| format!("{}:{}/{}", s, h, t)).collect();
        println!("mutation success (healthy/total): {}", line.join(" "));
    }
    // probe niche coverage — which input niches the probes target
    let mut niche_count: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for probes in veldt.probes.values() {
        for p in probes {
            *niche_count.entry(p.niche.clone()).or_default() += 1;
        }
    }
    let mut niches: Vec<(String, usize)> = niche_count.into_iter().collect();
    niches.sort_by(|a, b| b.1.cmp(&a.1));
    if !niches.is_empty() {
        let line: Vec<String> = niches.iter().take(8)
            .map(|(n, c)| format!("{}:{}", n, c)).collect();
        println!("probe niches: {}", line.join(" "));
    }
    // fitness trend — peak per run, is the garden improving?
    if veldt.fitness_history.len() >= 2 {
        let trend: Vec<String> = veldt.fitness_history.iter()
            .map(|f| format!("{:.2}", f)).collect();
        let (first, last) = (trend.first().unwrap(), trend.last().unwrap());
        let arrow = if veldt.fitness_history.last() > veldt.fitness_history.first() { "↑" }
                    else if veldt.fitness_history.last() < veldt.fitness_history.first() { "↓" } else { "→" };
        println!("fitness trend: {} → {} {}", first, last, arrow);
    }
    // coverage line — which families carry probe mass vs bare lineages
    let bare: Vec<&String> = veldt.functions.keys()
        .filter(|n| veldt.probes.get(*n).map(|p| p.is_empty()).unwrap_or(true))
        .collect();
    if !bare.is_empty() {
        println!("bare lineages (no probes — zero selection pressure): {}",
            bare.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "));
    }
    // mutation-source histogram — which operators produce living variants
    let mut sources: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            let src = e.origin.split(" of ").next().unwrap_or("?").to_string();
            // crossovers carry parents in the name — collapse to the op
            let src = if src.starts_with("crossover") { "crossover".to_string() } else { src };
            *sources.entry(src).or_default() += 1;
        }
    }
    if !sources.is_empty() {
        let mut ranked: Vec<_> = sources.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1));
        println!("mutation sources: {}", ranked.iter()
            .map(|(s, n)| format!("{}×{}", s, n)).collect::<Vec<_>>().join("  "));
    }
    let immigrants: usize = veldt.functions.values().flatten()
        .filter(|e| e.origin.starts_with("immigrant:")).count();
    if immigrants > 0 {
        println!("immigrants: {} variants from remote gardens", immigrants);
    }
    if !best.is_empty() {
        println!("top families:");
        for (name, fit, n) in best.iter().take(10) {
            println!("  {:<20} fitness {:>5.2}  ({} variants)", name, fit, n);
        }
    }
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
  veldt watch <file>           Re-run a program every time it changes
  veldt lsp                    Language server over stdio (hover/goto/diagnostics/completion)
  veldt test <name>            Re-trial all variants of a function family
  veldt adopt <name#N>         Canonicalize a variant (dispatch picks it)
  veldt export [file]          Export the garden to a JSON file
  veldt import <file>          Import a garden JSON file
  veldt run --trace <file>     Trace every executed statement
  veldt run --evolve <file>    Run 1 evolve cycle after the program
  veldt run --sandbox <file>   Run with all side-effect caps denied
  veldt watch <file> --sandbox Re-run untrusted code under Stage-0 caps
  veldt fuzz <name>|--all      Boundary-corpus sweep; divergences become probes
  veldt absorb                 Convert live call traces into probes
  veldt tournament <url>       Pull pool, trial merged families, push winners
  veldt diff <a#n> <b#n>       Show two variants' bodies side by side
  veldt stats                  Garden census + trust-ladder distribution
  veldt check <file>           Parse-only validation (CI/editor gate)
  veldt find <substr>          Local family search by name
  veldt diff --unified a b     Interleaved LCS diff of two variants");
    println!("  veldt visual                 Open visual ecosystem viewer in browser");
}

// === Garden registry: a tiny host where gardens meet ===
//
// POST /api/garden  — accepts a SavedVeldt JSON body; each (name,id)
// variant is merged into the pool (incoming wins on id conflict).
// GET  /api/garden  — returns the pool so clients can pull it.
// Pool persists at ~/.veldt/registry.json.

#[derive(Default)]
struct RegistryPool {
    gardens: std::collections::HashMap<String, serde_json::Value>,
}

fn registry_pool_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join(".veldt").join("registry.json")
}

fn registry_get() -> String {
    let p = registry_pool_path();
    if p.exists() {
        std::fs::read_to_string(&p).unwrap_or_else(|_| "{}".into())
    } else {
        "{}".into()
    }
}

fn registry_post(body: &str) -> Result<String, String> {
    let incoming: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| format!("bad garden json: {}", e))?;
    let mut pool: serde_json::Value = serde_json::from_str(&registry_get())
        .unwrap_or(serde_json::json!({}));
    // merge function families + traces; newest entries win per (name,id)
    if let (Some(inc), Some(pool_obj)) = (
        incoming.get("functions").and_then(|f| f.as_object()),
        pool.get_mut("functions").and_then(|f| f.as_object_mut()),
    ) {
        for (name, variants) in inc {
            let pool_family = pool_obj.entry(name.clone())
                .or_insert_with(|| serde_json::json!([]));
            if let (Some(pv), Some(iv)) =
                (pool_family.as_array_mut(), variants.as_array()) {
                for entry in iv {
                    let id = entry.get("id").and_then(|i| i.as_u64());
                    if let Some(pos) = pv.iter().position(|e|
                        e.get("id").and_then(|i| i.as_u64()) == id) {
                        pv[pos] = entry.clone();
                    } else {
                        pv.push(entry.clone());
                    }
                }
            }
        }
    } else if let Some(inc) = incoming.get("functions") {
        pool.as_object_mut()
            .map(|o| o.insert("functions".into(), inc.clone()));
    }
    let out = serde_json::to_string(&pool).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(registry_pool_path().parent().unwrap()).ok();
    std::fs::write(registry_pool_path(), &out).map_err(|e| e.to_string())?;
    Ok(format!("merged — pool now holds {} families",
        pool.get("functions").and_then(|f| f.as_object())
            .map(|o| o.len()).unwrap_or(0)))
}

fn run_registry(port: u16) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        use axum::{Router, routing::{get, post}};
        let app = Router::new()
            .route("/api/garden", get(|| async { registry_get() }))
            .route("/api/garden", post(|body: String| async move {
                match registry_post(&body) {
                    Ok(msg) => msg,
                    Err(e) => format!("error: {}", e),
                }
            }));
        let addr = format!("0.0.0.0:{}", port);
        println!("Garden registry on http://{}/api/garden", addr);
        println!("  veldt push http://<host>:{}  — send your garden here", port);
        println!("  veldt pull http://<host>:{}  — take the pool with you", port);
        let listener = tokio::net::TcpListener::bind(&addr).await
            .expect("bind registry port");
        axum::serve(listener, app).await.expect("registry serve");
    });
}

pub fn push_garden(url: &str) {
    let mut veldt = veldt::Veldt::new();
    if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
    let body = match std::fs::read_to_string(veldt.garden_path.join("veldt.json")) {
        Ok(b) => b,
        Err(e) => { eprintln!("No garden to push: {}", e); process::exit(1); }
    };
    let endpoint = format!("{}/api/garden", url.trim_end_matches('/'));
    let client = reqwest::blocking::Client::builder()
        .user_agent("Veldt/0.3").timeout(std::time::Duration::from_secs(30))
        .build().expect("http client");
    match client.post(&endpoint).body(body).send() {
        Ok(r) => println!("Pushed garden -> {} ({})", url,
            r.text().unwrap_or_default()),
        Err(e) => { eprintln!("Push failed: {}", e); process::exit(1); }
    }
}

pub fn merge_garden(url: &str) {
    // pull the pool and merge its families INTO the local garden —
    // immigrants keep their provenance and get tagged.
    let client = reqwest::blocking::Client::builder()
        .user_agent("Veldt/0.3").timeout(std::time::Duration::from_secs(30))
        .build().expect("http client");
    let body = match client.get(url).send().and_then(|r| r.error_for_status()) {
        Ok(r) => r.text().unwrap_or_default(),
        Err(e) => { eprintln!("Pull failed: {}", e); process::exit(1); }
    };
    let remote: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v, Err(e) => { eprintln!("Bad pool json: {}", e); process::exit(1); }
    };
    let added = merge_garden_json(&remote);
    println!("Merged {} immigrant variants from {}", added, url);
}

/// Merge remote garden JSON into the local garden — shared by
/// `pull --merge` and `import --merge`. Returns immigrants added.
fn merge_garden_json(remote: &serde_json::Value) -> usize {
    let mut veldt = veldt::Veldt::new();
    if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
    let mut added = 0;
    if let Some(fns) = remote.get("functions").and_then(|f| f.as_object()) {
        for (name, variants) in fns {
            let Some(list) = variants.as_array() else { continue; };
            let family = veldt.functions.entry(name.clone()).or_insert_with(Vec::new);
            for v in list {
                let id = v.get("id").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
                if family.iter().any(|e| e.id == id) { continue; }
                if let Ok(mut entry) = serde_json::from_value::<veldt::FunctionEntry>(v.clone()) {
                    entry.origin = format!("immigrant:{}", entry.origin);
                    family.push(entry);
                    added += 1;
                }
            }
        }
    }
    if let Err(e) = veldt.save() { eprintln!("Save failed: {}", e); process::exit(1); }
    added
}

fn search_registry(url: &str, needle: &str) {
    let client = reqwest::blocking::Client::builder()
        .user_agent("Veldt/0.3").timeout(std::time::Duration::from_secs(15))
        .build().expect("http client");
    let endpoint = format!("{}/api/garden", url.trim_end_matches('/'));
    let body = match client.get(&endpoint).send().and_then(|r| r.error_for_status()) {
        Ok(r) => r.text().unwrap_or_default(),
        Err(e) => { eprintln!("Search failed: {}", e); process::exit(1); }
    };
    let pool: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    let mut found = 0;
    if let Some(fns) = pool.get("functions").and_then(|f| f.as_object()) {
        for (name, variants) in fns {
            if !name.contains(needle) { continue; }
            found += 1;
            let n = variants.as_array().map(|v| v.len()).unwrap_or(0);
            println!("  {} — {} variant{}", name, n, if n == 1 { "" } else { "s" });
        }
    }
    if found == 0 { println!("No families matching '{}'", needle); }
}

fn fmt_check(path: &str) {
    let src = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Cannot read {}: {}", path, e); process::exit(1);
    });
    let mut lx = lexer::Lexer::new(&src);
    let tokens = lx.tokenize().unwrap_or_else(|e| {
        eprintln!("lex error: {}", e); process::exit(1);
    });
    let mut p = parser::Parser::new_with_lines(tokens, lx.token_lines.clone());
    let stmts = p.parse_program().unwrap_or_else(|e| {
        eprintln!("parse error: {}", e); process::exit(1);
    });
    let formatted = fmt::format_program(&stmts);
    if formatted.trim_end() == src.trim_end() {
        println!("{} is formatted", path);
    } else {
        eprintln!("{} is NOT formatted (run: veldt fmt {} --write)", path, path);
        process::exit(1);
    }
}

/// veldt check --strict — lint-lite warnings on a parsed program:
/// grow fns without test clauses, statements after return, lets
/// bound but never referenced.
pub fn lint(stmts: &[ast::Stmt]) -> Vec<String> {
    use ast::*;
    let mut out = Vec::new();
    lint_block(stmts, &mut out);
    out
}

fn lint_block(stmts: &[ast::Stmt], out: &mut Vec<String>) {
    use ast::*;
    let mut returned = false;
    // vars bound in this block — flag if never read later
    let mut bound: Vec<(usize, &String)> = Vec::new();
    for (i, s) in stmts.iter().enumerate() {
        if returned { out.push("unreachable code after return".into()); }
        match s {
            Stmt::Grow(GrowItem::Fn(name, params, body, tests)) => {
                if tests.is_empty() {
                    out.push(format!("grow fn {} has no test clause — no birth-check", name));
                }
                for p in params {
                    if !p.name.starts_with('_')
                        && !body.iter().any(|s| stmt_mentions(s, &p.name)) {
                        out.push(format!("{}: param {} never used", name, p.name));
                    }
                }
                let mut sub = Vec::new();
                lint_block(body, &mut sub);
                out.extend(sub.into_iter().map(|w| format!("{}: {}", name, w)));
            }
            Stmt::FnDef(name, params, body) => {
                for p in params {
                    if !p.name.starts_with('_')
                        && !body.iter().any(|s| stmt_mentions(s, &p.name)) {
                        out.push(format!("{}: param {} never used", name, p.name));
                    }
                }
                let mut sub = Vec::new();
                lint_block(body, &mut sub);
                out.extend(sub.into_iter().map(|w| format!("{}: {}", name, w)));
            }
            Stmt::Let(name, _) => bound.push((i, name)),
            Stmt::If(cond, a, b) => {
                if let Some(lit) = const_bool(cond) {
                    out.push(format!("if on constant {} — dead branch", lit));
                }
                lint_block(a, out);
                if let Some(e) = b { lint_block(e, out); }
            }
            Stmt::While(cond, body) | Stmt::For(_, cond, body) => {
                if let Some(lit) = const_bool(cond) {
                    out.push(format!("loop on constant {} — dead or infinite", lit));
                }
                if !body.iter().any(|s| has_exit(s)) {
                    out.push("loop body has no break/return/continue — check the exit condition".into());
                }
                lint_block(body, out);
            }
            Stmt::Try(a, _, b) => { lint_block(a, out); lint_block(b, out); }
            Stmt::Match(_, arms, els) => {
                for (_, body) in arms { lint_block(body, out); }
                if let Some(e) = els { lint_block(e, out); }
            }
            Stmt::Return(_) => returned = true,
            _ => {}
        }
    }
    for (i, name) in bound {
        let read = stmts.iter().skip(i + 1).any(|s| stmt_mentions(s, name));
        if !read && !name.starts_with('_') {
            out.push(format!("let {} bound but never read", name));
        }
    }
    // unused imports — alias bound but never referenced
    for s in stmts {
        if let Stmt::Import(path, Some(alias)) = s {
            if !alias.starts_with('_')
                && !stmts.iter().any(|x| stmt_mentions(x, alias)) {
                out.push(format!("import \"{}\" as {} — alias never used", path, alias));
            }
        }
    }
    // shadowed lets — `let x` redefining an earlier `let x` in the same block
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for s in stmts {
        if let Stmt::Let(name, _) = s {
            if !seen.insert(name.as_str()) {
                out.push(format!("let {} shadows an earlier binding in this block", name));
            }
        }
    }
    // dead functions — `fn name`/`grow fn name` defined but never called
    let mut defs: Vec<&String> = Vec::new();
    for s in stmts {
        match s {
            Stmt::FnDef(n, _, _) | Stmt::Grow(GrowItem::Fn(n, _, _, _)) => defs.push(n),
            _ => {}
        }
    }
    for name in defs {
        let called = stmts.iter().any(|s| stmt_calls(s, name));
        if !called {
            out.push(format!("fn {} defined but never called", name));
        }
    }
}

/// Does this block contain a loop-exit statement (break/return/continue)?
fn has_exit(s: &ast::Stmt) -> bool {
    use ast::*;
    match s {
        Stmt::Break | Stmt::Continue | Stmt::Return(_) => true,
        Stmt::If(_, a, b) => a.iter().any(has_exit)
            || b.as_ref().map(|x| x.iter().any(has_exit)).unwrap_or(false),
        Stmt::While(_, body) | Stmt::For(_, _, body) => body.iter().any(has_exit),
        Stmt::Try(a, _, b) => a.iter().any(has_exit) || b.iter().any(has_exit),
        Stmt::Match(_, arms, els) => arms.iter().any(|(_, b)| b.iter().any(has_exit))
            || els.as_ref().map(|x| x.iter().any(has_exit)).unwrap_or(false),
        _ => false,
    }
}

/// Is this expr a compile-time literal? Returns a label for the warning.
fn const_bool(e: &ast::Expr) -> Option<&'static str> {
    match e {
        ast::Expr::Bool(true) => Some("true"),
        ast::Expr::Bool(false) => Some("false"),
        ast::Expr::Int(0) => Some("0"),
        ast::Expr::Null => Some("null"),
        ast::Expr::UnaryOp(ast::UnaryOp::Not, inner) if const_bool(inner).is_some() =>
            Some("!const"),
        _ => None,
    }
}

/// Does this statement (or its children) call a function named `name`?
fn stmt_calls(s: &ast::Stmt, name: &str) -> bool {
    use ast::*;
    match s {
        Stmt::Let(_, e) | Stmt::ExprStmt(e) | Stmt::Print(e) | Stmt::Assign(_, e) => expr_calls(e, name),
        Stmt::If(c, a, b) => expr_calls(c, name) || a.iter().any(|x| stmt_calls(x, name))
            || b.as_ref().map(|x| x.iter().any(|y| stmt_calls(y, name))).unwrap_or(false),
        Stmt::While(c, body) | Stmt::For(_, c, body) => expr_calls(c, name) || body.iter().any(|x| stmt_calls(x, name)),
        Stmt::Return(Some(e)) => expr_calls(e, name),
        Stmt::Grow(GrowItem::Fn(_, _, body, _)) | Stmt::FnDef(_, _, body) => body.iter().any(|x| stmt_calls(x, name)),
        Stmt::LetDestructure(_, e) => expr_calls(e, name),
        Stmt::Try(a, _, b) => a.iter().any(|x| stmt_calls(x, name)) || b.iter().any(|x| stmt_calls(x, name)),
        Stmt::Match(c, arms, els) => expr_calls(c, name)
            || arms.iter().any(|(p, b)| expr_calls(p, name) || b.iter().any(|x| stmt_calls(x, name)))
            || els.as_ref().map(|x| x.iter().any(|y| stmt_calls(y, name))).unwrap_or(false),
        _ => false,
    }
}

fn expr_calls(e: &ast::Expr, name: &str) -> bool {
    use ast::*;
    match e {
        Expr::Call(n, _, args) => n == name || args.iter().any(|a| expr_calls(a, name)),
        Expr::MethodCall(a, _, args) | Expr::SafeGet(a, _, Some(args)) => expr_calls(a, name) || args.iter().any(|x| expr_calls(x, name)),
        Expr::BinOp(a, _, b) => expr_calls(a, name) || expr_calls(b, name),
        Expr::UnaryOp(_, a) => expr_calls(a, name),
        Expr::Index(a, i) => expr_calls(a, name) || expr_calls(i, name),
        Expr::Slice(o, lo, hi) => expr_calls(o, name)
            || lo.as_ref().map(|x| expr_calls(x, name)).unwrap_or(false)
            || hi.as_ref().map(|x| expr_calls(x, name)).unwrap_or(false),
        Expr::List(items) | Expr::Interp(items) => items.iter().any(|i| expr_calls(i, name)),
        Expr::StructLit(_, fs) => fs.iter().any(|(_, v)| expr_calls(v, name)),
        Expr::Lambda(_, body) => body.iter().any(|s| stmt_calls(s, name)),
        Expr::ListComp(v, _, iter, cond) => expr_calls(v, name) || expr_calls(iter, name)
            || cond.as_ref().map(|c| expr_calls(c, name)).unwrap_or(false),
        Expr::SafeGet(a, _, None) | Expr::FieldAccess(a, _) => expr_calls(a, name),
        _ => false,
    }
}

fn stmt_mentions(s: &ast::Stmt, name: &str) -> bool {
    use ast::*;
    match s {
        Stmt::Let(_, e) | Stmt::ExprStmt(e) | Stmt::Print(e) | Stmt::Assign(_, e) => expr_mentions(e, name),
        Stmt::If(c, a, b) => expr_mentions(c, name) || a.iter().any(|x| stmt_mentions(x, name))
            || b.as_ref().map(|x| x.iter().any(|y| stmt_mentions(y, name))).unwrap_or(false),
        Stmt::While(c, body) | Stmt::For(_, c, body) => expr_mentions(c, name) || body.iter().any(|x| stmt_mentions(x, name)),
        Stmt::Return(Some(e)) => expr_mentions(e, name),
        Stmt::Grow(GrowItem::Fn(_, _, body, _)) | Stmt::FnDef(_, _, body) => body.iter().any(|x| stmt_mentions(x, name)),
        Stmt::LetDestructure(_, e) => expr_mentions(e, name),
        _ => false,
    }
}

fn expr_mentions(e: &ast::Expr, name: &str) -> bool {
    use ast::*;
    match e {
        Expr::Var(n) => n == name,
        Expr::BinOp(a, _, b) => expr_mentions(a, name) || expr_mentions(b, name),
        Expr::UnaryOp(_, a) => expr_mentions(a, name),
        Expr::Call(_, _, args) => args.iter().any(|a| expr_mentions(a, name)),
        Expr::Index(a, i) => expr_mentions(a, name) || expr_mentions(i, name),
        Expr::Slice(o, lo, hi) => expr_mentions(o, name)
            || lo.as_ref().map(|x| expr_mentions(x, name)).unwrap_or(false)
            || hi.as_ref().map(|x| expr_mentions(x, name)).unwrap_or(false),
        Expr::MethodCall(a, _, args) | Expr::SafeGet(a, _, Some(args)) =>
            expr_mentions(a, name) || args.iter().any(|x| expr_mentions(x, name)),
        Expr::SafeGet(a, _, None) | Expr::FieldAccess(a, _) => expr_mentions(a, name),
        Expr::StructLit(_, fields) => fields.iter().any(|(_, v)| expr_mentions(v, name)),
        Expr::List(items) => items.iter().any(|i| expr_mentions(i, name)),
        Expr::Interp(parts) => parts.iter().any(|p| expr_mentions(p, name)),
        Expr::Lambda(_, body) => body.iter().any(|s| stmt_mentions(s, name)),
        Expr::ListComp(v, _, iter, cond) => expr_mentions(v, name) || expr_mentions(iter, name)
            || cond.as_ref().map(|c| expr_mentions(c, name)).unwrap_or(false),
        _ => false,
    }
}

fn watch_file(path: &str) {
    let mut last = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    println!("watching {} (ctrl-c to stop)", path);
    run_program(path, false, false);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(400));
        let now = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if now != last {
            last = now;
            println!("\n--- {} changed, rerunning ---", path);
            run_program(path, false, false);
        }
    }
}

// REPL tab-completion: builtins + keywords + garden fn names.
struct VeldtHelper { names: Vec<String> }

impl rustyline::completion::Completer for VeldtHelper {
    type Candidate = rustyline::completion::Pair;
    fn complete(&self, line: &str, pos: usize, _c: &rustyline::Context<'_>)
        -> rustyline::Result<(usize, Vec<rustyline::completion::Pair>)> {
        let start = line[..pos].rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map(|i| i + 1).unwrap_or(0);
        let prefix = &line[start..pos];
        let pairs: Vec<_> = self.names.iter()
            .filter(|n| n.starts_with(prefix) && !prefix.is_empty())
            .map(|n| rustyline::completion::Pair {
                display: n.clone(), replacement: n.clone() })
            .collect();
        Ok((start, pairs))
    }
}
impl rustyline::hint::Hinter for VeldtHelper {
    type Hint = String;
}
impl rustyline::highlight::Highlighter for VeldtHelper {}
impl rustyline::validate::Validator for VeldtHelper {}
impl rustyline::Helper for VeldtHelper {}

fn run_program_vm(path: &str) {
    // Phase 11: bytecode VM path. Same lex/parse; different executor.
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => { eprintln!("Error reading file '{}': {}", path, e); process::exit(1); }
    };
    let mut lexer = lexer::Lexer::new(&source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(e) => { eprintln!("Lex error: {}", e); process::exit(1); }
    };
    let mut parser = parser::Parser::new_with_lines(tokens, lexer.token_lines.clone());
    let stmts = match parser.parse_program() {
        Ok(s) => s,
        Err(e) => { eprintln!("Parse error: {}", e); process::exit(1); }
    };
    let mut steps: u64 = 100_000_000;
    let cli_args: Vec<String> = env::args().collect();
    if let Some(pos) = cli_args.iter().position(|a| a == "--steps") {
        if let Some(n) = cli_args.get(pos + 1).and_then(|s| s.parse::<u64>().ok()) {
            steps = n;
        }
    }
    let mut v = vm::Vm::new();
    let mut garden = veldt::Veldt::new();
    if garden.load().is_ok() {
        if let Err(e) = v.load_garden(&garden) {
            eprintln!("VM note: {}", e);
        }
    }
    if let Err(e) = v.run(&stmts, steps) {
        eprintln!("VM runtime error: {}", e);
        process::exit(1);
    }
}

fn start_repl() {
    use rustyline::DefaultEditor;
    let mut veldt = veldt::Veldt::new();
    let _ = veldt.load();
    let mut interp = interpreter::Interpreter::new();
    veldt.load_into_interpreter(&mut interp);

    println!("veldt {} — repl. :help for commands, ctrl-d to quit", env!("CARGO_PKG_VERSION"));
    let mut rl = rustyline::Editor::<VeldtHelper, rustyline::history::FileHistory>::new()
        .expect("rustyline init");
    let names: Vec<String> = ["fn","grow","let","while","for","if","else","return",
        "match","test","try","spawn","import","print","len","push","split","join",
        "sort","map","filter","reduce","keys","values","dict","list","range",
        "call_best","sandbox_run","shadow_call","env","args","exit","fetch","re_match","format","str","int"]
        .iter().map(|s| s.to_string())
        .chain(veldt.functions.keys().cloned())
        .collect();
    rl.set_helper(Some(VeldtHelper { names }));
    let hist = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
        .join(".veldt").join("repl_history.txt");
    let _ = rl.load_history(&hist);
    let mut buf = String::new();
    let mut depth: i64 = 0;
    loop {
        let prompt = if buf.is_empty() { "veldt> " } else { "  ... " };
        let line = match rl.readline(prompt) {
            Ok(l) => { let _ = rl.add_history_entry(l.as_str()); l }
            Err(_) => { println!(); break; }
        };
        let trimmed = line.trim();
        if buf.is_empty() {
            if trimmed.is_empty() { continue; }
            if trimmed.starts_with(':') {
                match trimmed {
                    ":quit" | ":q" => break,
                    _ if trimmed.starts_with(":load ") => {
                        let path = trimmed[6..].trim();
                        match std::fs::read_to_string(path) {
                            Ok(src) => {
                                let mut lx = lexer::Lexer::new(&src);
                                match lx.tokenize() {
                                    Ok(toks) => {
                                        let mut p = parser::Parser::new_with_lines(toks, lx.token_lines.clone());
                                        match p.parse_program() {
                                            Ok(stmts) => {
                                                for st in &stmts {
                                                    if let Err(e) = interp.exec_stmt_pub(st) {
                                                        eprintln!("{}", e); break;
                                                    }
                                                }
                                                println!("loaded {}", path);
                                            }
                                            Err(e) => eprintln!("parse: {}", e),
                                        }
                                    }
                                    Err(e) => eprintln!("lex: {}", e),
                                }
                            }
                            Err(e) => eprintln!("{}", e),
                        }
                        continue;
                    }
                    ":garden" => { let _ = veldt.load(); veldt.print_lineage(
                        veldt.functions.keys().next().map(|s| s.as_str()).unwrap_or("(none)")); }
                    ":stats" => { let _ = veldt.load(); print_stats(&veldt); }
                    ":absorb" => { let _ = veldt.load();
                        veldt.merge_traces(&interp.traces);
                        absorb_traces(None); }
                    _ if trimmed.starts_with(":fuzz ") => {
                        let _ = veldt.load(); fuzz_family(trimmed[6..].trim());
                    }
                    ":help" | _ => {
                        println!(":help  this help"); println!(":quit  exit");
                        println!(":load <file>  run a file in this session");
                        println!(":garden  show first lineage · :stats census");
                        println!(":fuzz <name> boundary sweep · :absorb traces->probes");
                        println!("exprs print their value");
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

/// Fitness tournament across gardens (Phase 10): pull a pool's
/// families into the local garden, re-trial every variant, crown the
/// fittest per family (losers marked sick so natural decay follows),
/// then push the judged garden back — the pool accumulates the
/// "world's fittest" strains.
fn tournament(url: &str, only: Option<&str>) {
    let base = url.trim_end_matches('/');
    merge_garden(&format!("{}/api/garden", base));

    let mut veldt = veldt::Veldt::new();
    if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
    let families: Vec<String> = match only {
        Some(n) => vec![n.to_string()],
        None => veldt.functions.keys().cloned().collect(),
    };
    let mut crowned = 0u32;
    for name in &families {
        let ids: Vec<u32> = veldt.functions.get(name)
            .map(|vs| vs.iter().map(|e| e.id).collect()).unwrap_or_default();
        if ids.len() < 2 { continue; }
        // re-trial each variant, then crown the fittest
        for id in &ids {
            let mut e = veldt.functions[name].iter().find(|e| e.id == *id).cloned().unwrap();
            health::HealthChecker::check_at_birth(&mut e, &veldt);
            if let Some(vs) = veldt.functions.get_mut(name) {
                for v in vs.iter_mut() { if v.id == *id { *v = e.clone(); break; } }
            }
        }
        let best_id = veldt.get_fittest_variant(name).map(|e| e.id);
        if let Some(vs) = veldt.functions.get_mut(name) {
            for v in vs.iter_mut() {
                v.health = if Some(v.id) == best_id {
                    crowned += 1;
                    veldt::Health::Healthy
                } else {
                    veldt::Health::Sick("lost tournament".into())
                };
            }
        }
        if let Some(b) = best_id {
            println!("  {}: champion {}#{} — fitness {:.2}", name, name, b,
                veldt.functions[name].iter().find(|e| e.id == b).map(|e| e.fitness).unwrap_or(0.0));
        }
    }
    veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));
    println!("{} champions crowned; pushing winners back to {}", crowned, base);
    push_garden(base);
}

/// std:fuzz (vision): sweep the boundary corpus against every variant
/// of a family, group outputs, flag variants that diverge from the
/// consensus answer — boundary bugs evolution should kill.
/// Garden-wide boundary sweep — fuzz every family, summary report.
fn fuzz_all() {
    let mut veldt = veldt::Veldt::new();
    if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
    let names: Vec<String> = veldt.functions.keys().cloned().collect();
    println!("=== fuzz --all — {} families ===", names.len());
    for name in &names {
        // reuse the single-family sweep but quiet the per-variant banner
        let before = std::time::Instant::now();
        // call the same path; it prints its own report
        println!("--- {}", name);
        // inline call: fuzz_family reloads the garden — acceptable, keeps logic single-sourced
        let _ = before;
        drop(veldt);
        fuzz_family(name);
        veldt = veldt::Veldt::new();
        let _ = veldt.load();
    }
}

fn fuzz_family(name: &str) {
    let mut veldt = veldt::Veldt::new();
    if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
    let Some(vars) = veldt.functions.get(name) else {
        println!("No variants of '{}'", name); return;
    };
    let variants: Vec<veldt::FunctionEntry> = vars.clone();
    println!("=== fuzz {} — {} variants, boundary corpus ===", name, variants.len());
    let mut cases = 0u32;
    let mut divergences = 0u32;
    // sweep each param-position boundary value (up to 8 cases per arg slot)
    for case in 0..8 {
        // build args: boundary value for every param (any/unknown -> int)
        let Some(first) = variants.first() else { break };
        let mut args = Vec::new();
        let mut ok = true;
        for p in &first.params {
            match evolve::boundary_value(&p.typ, case) {
                Some(v) => args.push(v),
                None => { ok = false; break; }
            }
        }
        if !ok { break; }
        cases += 1;
        // run each variant, group outputs
        let mut interp = interpreter::Interpreter::with_step_limit(50000);
        interp.caps = interpreter::Caps::sandboxed();
        veldt.load_into_interpreter(&mut interp);
        let mut by_out: std::collections::HashMap<String, Vec<u32>> =
            std::collections::HashMap::new();
        for v in &variants {
            if !matches!(v.health, veldt::Health::Healthy) { continue; }
            let out = match interp.call_function(name, Some(v.id), args.clone()) {
                Ok(r) => serde_json::to_string(&r).unwrap_or_default(),
                Err(e) => format!("ERR:{}", e),
            };
            by_out.entry(out).or_default().push(v.id);
        }
        if by_out.len() > 1 {
            divergences += 1;
            println!("  case {} args {:?}:", case,
                args.iter().map(|a| format!("{:?}", a)).collect::<Vec<_>>().join(","));
            for (out, ids) in &by_out {
                println!("    {} -> #{}", out, ids.iter().map(|i| i.to_string())
                    .collect::<Vec<_>>().join(", #"));
            }
            // divergence -> probe: majority answer becomes expected, so
            // dissenting variants fail this case on their next trial —
            // fuzzing feeds selection, not just a report.
            if let Some((expected, _)) = by_out.iter().max_by_key(|(_, ids)| ids.len()) {
                if !expected.starts_with("ERR:") {
                    let arg_strs: Vec<String> = args.iter()
                        .map(|a| serde_json::to_string(a).unwrap_or_default()).collect();
                    let pool = veldt.probes.entry(name.to_string()).or_insert_with(Vec::new);
                    if !pool.iter().any(|p| p.args == arg_strs) {
                        pool.push(veldt::Probe {
                            args: arg_strs,
                            expected: expected.clone(),
                            niche: format!("fuzz:case{}", case),
                            born: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs()).unwrap_or(0),
                            scale: 0,
                        });
                        println!("    ^ probe planted — dissenters will fail it next trial");
                    }
                }
            }
        }
    }
    if divergences > 0 {
        veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));
    }
    println!("{} boundary cases, {} divergent", cases, divergences);
}

/// Show two variants' bodies side by side — lines present in only one
/// get a -/+ marker, shared lines a space. The IDE's diff, in the CLI.
fn diff_variants(a: &str, b: &str) {
    let parse = |s: &str| -> Option<(String, u32)> {
        let (n, i) = s.split_once('#')?;
        Some((n.to_string(), i.parse().ok()?))
    };
    let (an, aid) = parse(a).unwrap_or_else(|| { eprintln!("bad ref {} — use name#id", a); process::exit(1); });
    let (bn, bid) = parse(b).unwrap_or_else(|| { eprintln!("bad ref {} — use name#id", b); process::exit(1); });
    if an != bn { eprintln!("diff works inside one lineage"); process::exit(1); }
    let mut veldt = veldt::Veldt::new();
    if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
    let Some(vs) = veldt.functions.get(&an) else {
        eprintln!("No variants of '{}'", an); process::exit(1);
    };
    let ea = vs.iter().find(|e| e.id == aid).unwrap_or_else(|| { eprintln!("no {}#{}", an, aid); process::exit(1); });
    let eb = vs.iter().find(|e| e.id == bid).unwrap_or_else(|| { eprintln!("no {}#{}", bn, bid); process::exit(1); });
    let sa = crate::fmt::format_program(&ea.body);
    let sb = crate::fmt::format_program(&eb.body);
    if std::env::args().any(|a| a == "--unified") {
        // single interleaved diff — common context once, changes -/+
        let mut out: Vec<(char, String)> = Vec::new();
        for l in sa.lines() { out.push(('-', l.to_string())); }
        for l in sb.lines() { out.push(('+', l.to_string())); }
        let pa: Vec<&str> = sa.lines().collect();
        let pb: Vec<&str> = sb.lines().collect();
        // LCS-based alignment for a readable unified view
        let n = pa.len(); let m = pb.len();
        let mut dp = vec![vec![0usize; m + 1]; n + 1];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                dp[i][j] = if pa[i] == pb[j] { dp[i+1][j] + 1 }
                    else { dp[i+1][j].max(dp[i][j+1]) };
            }
        }
        println!("--- {}#{} (fitness {:.2})  +++ {}#{} (fitness {:.2})", an, aid, ea.fitness, bn, bid, eb.fitness);
        let (mut i, mut j) = (0usize, 0usize);
        while i < n && j < m {
            if pa[i] == pb[j] {
                println!(" {}", pa[i]); i += 1; j += 1;
            } else if dp[i+1][j] >= dp[i][j+1] {
                println!("-{}", pa[i]); i += 1;
            } else {
                println!("+{}", pb[j]); j += 1;
            }
        }
        while i < n { println!("-{}", pa[i]); i += 1; }
        while j < m { println!("+{}", pb[j]); j += 1; }
        return;
    }
    let la: std::collections::HashSet<&str> = sa.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    let lb: std::collections::HashSet<&str> = sb.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    println!("--- {}#{} (fitness {:.2})", an, aid, ea.fitness);
    for l in sa.lines() {
        let t = l.trim();
        println!("{}{}", if t.is_empty() { " " } else if lb.contains(t) { " " } else { "-" }, l);
    }
    println!("+++ {}#{} (fitness {:.2})", bn, bid, eb.fitness);
    for l in sb.lines() {
        let t = l.trim();
        println!("{}{}", if t.is_empty() { " " } else if la.contains(t) { " " } else { "+" }, l);
    }
}

/// std:telemetry (vision): convert recorded call traces into probes.
/// Every real call a family has served becomes synthetic evidence —
/// live production behavior feeds the selection pool.
fn absorb_traces(only: Option<&str>) {
    let mut veldt = veldt::Veldt::new();
    if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
    let families: Vec<String> = match only {
        Some(n) => vec![n.to_string()],
        None => veldt.traces.keys().cloned().collect(),
    };
    let mut added = 0u32;
    for name in &families {
        let Some(traces) = veldt.traces.get(name) else { continue; };
        let pool = veldt.probes.entry(name.clone()).or_insert_with(Vec::new);
        for t in traces {
            // dedupe: skip if an identical arg-set probe exists
            if pool.iter().any(|p| p.args == t.args) { continue; }
            let args: Vec<interpreter::Value> = t.args.iter()
                .filter_map(|s| serde_json::from_str(s).ok()).collect();
            if args.len() != t.args.len() { continue; }
            pool.push(veldt::Probe {
                args: t.args.clone(),
                expected: t.result.clone(),
                niche: evolve::Evolver::niche_signature(&args),
                born: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs()).unwrap_or(0),
                scale: 0,
            });
            added += 1;
        }
    }
    veldt.save().unwrap_or_else(|e| eprintln!("Save error: {}", e));
    println!("Absorbed {} traces into probes", added);
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
    // URL import — pull a garden JSON over HTTP
    if path.starts_with("http://") || path.starts_with("https://") {
        let client = match reqwest::blocking::Client::builder()
            .user_agent("Veldt/0.3").timeout(std::time::Duration::from_secs(30)).build() {
            Ok(c) => c,
            Err(e) => { eprintln!("HTTP client error: {}", e); process::exit(1); }
        };
        match client.get(path).send().and_then(|r| r.error_for_status()) {
            Ok(resp) => match resp.text() {
                Ok(body) => {
                    std::fs::write(&dest, &body).unwrap_or_else(|e| {
                        eprintln!("Import write failed: {}", e); process::exit(1);
                    });
                    println!("Garden imported from {}", path);
                }
                Err(e) => { eprintln!("Fetch body failed: {}", e); process::exit(1); }
            },
            Err(e) => { eprintln!("Fetch failed: {}", e); process::exit(1); }
        }
        return;
    }
    match std::fs::copy(path, &dest) {
        Ok(_) => println!("Garden imported from {}", path),
        Err(e) => eprintln!("Import failed: {}", e),
    }
}

/// Read a garden JSON from a file path or URL.
fn read_garden_json(path: &str) -> serde_json::Value {
    let body = if path.starts_with("http://") || path.starts_with("https://") {
        let client = reqwest::blocking::Client::builder()
            .user_agent("Veldt/0.3").timeout(std::time::Duration::from_secs(30))
            .build().expect("http client");
        client.get(path).send().and_then(|r| r.error_for_status())
            .and_then(|r| r.text()).unwrap_or_else(|e| {
                eprintln!("Fetch failed: {}", e); process::exit(1);
            })
    } else {
        std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("Read failed: {}", e); process::exit(1);
        })
    };
    serde_json::from_str(&body).unwrap_or_else(|e| {
        eprintln!("Bad garden json in {}: {}", path, e); process::exit(1);
    })
}

/// Run a program in a subprocess and capture its stdout — for /api/play.
pub fn run_program_capture(path: &str) -> String {
    let exe = std::env::current_exe().unwrap_or_else(|_| "veldt".into());
    match std::process::Command::new(exe).arg("run").arg(path).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(e) => format!("Error running {}: {}", path, e),
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
    // --steps N: override the interpreter step limit
    let cli_args: Vec<String> = env::args().collect();
    if let Some(pos) = cli_args.iter().position(|a| a == "--steps") {
        if let Some(n) = cli_args.get(pos + 1).and_then(|s| s.parse::<u64>().ok()) {
            interp.step_limit = n;
        }
    }
    // --sandbox: run the script with all side-effect capabilities denied
    if cli_args.iter().any(|a| a == "--sandbox") {
        interp.caps = interpreter::Caps::sandboxed();
    }
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
