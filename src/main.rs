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
                    run_program_vm(tmp.to_str().unwrap(),
                        args.iter().any(|a| a == "--evolve"));
                } else {
                    run_program(tmp.to_str().unwrap(), args.iter().any(|a| a == "--trace"), args.iter().any(|a| a == "--evolve"));
                }
                let _ = std::fs::remove_file(&tmp);
            } else if args.iter().any(|a| a == "--vm") {
                // Phase 11: bytecode VM executor (computational subset)
                let file = args[2..].iter().find(|a| a.ends_with(".veldt"))
                    .cloned().unwrap_or_else(|| args[2].clone());
                run_program_vm(&file,
                    args.iter().any(|a| a == "--evolve"));
            } else {
                run_program(&args[2], args.iter().any(|a| a == "--trace"), args.iter().any(|a| a == "--evolve"));
            }
        }
        // veldt compare --all — run every bench/*.veldt on both engines.
        "compare" if args.iter().any(|a| a == "--all") => {
            println!("{:>18} {:>12} {:>12} {:>8}",
                     "bench", "tree-walk", "--vm", "speedup");
            for entry in std::fs::read_dir("bench").unwrap() {
                let p = entry.unwrap().path();
                if p.extension().and_then(|e| e.to_str())
                    != Some("veldt") { continue; }
                let f = p.to_str().unwrap().to_string();
                let t0 = std::time::Instant::now();
                run_program(&f, false, false);
                let tree = t0.elapsed();
                let t1 = std::time::Instant::now();
                run_program_vm(&f, false);
                let vm = t1.elapsed();
                println!("{:>18} {:>10.0}ms {:>10.0}ms {:>7.1}x",
                         p.file_name().unwrap().to_str().unwrap(),
                         tree.as_millis(), vm.as_millis(),
                         tree.as_secs_f64() / vm.as_secs_f64().max(1e-9));
            }
        }
        // veldt compare prog.veldt — run on both engines, report times.
        "compare" => {
            let file = args[2..].iter().find(|a| a.ends_with(".veldt"))
                .cloned().unwrap_or_else(|| args[2].clone());
            println!("=== tree-walk ===");
            let t0 = std::time::Instant::now();
            run_program(&file, false, false);
            let tree = t0.elapsed();
            println!("=== bytecode VM + JIT ===");
            let t1 = std::time::Instant::now();
            run_program_vm(&file, false);
            let vm = t1.elapsed();
            let speed = tree.as_secs_f64() / vm.as_secs_f64().max(1e-9);
            println!("--- compare ---");
            println!("  tree-walk: {:?}", tree);
            println!("  --vm:      {:?}", vm);
            println!("  speedup:   {:.1}x", speed);
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
            } else if let Some(expr) = args.iter().position(|a| a == "--filter")
                .and_then(|p| args.get(p + 1)) {
                // --filter <expr> — test only families with matching variants
                let mut veldt = veldt::Veldt::new();
                if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
                let or_groups: Vec<Vec<&str>> = expr.split("||")
                    .map(|g| g.split("&&").map(|s| s.trim()).collect())
                    .collect();
                let names: Vec<String> = veldt.functions.iter()
                    .filter(|(name, vs)| vs.iter().any(|e|
                        or_groups.iter().any(|g|
                            g.iter().all(|c| veldt::variant_matches_in(e, name, c)))))
                    .map(|(n, _)| n.clone())
                    .collect();
                println!("[test --filter {} — {} families]", expr, names.len());
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
            if args[2] == "--filter" {
                // --filter <expr> — adopt the best-fitness variant matching the filter
                let expr = args.get(3).map(|s| s.as_str()).unwrap_or("");
                let mut veldt = veldt::Veldt::new();
                if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
                let or_groups: Vec<Vec<&str>> = expr.split("||")
                    .map(|g| g.split("&&").map(|s| s.trim()).collect())
                    .collect();
                let best = veldt.functions.iter()
                    .flat_map(|(name, vs)| vs.iter()
                        .filter(|e| or_groups.iter().any(|g|
                            g.iter().all(|c| veldt::variant_matches_in(e, name, c))))
                        .map(|e| (name.clone(), e.id, e.fitness)))
                    .fold(None::<(String, u32, f64)>, |acc, c| {
                        if acc.map(|a| c.2 > a.2).unwrap_or(true) { Some(c) } else { acc }
                    });
                match best {
                    Some((name, id, fit)) => {
                        println!("[adopt --filter {} — best: {}#{} fitness {:.2}]", expr, name, id, fit);
                        adopt_variant(&format!("{}#{}", name, id));
                    }
                    None => println!("no variants match '{}'", expr),
                }
            } else {
                adopt_variant(&args[2]);
            }
        }
        // veldt verify --all [--quarantine] — audit every multi-variant
        // family; --quarantine demotes divergent variants to Dying so
        // call_best stops dispatching to them.
        "verify" if args.iter().any(|a| a == "--all") => {
            let quarantine = args.iter().any(|a| a == "--quarantine");
            let mut veldt = veldt::Veldt::new();
            if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
            let or_groups: Option<Vec<Vec<String>>> = args.iter()
                .position(|a| a == "--filter")
                .and_then(|p| args.get(p + 1))
                .map(|expr| expr.split("||")
                    .map(|g| g.split("&&").map(|s| s.trim().to_string()).collect())
                    .collect());
            let names: Vec<String> = veldt.functions.iter()
                .filter(|(_, vs)| vs.len() >= 2
                    && or_groups.as_ref().map(|gs| vs.iter().any(|e|
                        gs.iter().any(|g|
                            g.iter().all(|c| veldt::variant_matches_in(e, name, c)))))
                    .unwrap_or(true))
                .map(|(n, _)| n.clone()).collect();
            if names.is_empty() {
                println!("No families with multiple variants.");
            } else {
                let strict = args.iter().any(|a| a == "--strict");
                println!("Auditing {} families...", names.len());
                let mut demoted = Vec::new();
                let mut divergences = 0;
                for n in &names {
                    if let Some(id) = verify_family_find_divergent(n) {
                        divergences += 1;
                        if quarantine {
                            if let Some(vs) = veldt.functions.get_mut(n) {
                                for v in vs.iter_mut() {
                                    if v.id == id {
                                        v.health = veldt::Health::Dying;
                                        demoted.push(format!("{}#{}", n, id));
                                    }
                                }
                            }
                        }
                    }
                }
                if quarantine && !demoted.is_empty() {
                    veldt.save().unwrap_or_else(
                        |e| eprintln!("Save error: {}", e));
                    println!("Quarantined (Dying): {}", demoted.join(", "));
                }
                if strict && divergences > 0 {
                    eprintln!("{} divergent variant(s) found", divergences);
                    process::exit(1);
                }
            }
        }
        // veldt verify <fn> — bounded equivalence check across variants:
        // evaluates each variant on a bounded input domain and reports
        // which provably agree with the fittest (stronger than probes,
        // honest without a symbolic solver).
        "verify" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt verify <fn> [--domain N]");
                process::exit(1);
            }
            verify_family(&args[2], &args);
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
            if args.get(3).map(|s| s.as_str()) == Some("--filter") {
                // --filter <expr> — tournament only families with matching variants
                let expr = args.get(4).map(|s| s.as_str()).unwrap_or("");
                let mut veldt = veldt::Veldt::new();
                if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
                let or_groups: Vec<Vec<&str>> = expr.split("||")
                    .map(|g| g.split("&&").map(|s| s.trim()).collect())
                    .collect();
                let names: Vec<String> = veldt.functions.iter()
                    .filter(|(name, vs)| vs.iter().any(|e|
                        or_groups.iter().any(|g|
                            g.iter().all(|c| veldt::variant_matches_in(e, name, c)))))
                    .map(|(n, _)| n.clone())
                    .collect();
                println!("[tournament --filter {} — {} families]", expr, names.len());
                for name in &names {
                    tournament(&args[2], Some(name));
                }
            } else {
                tournament(&args[2], args.get(3).map(|s| s.as_str()));
            }
        }
        "fuzz" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt fuzz <name>|--all  (boundary corpus vs every variant)");
                process::exit(1);
            }
            if args[2] == "--filter" {
                // --filter <expr> — fuzz only families with matching variants
                let expr = args.get(3).map(|s| s.as_str()).unwrap_or("");
                let mut veldt = veldt::Veldt::new();
                if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
                let or_groups: Vec<Vec<&str>> = expr.split("||")
                    .map(|g| g.split("&&").map(|s| s.trim()).collect())
                    .collect();
                let names: Vec<String> = veldt.functions.iter()
                    .filter(|(name, vs)| vs.iter().any(|e|
                        or_groups.iter().any(|g|
                            g.iter().all(|c| veldt::variant_matches_in(e, name, c)))))
                    .map(|(n, _)| n.clone())
                    .collect();
                println!("[fuzz --filter {} — {} families]", expr, names.len());
                for name in &names {
                    fuzz_family(name);
                }
            } else if args[2] == "--all" || args[2] == "--ci" {
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
            if args.get(3).map(|s| s.as_str()) == Some("--filter") {
                // --filter <expr> — diff the two best-matching variants in args[2]
                let expr = args.get(4).map(|s| s.as_str()).unwrap_or("");
                let mut veldt = veldt::Veldt::new();
                if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
                let or_groups: Vec<Vec<&str>> = expr.split("||")
                    .map(|g| g.split("&&").map(|s| s.trim()).collect())
                    .collect();
                let vs = veldt.functions.get(&args[2]);
                match vs {
                    Some(vs) => {
                        let mut matching: Vec<_> = vs.iter()
                            .filter(|e| or_groups.iter().any(|g|
                                g.iter().all(|c| veldt::variant_matches_in(e, name, c))))
                            .collect();
                        matching.sort_by(|a, b| b.fitness.partial_cmp(&a.fitness)
                            .unwrap_or(std::cmp::Ordering::Equal));
                        if matching.len() >= 2 {
                            let a = format!("{}#{}", args[2], matching[0].id);
                            let b = format!("{}#{}", args[2], matching[1].id);
                            println!("[diff --filter {} — {} vs {}]", expr, a, b);
                            diff_variants(&a, &b);
                        } else {
                            println!("need 2+ matching variants in '{}' — found {}", args[2], matching.len());
                        }
                    }
                    None => println!("family '{}' not found", args[2]),
                }
            } else {
                diff_variants(&args[2], &args[3]);
            }
        }
        "absorb" => {
            if args.get(2).map(|s| s.as_str()) == Some("--filter") {
                // --filter <expr> — absorb traces for families with matching variants
                let expr = args.get(3).map(|s| s.as_str()).unwrap_or("");
                let mut veldt = veldt::Veldt::new();
                if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
                let or_groups: Vec<Vec<&str>> = expr.split("||")
                    .map(|g| g.split("&&").map(|s| s.trim()).collect())
                    .collect();
                let names: Vec<String> = veldt.functions.iter()
                    .filter(|(name, vs)| vs.iter().any(|e|
                        or_groups.iter().any(|g|
                            g.iter().all(|c| veldt::variant_matches_in(e, name, c)))))
                    .map(|(n, _)| n.clone())
                    .collect();
                println!("[absorb --filter {} — {} families]", expr, names.len());
                for name in &names {
                    absorb_traces(Some(name));
                }
            } else {
                absorb_traces(args.get(2).map(|s| s.as_str()));
            }
        }
        "find" => {
            if args.len() < 3 {
                eprintln!("Usage: veldt find <substr> [--json]  (find local families by name)");
                process::exit(1);
            }
            let mut veldt = veldt::Veldt::new();
            if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
            let needle = args[2].to_lowercase();
            let or_groups: Option<Vec<Vec<&str>>> = args.iter()
                .position(|a| a == "--filter")
                .and_then(|p| args.get(p + 1))
                .map(|expr| expr.split("||")
                    .map(|g| g.split("&&").map(|s| s.trim()).collect())
                    .collect());
            let hits: Vec<_> = veldt.functions.iter()
                .filter(|(n, vs)| n.to_lowercase().contains(&needle)
                    && or_groups.as_ref().map(|gs| vs.iter().any(|e|
                        gs.iter().any(|g|
                            g.iter().all(|c| veldt::variant_matches_in(e, name, c)))))
                    .unwrap_or(true))
                .collect();
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
                // --sort <field> — order find results: fitness (default) | count | name
                let sort_field = args.iter().position(|a| a == "--sort")
                    .and_then(|p| args.get(p + 1)).map(|s| s.as_str()).unwrap_or("");
                let mut hits = hits;
                match sort_field {
                    "count" => hits.sort_by(|a, b| b.1.len().cmp(&a.1.len())),
                    "name" => hits.sort_by(|a, b| a.0.cmp(b.0)),
                    _ => hits.sort_by(|a, b| {
                        let at = a.1.iter().map(|e| e.fitness).fold(f64::NEG_INFINITY, f64::max);
                        let bt = b.1.iter().map(|e| e.fitness).fold(f64::NEG_INFINITY, f64::max);
                        bt.partial_cmp(&at).unwrap_or(std::cmp::Ordering::Equal)
                    }),
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
            let jit_census = args.iter().any(|a| a == "--jit");
            if jit_census {
                // Which fns compile to native — build a fresh VM with the
                // garden loaded and try each family's fittest variant.
                let mut v = vm::Vm::new();
                v.load_garden(&veldt).ok();
                let mut engine = crate::jit::JitEngine::new().unwrap();
                let verify = args.iter().any(|a| a == "--verify");
                println!("JIT coverage across {} families{}", veldt.functions.len(),
                         if verify { " (+ bounded verify)" } else { "" });
                let mut native = 0; let mut cold = 0;
                if verify {
                    for name in veldt.functions.keys() {
                        let j = engine.compile_fn(name, &v.fns).is_some();
                        let status = if j { "native" } else { "bytecode" };
                        let ok = verify_family_find_divergent(name).is_none();
                        println!("  {}: {} {}", name, status,
                                 if ok { "+ verified" } else { "+ DIVERGENT" });
                        if j { native += 1 } else { cold += 1 }
                    }
                    println!("{} native, {} bytecode", native, cold);
                    return;
                }
                let why = args.iter().any(|a| a == "--why");
                for (name, _) in &veldt.functions {
                    match engine.compile_fn(name, &v.fns) {
                        Some(_) => { println!("  {}: native", name); native += 1; }
                        None => {
                            let fit = v.fns.get(name).and_then(|vs|
                                vs.iter().max_by(|a, b|
                                    a.fitness.partial_cmp(&b.fitness)
                                        .unwrap_or(std::cmp::Ordering::Equal)));
                            let key = fit.map(|f|
                                format!("{}#{}", name, f.id));
                            let reason = key.and_then(|k|
                                engine.fail_at.get(&k).copied())
                                .and_then(|pc| fit
                                    .map(|f| format!(" at pc {}: {:?}",
                                        pc, f.code.code.get(pc))))
                                .unwrap_or_default();
                            println!("  {}: bytecode{}", name, reason);
                            cold += 1;
                        }
                    }
                }
                println!("{} native, {} bytecode ({:.0}% JIT-able)",
                         native, cold,
                         native as f64 / (native + cold).max(1) as f64 * 100.0);
                return;
            }
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
            } else if args.iter().any(|a| a == "--usage") {
                // --usage — only variants that actually get called
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter().filter(|e| e.usage_count > 0)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (used variants) ===");
                filtered.print_garden(sick_only, healthy_only);
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
            } else if let Some(expr) = args.iter().position(|a| a == "--filter")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --filter <expr> — composable variant query
                // expr splits on || (OR groups); each group splits on && (AND clauses)
                // fields: fitness age usage size args lifespan last_used niches
                //         health trust origin calls
                // ops: > < >= <= == != ~ (contains)
                let or_groups: Vec<Vec<&str>> = expr.split("||")
                    .map(|g| g.split("&&").map(|s| s.trim()).collect())
                    .collect();
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| or_groups.iter().any(|g|
                            g.iter().all(|c| veldt::variant_matches_in(e, name, c))))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                // --sort <field> composes with --filter — sort the filtered result
                if let Some(field) = args.iter().position(|a| a == "--sort")
                    .and_then(|p| args.get(p + 1)) {
                    for vs in filtered.functions.values_mut() {
                        veldt::sort_variants(vs, field);
                    }
                    println!("=== The Veldt (filter: {} | sort: {}) ===", expr, field);
                } else {
                    println!("=== The Veldt (filter: {}) ===", expr);
                }
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(field) = args.iter().position(|a| a == "--sort")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --sort <field> — sort variants within each family by a field
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let mut vs = vs.clone();
                    veldt::sort_variants(&mut vs, &field);
                    sorted.functions.insert(name.clone(), vs);
                }
                println!("=== The Veldt (sort: {}) ===", field);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--max_niche_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --max_niche_fam <name:max> — variants in this family with <= max niches
                let (fam, max_n) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<usize>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or((spec.clone(), 0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.niche_scores.len() <= max_n)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} niches <= {}) ===", fam, max_n);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--min_niche_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --min_niche_fam <name:min> — variants in this family with >= min niches
                let (fam, min_n) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<usize>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or((spec.clone(), 0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.niche_scores.len() >= min_n)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} niches >= {}) ===", fam, min_n);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--max_last_used_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --max_last_used_fam <name:max> — variants in this family with last_used <= max
                let (fam, max_lu) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<u64>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or((spec.clone(), 0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.last_used <= max_lu)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} last_used <= {}) ===", fam, max_lu);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--min_last_used_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --min_last_used_fam <name:min> — variants in this family with last_used >= min
                let (fam, min_lu) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<u64>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or((spec.clone(), 0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.last_used >= min_lu)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} last_used >= {}) ===", fam, min_lu);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--max_lifespan_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --max_lifespan_fam <name:max> — variants in this family with lifespan <= max
                let (fam, max_l) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<f64>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or((spec.clone(), 0.0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.lifespan <= max_l)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} lifespan <= {}) ===", fam, max_l);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--min_lifespan_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --min_lifespan_fam <name:min> — variants in this family with lifespan >= min
                let (fam, min_l) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<f64>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or((spec.clone(), 0.0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.lifespan >= min_l)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} lifespan >= {}) ===", fam, min_l);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--max_args_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --max_args_fam <name:max> — variants in this family with args <= max
                let (fam, max_a) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<usize>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or((spec.clone(), 0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.params.len() <= max_a)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} args <= {}) ===", fam, max_a);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--min_args_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --min_args_fam <name:min> — variants in this family with args >= min
                let (fam, min_a) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<usize>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or((spec.clone(), 0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.params.len() >= min_a)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} args >= {}) ===", fam, min_a);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--max_size_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --max_size_fam <name:max> — variants in this family with body <= max chars
                let (fam, max_s) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<usize>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or((spec.clone(), 0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| crate::fmt::format_program(&e.body).len() <= max_s)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} size <= {}) ===", fam, max_s);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--min_size_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --min_size_fam <name:min> — variants in this family with body >= min chars
                let (fam, min_s) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<usize>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or((spec.clone(), 0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| crate::fmt::format_program(&e.body).len() >= min_s)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} size >= {}) ===", fam, min_s);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--max_age_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --max_age_fam <name:max> — variants in this family with age <= max
                let (fam, max_a) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<u64>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or((spec.clone(), 0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.age <= max_a)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} age <= {}) ===", fam, max_a);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--min_age_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --min_age_fam <name:min> — variants in this family with age >= min
                let (fam, min_a) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<u64>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or((spec.clone(), 0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.age >= min_a)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} age >= {}) ===", fam, min_a);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--max_usage_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --max_usage_fam <name:max> — variants in this family with usage <= max
                let (fam, max_u) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<u64>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or((spec.clone(), 0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.usage_count <= max_u)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} usage <= {}) ===", fam, max_u);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--min_usage_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --min_usage_fam <name:min> — variants in this family with usage >= min
                let (fam, min_u) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<u64>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or((spec.clone(), 0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.usage_count >= min_u)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} usage >= {}) ===", fam, min_u);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--max_fitness_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --max_fitness_fam <name:max> — variants in this family with fitness <= max
                let (fam, max_f) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<f64>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or((spec.clone(), 0.0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.fitness <= max_f)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} fitness <= {}) ===", fam, max_f);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--min_fitness_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --min_fitness_fam <name:min> — variants in this family with fitness >= min
                let (fam, min_f) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<f64>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or((spec.clone(), 0.0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.fitness >= min_f)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} fitness >= {}) ===", fam, min_f);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--no_calls_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --no_calls_fam <name:builtin> — variants in this family that DON'T call this builtin
                let (fam, builtin) = spec.split_once(':')
                    .map(|(n, b)| (n.to_string(), b.to_string()))
                    .unwrap_or((spec.clone(), String::new()));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| !crate::fmt::format_program(&e.body).contains(&builtin))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} no calls {}) ===", fam, builtin);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--calls_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --calls_fam <name:builtin> — variants in this family that call this builtin
                let (fam, builtin) = spec.split_once(':')
                    .map(|(n, b)| (n.to_string(), b.to_string()))
                    .unwrap_or((spec.clone(), String::new()));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| crate::fmt::format_program(&e.body).contains(&builtin))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} calls {}) ===", fam, builtin);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(min_f) = args.iter().position(|a| a == "--fit_count")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<f64>().ok()) {
                // --fit_count <min> — families with at least 1 variant above fitness min
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| e.fitness >= min_f) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with fitness >= {}) ===", min_f);
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--risky_count") {
                // --risky_count — families with at least 1 risky variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| matches!(e.health, veldt::Health::Sick(_))
                        && e.trust == veldt::Trust::Seedling) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with risky) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--proven_count") {
                // --proven_count — families with at least 1 proven variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| e.health == veldt::Health::Healthy
                        && e.trust != veldt::Trust::Seedling) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with proven) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--underused_count") {
                // --underused_count — families with at least 1 underused variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| (e.usage_count as f64) < e.lifespan) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with underused) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--overworked_count") {
                // --overworked_count — families with at least 1 overworked variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| (e.usage_count as f64) > e.lifespan) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with overworked) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--aging_count") {
                // --aging_count — families with at least 1 aging variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| (e.age as f64) >= e.lifespan) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with aging) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--growing_count") {
                // --growing_count — families with at least 1 growing variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| (e.age as f64) < e.lifespan) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with growing) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--stale_count") {
                // --stale_count — families with at least 1 stale variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e|
                        veldt.run_count.saturating_sub(e.last_used) > 10) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with stale) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--zero_count") {
                // --zero_count — families with at least 1 zero-usage variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| e.usage_count == 0) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with unused) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--usage_count") {
                // --usage_count — families with at least 1 used variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| e.usage_count > 0) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with used) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--niche_count") {
                // --niche_count — families with at least 1 niche-covered variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| !e.niche_scores.is_empty()) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with niche-covered) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--llm_count") {
                // --llm_count — families with at least 1 LLM-bred variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| e.origin.contains("llm")) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with LLM-bred) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--bred_count") {
                // --bred_count — families with at least 1 bred variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| !e.origin.starts_with("seed")
                        && !e.origin.starts_with("immigrant")) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with bred) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--seed_count") {
                // --seed_count — families with at least 1 seed variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| e.origin.starts_with("seed")) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with seeds) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--immigrant_count") {
                // --immigrant_count — families with at least 1 immigrant variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| e.origin.starts_with("immigrant")) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with immigrants) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--established_count") {
                // --established_count — families with at least 1 Established variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| e.trust == veldt::Trust::Established) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with established) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--seedling_count") {
                // --seedling_count — families with at least 1 Seedling variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| e.trust == veldt::Trust::Seedling) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with seedling) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--canonical_count") {
                // --canonical_count — families with at least 1 Canonical variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| e.trust == veldt::Trust::Canonical) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with canonical) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--dying_count") {
                // --dying_count — families with at least 1 dying variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| e.health == veldt::Health::Dying) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with dying) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--healthy_count") {
                // --healthy_count — families with at least 1 healthy variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| e.health == veldt::Health::Healthy) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with healthy) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--sick_count") {
                // --sick_count — families with at least 1 sick variant
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.iter().any(|e| matches!(e.health, veldt::Health::Sick(_))) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (families with sick) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--trust_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --trust_fam <name> — variants in this family sorted by trust
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| {
                        let ra = match a.trust {
                            veldt::Trust::Canonical => 3,
                            veldt::Trust::Established => 2,
                            veldt::Trust::Seedling => 1,
                            _ => 0,
                        };
                        let rb = match b.trust {
                            veldt::Trust::Canonical => 3,
                            veldt::Trust::Established => 2,
                            veldt::Trust::Seedling => 1,
                            _ => 0,
                        };
                        rb.cmp(&ra)
                    });
                    sorted.functions.insert(fam.clone(), sorted_vs);
                }
                println!("=== The Veldt ({} most-trusted first) ===", fam);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--health_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --health_fam <name> — variants in this family sorted by health
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| {
                        let ra = match &a.health {
                            veldt::Health::Healthy => 2,
                            veldt::Health::Sick(_) => 1,
                            veldt::Health::Dying => 0,
                        };
                        let rb = match &b.health {
                            veldt::Health::Healthy => 2,
                            veldt::Health::Sick(_) => 1,
                            veldt::Health::Dying => 0,
                        };
                        rb.cmp(&ra)
                    });
                    sorted.functions.insert(fam.clone(), sorted_vs);
                }
                println!("=== The Veldt ({} healthiest first) ===", fam);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--origin_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --origin_fam <name> — variants in this family sorted by origin
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| a.origin.cmp(&b.origin));
                    sorted.functions.insert(fam.clone(), sorted_vs);
                }
                println!("=== The Veldt ({} by origin) ===", fam);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--id_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --id_fam <name> — variants in this family sorted by id (creation order)
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| a.id.cmp(&b.id));
                    sorted.functions.insert(fam.clone(), sorted_vs);
                }
                println!("=== The Veldt ({} creation order) ===", fam);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--niche_score_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --niche_score_fam <name> — variants in this family sorted by avg niche score
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| {
                        let ra = if a.niche_scores.is_empty() { 0.0 } else {
                            a.niche_scores.values().sum::<f64>() / a.niche_scores.len() as f64 };
                        let rb = if b.niche_scores.is_empty() { 0.0 } else {
                            b.niche_scores.values().sum::<f64>() / b.niche_scores.len() as f64 };
                        rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    sorted.functions.insert(fam.clone(), sorted_vs);
                }
                println!("=== The Veldt ({} highest-niche first) ===", fam);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--efficiency_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --efficiency_fam <name> — variants in this family sorted by fitness/size
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| {
                        let sa = crate::fmt::format_program(&a.body).len();
                        let sb = crate::fmt::format_program(&b.body).len();
                        let ra = if sa > 0 { a.fitness / sa as f64 } else { 0.0 };
                        let rb = if sb > 0 { b.fitness / sb as f64 } else { 0.0 };
                        rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    sorted.functions.insert(fam.clone(), sorted_vs);
                }
                println!("=== The Veldt ({} most-efficient first) ===", fam);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--size_rate_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --size_rate_fam <name> — variants in this family sorted by size/age
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| {
                        let ra = if a.age > 0 {
                            crate::fmt::format_program(&a.body).len() as f64 / a.age as f64
                        } else { 0.0 };
                        let rb = if b.age > 0 {
                            crate::fmt::format_program(&b.body).len() as f64 / b.age as f64
                        } else { 0.0 };
                        rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    sorted.functions.insert(fam.clone(), sorted_vs);
                }
                println!("=== The Veldt ({} fastest-growing first) ===", fam);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--usage_rate_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --usage_rate_fam <name> — variants in this family sorted by usage/age
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| {
                        let ra = if a.age > 0 { a.usage_count as f64 / a.age as f64 } else { 0.0 };
                        let rb = if b.age > 0 { b.usage_count as f64 / b.age as f64 } else { 0.0 };
                        rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    sorted.functions.insert(fam.clone(), sorted_vs);
                }
                println!("=== The Veldt ({} most-called-per-run first) ===", fam);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--fit_rate_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --fit_rate_fam <name> — variants in this family sorted by fitness/age
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| {
                        let ra = if a.age > 0 { a.fitness / a.age as f64 } else { 0.0 };
                        let rb = if b.age > 0 { b.fitness / b.age as f64 } else { 0.0 };
                        rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    sorted.functions.insert(fam.clone(), sorted_vs);
                }
                println!("=== The Veldt ({} fastest-learners first) ===", fam);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--lifespan_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --lifespan_fam <name> — variants in this family sorted by lifespan
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| b.lifespan.partial_cmp(&a.lifespan)
                        .unwrap_or(std::cmp::Ordering::Equal));
                    sorted.functions.insert(fam.clone(), sorted_vs);
                }
                println!("=== The Veldt ({} longest-lived first) ===", fam);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--args_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --args_fam <name> — variants in this family sorted by arg count
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| b.params.len().cmp(&a.params.len()));
                    sorted.functions.insert(fam.clone(), sorted_vs);
                }
                println!("=== The Veldt ({} most-args first) ===", fam);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--last_used_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --last_used_fam <name> — variants in this family sorted by last_used
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| b.last_used.cmp(&a.last_used));
                    sorted.functions.insert(fam.clone(), sorted_vs);
                }
                println!("=== The Veldt ({} most-recent first) ===", fam);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--niche_sort_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --niche_sort_fam <name> — variants in this family sorted by niche count
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| b.niche_scores.len().cmp(&a.niche_scores.len()));
                    sorted.functions.insert(fam.clone(), sorted_vs);
                }
                println!("=== The Veldt ({} broadest first) ===", fam);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--size_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --size_fam <name> — variants in this family sorted by code size
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b|
                        crate::fmt::format_program(&b.body).len()
                            .cmp(&crate::fmt::format_program(&a.body).len()));
                    sorted.functions.insert(fam.clone(), sorted_vs);
                }
                println!("=== The Veldt ({} largest first) ===", fam);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--usage_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --usage_fam <name> — variants in this family sorted by usage
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| b.usage_count.cmp(&a.usage_count));
                    sorted.functions.insert(fam.clone(), sorted_vs);
                }
                println!("=== The Veldt ({} most-called first) ===", fam);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--fit_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --fit_fam <name> — variants in this family sorted by fitness
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| b.fitness.partial_cmp(&a.fitness)
                        .unwrap_or(std::cmp::Ordering::Equal));
                    sorted.functions.insert(fam.clone(), sorted_vs);
                }
                println!("=== The Veldt ({} fittest first) ===", fam);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--age_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --age_fam <name> — variants in this family sorted by age
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| b.age.cmp(&a.age));
                    sorted.functions.insert(fam.clone(), sorted_vs);
                }
                println!("=== The Veldt ({} oldest first) ===", fam);
                sorted.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--source_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --source_fam <name:src> — variants from a source in this family
                let (fam, src) = spec.split_once(':')
                    .map(|(n, s)| (n.to_string(), s.to_string()))
                    .unwrap_or((spec.clone(), String::new()));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.origin.starts_with(&src))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} source:{}) ===", fam, src);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--niche_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --niche_fam <name> — variants with niche coverage in this family
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| !e.niche_scores.is_empty())
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} niche-covered) ===", fam);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--llm_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --llm_fam <name> — LLM-bred variants in this family
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.origin.contains("llm"))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} LLM-bred) ===", fam);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--bred_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --bred_fam <name> — bred variants in this family
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| !e.origin.starts_with("seed") && !e.origin.starts_with("immigrant"))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} bred) ===", fam);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--seed_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --seed_fam <name> — seed variants in this family
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.origin.starts_with("seed"))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} seeds) ===", fam);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--immigrant_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --immigrant_fam <name> — immigrant variants in this family
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.origin.starts_with("immigrant"))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} immigrants) ===", fam);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--established_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --established_fam <name> — established-trust variants in this family
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.trust == veldt::Trust::Established)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} established) ===", fam);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--seedling_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --seedling_fam <name> — seedling-trust variants in this family
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.trust == veldt::Trust::Seedling)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} seedling) ===", fam);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--canonical_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --canonical_fam <name> — canonical-trust variants in this family
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.trust == veldt::Trust::Canonical)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} canonical) ===", fam);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--dying_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --dying_fam <name> — dying variants in this family
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.health == veldt::Health::Dying)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} dying) ===", fam);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--healthy_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --healthy_fam <name> — healthy variants in this family
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.health == veldt::Health::Healthy)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} healthy) ===", fam);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(fam) = args.iter().position(|a| a == "--sick_fam")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --sick_fam <name> — sick variants in this family
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                if let Some(vs) = veldt.functions.get(&fam) {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| matches!(e.health, veldt::Health::Sick(_)))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                }
                println!("=== The Veldt ({} sick) ===", fam);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(target) = args.iter().position(|a| a == "--cousins")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --cousins <fam#id> — same-family variants with a different parent
                let (fam, id) = target.split_once('#')
                    .and_then(|(n, i)| i.parse::<u32>().ok().map(|id| (n.to_string(), id)))
                    .unwrap_or(("".to_string(), 0));
                let parent = veldt.functions.get(&fam)
                    .and_then(|vs| vs.iter().find(|e| e.id == id))
                    .map(|e| e.origin.clone());
                match parent {
                    Some(p) => {
                        let mut filtered = veldt::Veldt::new();
                        filtered.probes = veldt.probes.clone();
                        filtered.obituaries = veldt.obituaries.clone();
                        filtered.run_count = veldt.run_count;
                        filtered.garden_path = veldt.garden_path.clone();
                        if let Some(vs) = veldt.functions.get(&fam) {
                            let keep: Vec<_> = vs.iter()
                                .filter(|e| e.origin != p && e.id != id)
                                .cloned().collect();
                            if !keep.is_empty() { filtered.functions.insert(fam.clone(), keep); }
                        }
                        println!("=== The Veldt (cousins of {}) ===", target);
                        filtered.print_garden(sick_only, healthy_only);
                    }
                    None => println!("{} not found", target),
                }
            } else if let Some(target) = args.iter().position(|a| a == "--siblings")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --siblings <fam#id> — variants with the same parent as this variant
                let (fam, id) = target.split_once('#')
                    .and_then(|(n, i)| i.parse::<u32>().ok().map(|id| (n.to_string(), id)))
                    .unwrap_or(("".to_string(), 0));
                let parent = veldt.functions.get(&fam)
                    .and_then(|vs| vs.iter().find(|e| e.id == id))
                    .map(|e| e.origin.clone());
                match parent {
                    Some(p) if p != "seed" => {
                        let mut filtered = veldt::Veldt::new();
                        filtered.probes = veldt.probes.clone();
                        filtered.obituaries = veldt.obituaries.clone();
                        filtered.run_count = veldt.run_count;
                        filtered.garden_path = veldt.garden_path.clone();
                        for (name, vs) in &veldt.functions {
                            let keep: Vec<_> = vs.iter()
                                .filter(|e| e.origin == p && e.id != id)
                                .cloned().collect();
                            if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                        }
                        println!("=== The Veldt (siblings of {}) ===", target);
                        filtered.print_garden(sick_only, healthy_only);
                    }
                    _ => println!("{} has no siblings (seed or not found)", target),
                }
            } else if let Some(parent) = args.iter().position(|a| a == "--descendants")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --descendants <fam#id> — variants whose origin mentions this parent
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.origin.contains(&parent))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (descendants of {}) ===", parent);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--max_niche_score")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --max_niche_score <niche:max> — variants scoring <= max on this niche
                let (niche, max_s) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<f64>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or(("".to_string(), 0.0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.niche_scores.get(&niche)
                            .map(|s| *s <= max_s).unwrap_or(false))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt ({} <= {}) ===", niche, max_s);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(spec) = args.iter().position(|a| a == "--min_niche_score")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --min_niche_score <niche:min> — variants scoring >= min on this niche
                let (niche, min_s) = spec.split_once(':')
                    .and_then(|(n, s)| s.parse::<f64>().ok().map(|m| (n.to_string(), m)))
                    .unwrap_or(("".to_string(), 0.0));
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.niche_scores.get(&niche)
                            .map(|s| *s >= min_s).unwrap_or(false))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt ({} >= {}) ===", niche, min_s);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(act_n) = args.iter().position(|a| a == "--active")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<u64>().ok()) {
                // --active <n> — variants used within the last n runs
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| veldt.run_count.saturating_sub(e.last_used) <= act_n)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (active <= {} runs) ===", act_n);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(stale_n) = args.iter().position(|a| a == "--stale_after")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<u64>().ok()) {
                // --stale_after <n> — variants unused for > n runs (dormant)
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| veldt.run_count.saturating_sub(e.last_used) > stale_n)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (stale > {} runs) ===", stale_n);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(max_lu) = args.iter().position(|a| a == "--max_last_used")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<u64>().ok()) {
                // --max_last_used <n> — variants not used since run n (cold)
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.last_used <= max_lu)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (last_used <= {}) ===", max_lu);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(min_lu) = args.iter().position(|a| a == "--min_last_used")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<u64>().ok()) {
                // --min_last_used <n> — variants used since run n (recently called)
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.last_used >= min_lu)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (last_used >= {}) ===", min_lu);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(max_nc) = args.iter().position(|a| a == "--max_niche")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<usize>().ok()) {
                // --max_niche <n> — variants with <= n niche scores (narrow coverage)
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.niche_scores.len() <= max_nc)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (niches <= {}) ===", max_nc);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(min_nc) = args.iter().position(|a| a == "--min_niche")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<usize>().ok()) {
                // --min_niche <n> — variants with >= n niche scores (broad coverage)
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.niche_scores.len() >= min_nc)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (niches >= {}) ===", min_nc);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(max_a) = args.iter().position(|a| a == "--max_args")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<usize>().ok()) {
                // --max_args <n> — variants with <= n params (simple signatures)
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.params.len() <= max_a)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (args <= {}) ===", max_a);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(min_a) = args.iter().position(|a| a == "--min_args")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<usize>().ok()) {
                // --min_args <n> — variants with >= n params (complex signatures)
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.params.len() >= min_a)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (args >= {}) ===", min_a);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(min_rate) = args.iter().position(|a| a == "--fit_rate")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<f64>().ok()) {
                // --fit_rate <n> — variants with fitness/age >= n (fast-learners)
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.age > 0 && (e.fitness / e.age as f64) >= min_rate)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (fitness/age >= {}) ===", min_rate);
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--overworked") {
                // overworked variants — usage_count > lifespan (carrying the load)
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| (e.usage_count as f64) > e.lifespan)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (overworked) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--underused") {
                // underused variants — usage_count < lifespan (not pulling their weight)
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| (e.usage_count as f64) < e.lifespan)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (underused) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--aging") {
                // aging variants — age >= lifespan (past expected lifetime)
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| (e.age as f64) >= e.lifespan)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (aging) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--growing") {
                // growing variants — age < lifespan (still in their growth phase)
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| (e.age as f64) < e.lifespan)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (growing) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(max_s) = args.iter().position(|a| a == "--max_size")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<usize>().ok()) {
                // --max_size <n> — variants with body <= n chars
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| crate::fmt::format_program(&e.body).len() <= max_s)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (size <= {}) ===", max_s);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(min_s) = args.iter().position(|a| a == "--min_size")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<usize>().ok()) {
                // --min_size <n> — variants with body >= n chars
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| crate::fmt::format_program(&e.body).len() >= min_s)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (size >= {}) ===", min_s);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(arity) = args.iter().position(|a| a == "--arity")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<usize>().ok()) {
                // --arity <n> — variants with exactly n params
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.params.len() == arity)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (arity == {}) ===", arity);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(max_f) = args.iter().position(|a| a == "--max_fit")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<f64>().ok()) {
                // --max_fit <n> — variants with fitness <= n (the weak)
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.fitness <= max_f)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (fitness <= {}) ===", max_f);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(max_ls) = args.iter().position(|a| a == "--max_lifespan")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<f64>().ok()) {
                // --max_lifespan <n> — variants with lifespan <= n
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.lifespan <= max_ls)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (lifespan <= {}) ===", max_ls);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(min_ls) = args.iter().position(|a| a == "--min_lifespan")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<f64>().ok()) {
                // --min_lifespan <n> — variants with lifespan >= n
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.lifespan >= min_ls)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (lifespan >= {}) ===", min_ls);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(builtin) = args.iter().position(|a| a == "--no_calls")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --no_calls <builtin> — variants that DON'T call this builtin
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| !crate::fmt::format_program(&e.body)
                            .contains(&builtin))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (no calls: {}) ===", builtin);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(builtin) = args.iter().position(|a| a == "--calls")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --calls <builtin> — variants whose source calls this builtin
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| crate::fmt::format_program(&e.body)
                            .contains(&builtin))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (calls: {}) ===", builtin);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(probe) = args.iter().position(|a| a == "--failing")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --failing <input> — variants whose Sick fails include this input
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| {
                            if let veldt::Health::Sick(err) = &e.health {
                                err.contains(&probe)
                            } else { false }
                        }).cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (failing: {}) ===", probe);
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--risky") {
                // risky variants — sick + Seedling trust (untested + failing)
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| matches!(e.health, veldt::Health::Sick(_))
                            && e.trust == veldt::Trust::Seedling)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (risky) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--proven") {
                // proven variants — healthy + Established/Canonical trust
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.health == veldt::Health::Healthy
                            && e.trust != veldt::Trust::Seedling)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (proven) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(min_n) = args.iter().position(|a| a == "--niches")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<usize>().ok()) {
                // --niches <n> — families covering at least n niche types
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let niche_count: usize = vs.iter()
                        .flat_map(|e| e.niche_scores.keys())
                        .collect::<std::collections::HashSet<_>>().len();
                    if niche_count >= min_n {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (niches >= {}) ===", min_n);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(max_t) = args.iter().position(|a| a == "--max_traces")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<usize>().ok()) {
                // --max_traces <n> — families with at most n traces
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if veldt.traces.get(name).map(|t| t.len()).unwrap_or(0) <= max_t {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (traces <= {}) ===", max_t);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(min_t) = args.iter().position(|a| a == "--min_traces")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<usize>().ok()) {
                // --min_traces <n> — families with at least n traces
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if veldt.traces.get(name).map(|t| t.len()).unwrap_or(0) >= min_t {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (traces >= {}) ===", min_t);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(max_d) = args.iter().position(|a| a == "--max_deaths")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<usize>().ok()) {
                // --max_deaths <n> — families with at most n deaths
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let deaths = veldt.obituaries.iter().filter(|o| o.name == *name).count();
                    if deaths <= max_d {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (deaths <= {}) ===", max_d);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(min_d) = args.iter().position(|a| a == "--min_deaths")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<usize>().ok()) {
                // --min_deaths <n> — families with at least n deaths
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let deaths = veldt.obituaries.iter().filter(|o| o.name == *name).count();
                    if deaths >= min_d {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (deaths >= {}) ===", min_d);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(max_v) = args.iter().position(|a| a == "--max_variants")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<usize>().ok()) {
                // --max_variants <n> — families with at most n variants
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.len() <= max_v {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (variants <= {}) ===", max_v);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(min_v) = args.iter().position(|a| a == "--min_variants")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<usize>().ok()) {
                // --min_variants <n> — families with at least n variants
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.len() >= min_v {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (variants >= {}) ===", min_v);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(max_p) = args.iter().position(|a| a == "--max_probes")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<usize>().ok()) {
                // --max_probes <n> — families with at most n probes
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if veldt.probes.get(name).map(|p| p.len()).unwrap_or(0) <= max_p {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (probes <= {}) ===", max_p);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(min_p) = args.iter().position(|a| a == "--min_probes")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<usize>().ok()) {
                // --min_probes <n> — families with at least n probes
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if veldt.probes.get(name).map(|p| p.len()).unwrap_or(0) >= min_p {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (probes >= {}) ===", min_p);
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--multi") {
                // multi-variant families — the lineages with rivals
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.len() > 1 {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (multi-variant) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--single") {
                // single-variant families — the minimal lineages
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if vs.len() == 1 {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (single-variant) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--tested") {
                // families with >0 probes — the tested
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if veldt.probes.get(name).map(|p| !p.is_empty()).unwrap_or(false) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (tested) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--bare") {
                // families with 0 probes — the untested
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    if veldt.probes.get(name).map(|p| p.is_empty()).unwrap_or(true) {
                        filtered.functions.insert(name.clone(), vs.clone());
                    }
                }
                println!("=== The Veldt (bare) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--zero_usage") {
                // variants never called — the dead weight
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.usage_count == 0)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (never called) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(min_use) = args.iter().position(|a| a == "--min_usage")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<u64>().ok()) {
                // --min_usage <n> — variants called at least n times
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.usage_count >= min_use)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (usage >= {}) ===", min_use);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(max_age) = args.iter().position(|a| a == "--max_age")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<u64>().ok()) {
                // --max_age <n> — variants younger than n runs
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.age <= max_age)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (age <= {}) ===", max_age);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(min_age) = args.iter().position(|a| a == "--min_age")
                .and_then(|p| args.get(p + 1)).and_then(|s| s.parse::<u64>().ok()) {
                // --min_age <n> — variants older than n runs
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.age >= min_age)
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (age >= {}) ===", min_age);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(src) = args.iter().position(|a| a == "--source")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --source <src> — variants from this mutation source
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.origin.starts_with(&src))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (source: {}) ===", src);
                filtered.print_garden(sick_only, healthy_only);
            } else if let Some(niche) = args.iter().position(|a| a == "--niche")
                .and_then(|p| args.get(p + 1)).cloned() {
                // --niche <niche> — variants that score on this niche
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.niche_scores.contains_key(&niche))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (niche: {}) ===", niche);
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--llm") {
                // LLM-bred variants only — the BYOK mutations
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.origin.contains("llm"))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (LLM-bred) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--bred") {
                // bred variants only — mutation/crossover/recombine, not seeds/immigrants
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| !e.origin.starts_with("seed") && !e.origin.starts_with("immigrant"))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (bred) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--seed") {
                // seed variants only — the originals
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.origin.starts_with("seed"))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (seeds) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--immigrant") {
                // immigrant variants only — foreign-garden influence
                let mut filtered = veldt::Veldt::new();
                filtered.probes = veldt.probes.clone();
                filtered.obituaries = veldt.obituaries.clone();
                filtered.run_count = veldt.run_count;
                filtered.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| e.origin.starts_with("immigrant"))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.functions.insert(name.clone(), keep); }
                }
                println!("=== The Veldt (immigrants) ===");
                filtered.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--obit") {
                // most-deaths families first — sort families by obituary count
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                let mut fams: Vec<(&String, &Vec<veldt::FunctionEntry>)> =
                    veldt.functions.iter().collect();
                fams.sort_by(|a, b| {
                    let dc_b = veldt.obituaries.iter().filter(|o| o.name == **b.0).count();
                    let dc_a = veldt.obituaries.iter().filter(|o| o.name == **a.0).count();
                    dc_b.cmp(&dc_a)
                });
                for (name, vs) in fams {
                    sorted.functions.insert(name.clone(), vs.clone());
                }
                println!("=== The Veldt (most-deaths first) ===");
                sorted.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--trace") {
                // most-traced families first — sort families by trace volume
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                let mut fams: Vec<(&String, &Vec<veldt::FunctionEntry>)> =
                    veldt.functions.iter().collect();
                fams.sort_by(|a, b| veldt.traces.get(b.0.as_str()).map(|t| t.len()).unwrap_or(0)
                    .cmp(&veldt.traces.get(a.0.as_str()).map(|t| t.len()).unwrap_or(0)));
                for (name, vs) in fams {
                    sorted.functions.insert(name.clone(), vs.clone());
                }
                println!("=== The Veldt (most-traced first) ===");
                sorted.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--probes") {
                // most-probed families first — sort families by probe count desc
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                let mut fams: Vec<(&String, &Vec<veldt::FunctionEntry>)> =
                    veldt.functions.iter().collect();
                fams.sort_by(|a, b| veldt.probes.get(b.0.as_str()).map(|p| p.len()).unwrap_or(0)
                    .cmp(&veldt.probes.get(a.0.as_str()).map(|p| p.len()).unwrap_or(0)));
                for (name, vs) in fams {
                    sorted.functions.insert(name.clone(), vs.clone());
                }
                println!("=== The Veldt (most-probed first) ===");
                sorted.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--args") {
                // most-args first — sort each family by param count desc
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| b.params.len().cmp(&a.params.len()));
                    sorted.functions.insert(name.clone(), sorted_vs);
                }
                println!("=== The Veldt (most-args first) ===");
                sorted.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--worst") {
                // weakest first — sort each family by fitness asc (triage view)
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| a.fitness.partial_cmp(&b.fitness)
                        .unwrap_or(std::cmp::Ordering::Equal));
                    sorted.functions.insert(name.clone(), sorted_vs);
                }
                println!("=== The Veldt (weakest first) ===");
                sorted.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--best") {
                // champions first — sort each family by fitness desc
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| b.fitness.partial_cmp(&a.fitness)
                        .unwrap_or(std::cmp::Ordering::Equal));
                    sorted.functions.insert(name.clone(), sorted_vs);
                }
                println!("=== The Veldt (champions first) ===");
                sorted.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--narrow") {
                // narrowest variants first — sort each family by niche count asc
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| a.niche_scores.len().cmp(&b.niche_scores.len()));
                    sorted.functions.insert(name.clone(), sorted_vs);
                }
                println!("=== The Veldt (narrowest first) ===");
                sorted.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--wide") {
                // widest variants first — sort each family by niche count desc
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| b.niche_scores.len().cmp(&a.niche_scores.len()));
                    sorted.functions.insert(name.clone(), sorted_vs);
                }
                println!("=== The Veldt (widest first) ===");
                sorted.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--cold") {
                // least-called variants first — sort each family by usage_count asc
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| a.usage_count.cmp(&b.usage_count));
                    sorted.functions.insert(name.clone(), sorted_vs);
                }
                println!("=== The Veldt (least-called first) ===");
                sorted.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--hot") {
                // most-called variants first — sort each family by usage_count desc
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| b.usage_count.cmp(&a.usage_count));
                    sorted.functions.insert(name.clone(), sorted_vs);
                }
                println!("=== The Veldt (most-called first) ===");
                sorted.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--small") {
                // smallest variants first — sort each family by body size asc
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b|
                        crate::fmt::format_program(&a.body).len()
                            .cmp(&crate::fmt::format_program(&b.body).len()));
                    sorted.functions.insert(name.clone(), sorted_vs);
                }
                println!("=== The Veldt (smallest first) ===");
                sorted.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--big") {
                // largest variants first — sort each family by body size
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b|
                        crate::fmt::format_program(&b.body).len()
                            .cmp(&crate::fmt::format_program(&a.body).len()));
                    sorted.functions.insert(name.clone(), sorted_vs);
                }
                println!("=== The Veldt (largest first) ===");
                sorted.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--stale") {
                // stale variants first — sort each family by last_used asc
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| a.last_used.cmp(&b.last_used));
                    sorted.functions.insert(name.clone(), sorted_vs);
                }
                println!("=== The Veldt (stale first) ===");
                sorted.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--young") {
                // newest variants first — sort each family by age asc
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| a.age.cmp(&b.age));
                    sorted.functions.insert(name.clone(), sorted_vs);
                }
                println!("=== The Veldt (newest first) ===");
                sorted.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--old") {
                // oldest variants first — sort each family by age desc
                let mut sorted = veldt::Veldt::new();
                sorted.probes = veldt.probes.clone();
                sorted.obituaries = veldt.obituaries.clone();
                sorted.run_count = veldt.run_count;
                sorted.garden_path = veldt.garden_path.clone();
                for (name, vs) in &veldt.functions {
                    let mut sorted_vs = vs.clone();
                    sorted_vs.sort_by(|a, b| b.age.cmp(&a.age));
                    sorted.functions.insert(name.clone(), sorted_vs);
                }
                println!("=== The Veldt (oldest first) ===");
                sorted.print_garden(sick_only, healthy_only);
            } else if args.iter().any(|a| a == "--dead") {
                // extinct lineages — names in obituaries but not in functions
                let extinct: std::collections::BTreeSet<&String> = veldt.obituaries.iter()
                    .map(|o| &o.name)
                    .filter(|n| !veldt.functions.contains_key(*n))
                    .collect();
                println!("=== Extinct lineages ({} lineages) ===", extinct.len());
                for n in &extinct {
                    let deaths: Vec<&veldt::Obituary> = veldt.obituaries.iter()
                        .filter(|o| o.name == **n).collect();
                    let last = deaths.iter().map(|o| o.age).max().unwrap_or(0);
                    println!("  {} — {} variants died, last at age {}", n, deaths.len(), last);
                }
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
            if let Some(expr) = args.iter().position(|a| a == "--filter")
                .and_then(|p| args.get(p + 1)) {
                // --filter <expr> — only show matching variants in the lineage
                let or_groups: Vec<Vec<&str>> = expr.split("||")
                    .map(|g| g.split("&&").map(|s| s.trim()).collect())
                    .collect();
                if let Some(vs) = veldt.functions.get_mut(&args[2]) {
                    vs.retain(|e| or_groups.iter().any(|g|
                        g.iter().all(|c| veldt::variant_matches_in(e, name, c))));
                }
                println!("[lineage --filter {}]", expr);
            }
            if let Some(field) = args.iter().position(|a| a == "--sort")
                .and_then(|p| args.get(p + 1)) {
                // --sort <field> — sort the lineage by a field
                if let Some(vs) = veldt.functions.get_mut(&args[2]) {
                    veldt::sort_variants(vs, field);
                }
                println!("[lineage --sort {}]", field);
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
            if let Some(expr) = args.iter().position(|a| a == "--filter")
                .and_then(|p| args.get(p + 1)) {
                let or_groups: Vec<Vec<&str>> = expr.split("||")
                    .map(|g| g.split("&&").map(|s| s.trim()).collect())
                    .collect();
                let mut filtered = std::collections::HashMap::new();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| or_groups.iter().any(|g|
                            g.iter().all(|c| veldt::variant_matches_in(e, name, c))))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.insert(name.clone(), keep); }
                }
                veldt.functions = filtered;
                println!("[stats --filter {}]", expr);
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
                let mut use_count: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
                let mut fit_by_source: std::collections::HashMap<String, Vec<f64>> = std::collections::HashMap::new();
                let mut mut_rate: std::collections::HashMap<String, (usize, usize)> = std::collections::HashMap::new();
                let mut niche_sum: std::collections::HashMap<String, (f64, usize)> = std::collections::HashMap::new();
                const BUILTINS: &[&str] = &[
                    "canvas", "grid_", "ui_", "bt_", "balance", "astar", "bfs", "los",
                    "fov", "rand", "roll", "dice", "choice", "weighted", "noise", "fbm",
                    "perlin", "poisson", "voronoi", "delaunay", "wfc", "maze", "flood",
                    "neighbor", "lerp", "clamp", "dist", "aabb", "smoothstep", "bezier",
                    "point_in_poly", "convex", "ease", "bounce", "line_points", "turn",
                    "fsm", "book", "svc_", "robot", "edge", "game_loop", "dist_stats",
                ];
                for vs in veldt.functions.values() {
                    for e in vs {
                        let src = crate::fmt::format_program(&e.body);
                        for b in BUILTINS { if src.contains(b) { *use_count.entry(b).or_default() += 1; } }
                        let origin = e.origin.split(' ').next().unwrap_or("seed").to_string();
                        fit_by_source.entry(origin.clone()).or_default().push(e.fitness);
                        let ent = mut_rate.entry(origin).or_default();
                        if e.health == veldt::Health::Healthy { ent.0 += 1; }
                        ent.1 += 1;
                        for (n, s) in &e.niche_scores {
                            let ent = niche_sum.entry(n.clone()).or_default();
                            ent.0 += s; ent.1 += 1;
                        }
                    }
                }
                println!("{}", serde_json::json!({
                    "lineages": veldt.functions.len(), "variants": total,
                    "healthy": healthy, "sick": sick, "dying": dying,
                    "seedling": seed, "established": estab, "canonical": canon,
                    "probes": veldt.probes.values().map(|v| v.len()).sum::<usize>(),
                    "deaths": veldt.obituaries.len(), "runs": veldt.run_count,
                    "builtin_usage": use_count.iter().map(|(b, c)| serde_json::json!({(*b): c})).collect::<Vec<_>>(),
                    "fitness_by_source": fit_by_source.iter().map(|(s, fits)| serde_json::json!({
                        s.as_str(): fits.iter().sum::<f64>() / fits.len().max(1) as f64,
                    })).collect::<Vec<_>>(),
                    "mutation_success": mut_rate.iter().map(|(s, (h, t))| serde_json::json!({
                        s.as_str(): *h as f64 / (*t).max(1) as f64,
                    })).collect::<Vec<_>>(),
                    "avg_niche_scores": niche_sum.iter().map(|(n, (s, c))| serde_json::json!({
                        n.as_str(): *s / (*c).max(1) as f64,
                    })).collect::<Vec<_>>(),
                    "probe_coverage": veldt.probes.iter().map(|(n, p)| serde_json::json!({n.as_str(): p.len()})).collect::<Vec<_>>(),
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
            if let Some(expr) = args.iter().position(|a| a == "--filter")
                .and_then(|p| args.get(p + 1)) {
                let or_groups: Vec<Vec<&str>> = expr.split("||")
                    .map(|g| g.split("&&").map(|s| s.trim()).collect())
                    .collect();
                let mut filtered = std::collections::HashMap::new();
                for (name, vs) in &veldt.functions {
                    let keep: Vec<_> = vs.iter()
                        .filter(|e| or_groups.iter().any(|g|
                            g.iter().all(|c| veldt::variant_matches_in(e, name, c))))
                        .cloned().collect();
                    if !keep.is_empty() { filtered.insert(name.clone(), keep); }
                }
                veldt.functions = filtered;
                println!("[bench --filter {}]", expr);
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
            evolver.filter = args.iter().position(|a| a == "--filter")
                .and_then(|p| args.get(p + 1)).cloned();
            evolver.sort = args.iter().position(|a| a == "--sort")
                .and_then(|p| args.get(p + 1)).cloned();
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
        "garden" => "veldt garden [--sick|--obituaries|--json|--filter <expr>] — census\n  --filter <expr> — composable query: fields fitness age usage size args\n    lifespan last_used niches health trust origin calls; ops > < >= <= == != ~\n    `&&` for AND, `||` for OR. e.g. --filter \"fitness > 1.5 && age < 100\"\n  veldt lineage <name> · veldt stats [--json] · veldt test <name>|--all\n  veldt adopt <name#N> · veldt fuzz <name>|--all · veldt find <substr>\n  veldt diff <a#n> <b#n> · veldt absorb · veldt tournament <url>",
        "builtins" => "len type str int float print eprint input read_stdin args exit\n  abs max min round floor ceil sqrt pow rand date now sleep env\n  fetch post serve match_path read_file write_file ls exists\n  dict get put has keys values items merge delete get_or\n  re_match re_find re_replace re_split re_captures format\n  canvas_* grid_* ui_* bt_* balance_* astar* bfs los fov turn fsm_next\n  rand* roll dice choice weighted_choice noise2 fbm perlin poisson_disc\n  voronoi delaunay wfc maze_gen flood_fill neighbors lerp clamp dist aabb\n  smoothstep bezier point_in_poly convex_hull ease_* bounce line_points",
        "strings" => "s.len() s.trim() s.ltrim() s.rtrim() s.to_lower() s.to_upper()\n  s.split(sep) s.replace(a,b) s.contains(x) s.starts_with(p) s.ends_with(p)\n  s.chars() s.repeat(n) s.pad(n[,ch]) s.split_at(i) s[a:b]",
        "lists" => "xs.len() xs.push(v) xs.pop() xs.contains(v) xs.join(sep)\n  xs.enumerate() xs.zip(ys) xs.flatten() xs.take(n) xs.skip(n)\n  xs.sort() xs.sort_by(f) xs.group_by(f) xs.min_by(f) xs.max_by(f)\n  xs.reverse() xs.index_of(v) xs[a:b] xs[-1]",
        "types" => "int float str bool list dict set null fn — types are advisory docs\n  `x in xs` membership · `??` null-coalesce · `?.` safe access · `|>` pipe",
        "registry" => "veldt registry [port] — host a garden pool · veldt push <url> · veldt pull <url> [--merge]\n  veldt search <url> <name> · veldt tournament <url> — pull, trial, push winners back\n  veldt import <file|url> [--merge] · veldt export [file] — garden JSON",
        "sandbox" => "veldt run --sandbox <file> · veldt watch <file> --sandbox — Stage-0 caps (fs/net/proc denied)\n  sandbox_run(\"fn\", args) builtin · shadow_call(\"fn\", args) — all variants run sandboxed\n  Trials/probes/fuzz already run sandboxed — untrusted code can't touch the host",
        "filter" => "--filter <expr> — the variant query DSL (usable in garden/evolve/stats/test/fuzz/find/lineage/adopt/diff/verify/bench/tournament/absorb + /api/filter)\n  fields: fitness age usage size args lifespan last_used niches health trust origin calls\n  ops: > < >= <= == != ~ (contains) | connectors: && (AND) || (OR) | prefix ! or 'not ' (NOT)\n  e.g. --filter \"fitness > 1.5 && age < 100\" · \"origin ~ immigrant || origin ~ llm\" · \"!health == sick\"\n  --sort <field> — sort variants (garden/stats/lineage/find/evolve + /api/filter?sort=)\n  fields: fitness age usage size args lifespan last_used niches fit_rate usage_rate\n          size_rate efficiency niche_score id origin health trust | e.g. --sort fit_rate",
        _ => "veldt help <topic> — run, repl, agent, evolve, garden, builtins, strings, lists, types, registry, sandbox, filter",
    };
    println!("{}", text);
}

fn print_stats(veldt: &veldt::Veldt) {
    let mut total = 0usize;
    let mut healthy = 0usize;
    let mut sick = 0usize;
    let mut dying = 0usize;
    // --sort <field> — compute a per-family sort key; `best` carries
    // (name, sort_key, peak_fitness, count) for the print.
    let sort_field = std::env::args().nth(
        std::env::args().position(|a| a == "--sort").map(|p| p + 1).unwrap_or(usize::MAX))
        .unwrap_or_default();
    let mut best: Vec<(String, f64, f64, usize)> = Vec::new();
    for (name, variants) in &veldt.functions {
        let top = variants.iter().map(|e| e.fitness)
            .fold(f64::NEG_INFINITY, f64::max);
        let n = variants.len().max(1) as f64;
        let key = match sort_field.as_str() {
            "count" => variants.len() as f64,
            "avg_age" => variants.iter().map(|e| e.age as f64).sum::<f64>() / n,
            "usage" => variants.iter().map(|e| e.usage_count).sum::<u64>() as f64,
            "size" => variants.iter().map(|e|
                crate::fmt::format_program(&e.body).len() as f64).sum::<f64>() / n,
            "probes" => veldt.probes.get(name).map(|p| p.len()).unwrap_or(0) as f64,
            "deaths" => veldt.obituaries.iter().filter(|o| o.name == *name).count() as f64,
            "traces" => veldt.traces.get(name).map(|t| t.len()).unwrap_or(0) as f64,
            "immigrants" => variants.iter().filter(|e| e.origin.contains("immigrant")).count() as f64,
            "sick" => variants.iter().filter(|e| matches!(e.health, Health::Sick(_))).count() as f64,
            "healthy" => variants.iter().filter(|e| matches!(e.health, Health::Healthy)).count() as f64,
            "dying" => variants.iter().filter(|e| matches!(e.health, Health::Dying)).count() as f64,
            "healing" => variants.iter().map(|e| e.healing_attempts.len() as f64).sum::<f64>() / n,
            "proven" => variants.iter().filter(|e| matches!(e.health, Health::Healthy)
                && matches!(e.trust, veldt::Trust::Established | veldt::Trust::Canonical)).count() as f64,
            "risky" => variants.iter().filter(|e| matches!(e.health, Health::Sick(_))
                && matches!(e.trust, veldt::Trust::Seedling)).count() as f64,
            "stale" => {
                // staleness — runs since the family's most-recent call
                let max_lu = variants.iter().map(|e| e.last_used).max().unwrap_or(0);
                veldt.run_count.saturating_sub(max_lu) as f64
            }
            "canonical" => variants.iter().filter(|e| matches!(e.trust, veldt::Trust::Canonical)).count() as f64,
            "established" => variants.iter().filter(|e| matches!(e.trust, veldt::Trust::Established)).count() as f64,
            "seedling" => variants.iter().filter(|e| matches!(e.trust, veldt::Trust::Seedling)).count() as f64,
            "niches" => variants.iter().map(|e| e.niche_scores.len() as f64).sum::<f64>() / n,
            "args" => variants.iter().map(|e| e.params.len() as f64).sum::<f64>() / n,
            "lifespan" => variants.iter().map(|e| e.lifespan).sum::<f64>() / n,
            "last_used" => variants.iter().map(|e| e.last_used).max().unwrap_or(0) as f64,
            "fit_rate" => variants.iter().map(|e| e.fitness / (e.age as f64 + 1.0)).sum::<f64>() / n,
            "usage_rate" => variants.iter().map(|e| e.usage_count as f64 / (e.age as f64 + 1.0)).sum::<f64>() / n,
            "efficiency" => variants.iter().map(|e|
                e.fitness / (crate::fmt::format_program(&e.body).len() as f64 + 1.0)).sum::<f64>() / n,
            "niche_score" => variants.iter().map(|e|
                e.niche_scores.values().sum::<f64>() / e.niche_scores.len().max(1) as f64).sum::<f64>() / n,
            "trial_score" => variants.iter().map(|e| e.trial_score).sum::<f64>() / n,
            "efficiency" => variants.iter().map(|e| e.efficiency).sum::<f64>() / n,
            "born" => variants.iter().map(|e| e.born).max().unwrap_or(0) as f64,
            "parents" => variants.iter().map(|e| e.parents.len() as f64).sum::<f64>() / n,
            _ if sort_field.starts_with("niche:") => {
                // niche:<name> — per-family avg score on a specific niche
                let niche = &sort_field[6..];
                variants.iter().map(|e|
                    e.niche_scores.get(niche).copied().unwrap_or(0.0)).sum::<f64>() / n
            }
            "name" => 0.0, // sorted separately below
            _ => if variants.is_empty() { 0.0 } else { top }, // "fitness" default
        };
        best.push((name.clone(), key, if variants.is_empty() { 0.0 } else { top }, variants.len()));
        for e in variants {
            total += 1;
            match e.health {
                Health::Healthy => healthy += 1,
                Health::Sick(_) => sick += 1,
                Health::Dying => dying += 1,
            }
        }
    }
    if sort_field == "name" {
        best.sort_by(|a, b| a.0.cmp(&b.0));
    } else {
        best.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    }
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
    if std::env::args().any(|a| a == "--top") {
        let mut fam: Vec<(&String, f64)> = veldt.functions.iter()
            .map(|(n, vs)| (n, vs.iter().map(|e| e.fitness)
                .fold(f64::NEG_INFINITY, f64::max))).collect();
        fam.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        for (n, f) in fam.iter().take(5) {
            println!("  {} peak {:.2}", n, f);
        }
        if veldt.fitness_history.len() >= 2 {
            let trend = veldt.fitness_history.iter().map(|f| format!("{:.2}", f))
                .collect::<Vec<_>>();
            let arrow = if veldt.fitness_history.last() > veldt.fitness_history.first() { "↑" }
                        else if veldt.fitness_history.last() < veldt.fitness_history.first() { "↓" } else { "→" };
            println!("fitness trend: {} → {} {}", trend.first().unwrap(), trend.last().unwrap(), arrow);
        }
        return;
    }
    if std::env::args().any(|a| a == "--brief") {
        if veldt.fitness_history.len() >= 2 {
            let trend = veldt.fitness_history.iter().map(|f| format!("{:.2}", f))
                .collect::<Vec<_>>();
            let arrow = if veldt.fitness_history.last() > veldt.fitness_history.first() { "↑" }
                        else if veldt.fitness_history.last() < veldt.fitness_history.first() { "↓" } else { "→" };
            println!("fitness trend: {} → {} {}", trend.first().unwrap(), trend.last().unwrap(), arrow);
        }
        return;
    }
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
    // avg variant age — turnover: young garden = active evolution
    let ages: Vec<u64> = veldt.functions.values()
        .flat_map(|vs| vs.iter().map(|e| e.age)).collect();
    if !ages.is_empty() {
        let avg = ages.iter().sum::<u64>() / ages.len() as u64;
        let max = ages.iter().max().unwrap_or(&0);
        let min = ages.iter().min().unwrap_or(&0);
        println!("variant age: avg {} ({}..{}) — {}% under 20 runs",
            avg, min, max,
            ages.iter().filter(|a| **a < 20).count() * 100 / ages.len());
    }
    // trace volume — production evidence per family
    let mut trace_count: Vec<(&String, usize)> = veldt.traces.iter()
        .map(|(n, t)| (n, t.len())).collect();
    trace_count.sort_by(|a, b| b.1.cmp(&a.1));
    if !trace_count.is_empty() {
        let line: Vec<String> = trace_count.iter().take(8)
            .map(|(n, c)| format!("{}:{}", n, c)).collect();
        println!("call traces: {}", line.join(" "));
    }
    // usage distribution — which variants actually get called
    let mut usage: Vec<(String, u64)> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().map(move |e| (format!("{}#{}", n, e.id), e.usage_count)))
        .collect();
    usage.sort_by(|a, b| b.1.cmp(&a.1));
    let nonzero: Vec<_> = usage.into_iter().filter(|(_, u)| *u > 0).take(8).collect();
    if !nonzero.is_empty() {
        let line: Vec<String> = nonzero.iter()
            .map(|(n, u)| format!("{}:{}", n, u)).collect();
        println!("usage: {}", line.join(" "));
    }
    // death causes — why variants die
    let mut causes: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for o in &veldt.obituaries {
        *causes.entry(o.cause_of_death.clone()).or_default() += 1;
    }
    let mut cause_list: Vec<(String, usize)> = causes.into_iter().collect();
    cause_list.sort_by(|a, b| b.1.cmp(&a.1));
    if !cause_list.is_empty() {
        let line: Vec<String> = cause_list.iter().take(6)
            .map(|(c, n)| format!("{}:{}", c, n)).collect();
        println!("death causes: {}", line.join(" "));
    }
    // avg death age — how old variants are when they die
    if !veldt.obituaries.is_empty() {
        let ages: Vec<u64> = veldt.obituaries.iter().map(|o| o.age).collect();
        let avg = ages.iter().sum::<u64>() / ages.len() as u64;
        let (lo, hi) = (*ages.iter().min().unwrap_or(&0), *ages.iter().max().unwrap_or(&0));
        println!("death age: avg {} ({}..{}) over {} deaths",
            avg, lo, hi, ages.len());
    }
    // healing attempts — how often variants tried to heal before dying
    if !veldt.obituaries.is_empty() {
        let attempts: Vec<usize> = veldt.obituaries.iter()
            .map(|o| o.healing_attempts.len()).collect();
        let avg = attempts.iter().sum::<usize>() / attempts.len().max(1);
        let max = attempts.iter().max().unwrap_or(&0);
        let healed_once = attempts.iter().filter(|a| **a > 0).count();
        println!("healing: {} of {} deaths tried (avg {} attempts, max {})",
            healed_once, attempts.len(), avg, max);
    }
    // avg usage per variant — selection pressure intensity
    let used: Vec<u64> = veldt.functions.values()
        .flat_map(|vs| vs.iter().map(|e| e.usage_count)).collect();
    if !used.is_empty() {
        let avg = used.iter().sum::<u64>() / used.len() as u64;
        let called = used.iter().filter(|u| **u > 0).count();
        println!("usage rate: avg {} calls/variant ({} of {} called)",
            avg, called, used.len());
    }
    // avg family size — genetic diversity per lineage
    let sizes: Vec<usize> = veldt.functions.values().map(|vs| vs.len()).collect();
    if !sizes.is_empty() {
        let avg = sizes.iter().sum::<usize>() / sizes.len();
        let (lo, hi) = (*sizes.iter().min().unwrap_or(&0), *sizes.iter().max().unwrap_or(&0));
        println!("family size: avg {} variants/lineage ({}..{})",
            avg, lo, hi);
    }
    // struct/variable census — the non-fn garden state
    if !veldt.structs.is_empty() || !veldt.variables.is_empty() {
        println!("state: {} structs, {} variables",
            veldt.structs.len(), veldt.variables.len());
    }
    // avg lifespan — how long variants live
    let lifespans: Vec<f64> = veldt.functions.values()
        .flat_map(|vs| vs.iter().map(|e| e.lifespan)).collect();
    if !lifespans.is_empty() {
        let avg = lifespans.iter().sum::<f64>() / lifespans.len() as f64;
        let (lo, hi) = (lifespans.iter().copied().fold(f64::INFINITY, f64::min),
                        lifespans.iter().copied().fold(f64::NEG_INFINITY, f64::max));
        println!("lifespan: avg {:.0} runs ({}..{})", avg, lo, hi);
    }
    // avg niches per variant — niche diversity per variant
    let niche_counts: Vec<usize> = veldt.functions.values()
        .flat_map(|vs| vs.iter().map(|e| e.niche_scores.len())).collect();
    if !niche_counts.is_empty() {
        let avg = niche_counts.iter().sum::<usize>() / niche_counts.len();
        let (lo, hi) = (*niche_counts.iter().min().unwrap_or(&0),
                        *niche_counts.iter().max().unwrap_or(&0));
        println!("niches: avg {} per variant ({}..{})", avg, lo, hi);
    }
    // obituary staleness — runs since last use before death
    if !veldt.obituaries.is_empty() {
        let stale: Vec<u64> = veldt.obituaries.iter()
            .map(|o| veldt.run_count.saturating_sub(o.last_used)).collect();
        let avg = stale.iter().sum::<u64>() / stale.len() as u64;
        let max = stale.iter().max().unwrap_or(&0);
        let stale_10 = stale.iter().filter(|s| **s >= 10).count();
        println!("staleness: {} of {} deaths sat 10+ runs idle (avg {}, max {})",
            stale_10, veldt.obituaries.len(), avg, max);
    }
    // fitness at death — are the dead the weakest (selection working)
    if !veldt.obituaries.is_empty() {
        let dead_fits: Vec<f64> = veldt.functions.values()
            .flat_map(|vs| vs.iter().map(|e| e.fitness))
            .collect();
        let live_peak = dead_fits.iter().copied()
            .fold(f64::NEG_INFINITY, f64::max);
        let dead_ages: Vec<f64> = veldt.obituaries.iter()
            .map(|o| o.age as f64).collect();
        let avg_dead_age = dead_ages.iter().sum::<f64>() / dead_ages.len() as f64;
        println!("selection: live peak {:.2} vs dead avg age {:.0} runs",
            live_peak, avg_dead_age);
    }
    // avg fitness per family — which lineages are fittest on average
    let mut fam_avg: Vec<(&String, f64)> = veldt.functions.iter()
        .map(|(n, vs)| {
            let avg = vs.iter().map(|e| e.fitness).sum::<f64>()
                / vs.len().max(1) as f64;
            (n, avg)
        }).collect();
    fam_avg.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !fam_avg.is_empty() {
        let line: Vec<String> = fam_avg.iter().take(8)
            .map(|(n, a)| format!("{}:{:.2}", n, a)).collect();
        println!("avg fitness/family: {}", line.join(" "));
    }
    // avg niche score per family — per-lineage niche profile
    for (name, vs) in &veldt.functions {
        let mut fam_niches: std::collections::HashMap<String, Vec<f64>> =
            std::collections::HashMap::new();
        for e in vs {
            for (n, s) in &e.niche_scores {
                fam_niches.entry(n.clone()).or_default().push(*s);
            }
        }
        let mut top: Vec<(String, f64)> = fam_niches.into_iter()
            .map(|(n, ss)| (n, ss.iter().sum::<f64>() / ss.len().max(1) as f64))
            .collect();
        top.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        if !top.is_empty() {
            let line: Vec<String> = top.iter().take(4)
                .map(|(n, a)| format!("{}:{:.2}", n, a)).collect();
            println!("  {}: {}", name, line.join(" "));
        }
    }
    // health at death — are the dead mostly sick (working) or healthy (wasteful)
    if !veldt.obituaries.is_empty() {
        let (mut h, mut s, mut d) = (0usize, 0usize, 0usize);
        for o in &veldt.obituaries {
            match o.health {
                veldt::Health::Healthy => h += 1,
                veldt::Health::Sick(_) => s += 1,
                veldt::Health::Dying => d += 1,
            }
        }
        println!("death health: {} healthy, {} sick, {} dying",
            h, s, d);
    }
    // avg variant body size — code length of living variants
    let sizes: Vec<usize> = veldt.functions.values()
        .flat_map(|vs| vs.iter().map(|e| crate::fmt::format_program(&e.body).len()))
        .collect();
    if !sizes.is_empty() {
        let avg = sizes.iter().sum::<usize>() / sizes.len();
        let (lo, hi) = (*sizes.iter().min().unwrap_or(&0),
                        *sizes.iter().max().unwrap_or(&0));
        println!("code size: avg {} chars/variant ({}..{})", avg, lo, hi);
    }
    // avg variant args — param count distribution (signature complexity)
    let arg_counts: Vec<usize> = veldt.functions.values()
        .flat_map(|vs| vs.iter().map(|e| e.params.len()))
        .collect();
    if !arg_counts.is_empty() {
        let avg = arg_counts.iter().sum::<usize>() / arg_counts.len();
        let (lo, hi) = (*arg_counts.iter().min().unwrap_or(&0),
                        *arg_counts.iter().max().unwrap_or(&0));
        println!("arity: avg {} params/variant ({}..{})", avg, lo, hi);
    }
    // avg age per family — per-lineage turnover (churn vs stable)
    let mut fam_age: Vec<(&String, f64)> = veldt.functions.iter()
        .map(|(n, vs)| {
            let avg = vs.iter().map(|e| e.age as f64).sum::<f64>()
                / vs.len().max(1) as f64;
            (n, avg)
        }).collect();
    fam_age.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !fam_age.is_empty() {
        let line: Vec<String> = fam_age.iter().take(8)
            .map(|(n, a)| format!("{}:{:.0}", n, a)).collect();
        println!("avg age/family: {}", line.join(" "));
    }
    // avg usage per family — per-lineage call pressure
    let mut fam_use: Vec<(&String, f64)> = veldt.functions.iter()
        .map(|(n, vs)| {
            let avg = vs.iter().map(|e| e.usage_count as f64).sum::<f64>()
                / vs.len().max(1) as f64;
            (n, avg)
        }).collect();
    fam_use.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !fam_use.is_empty() {
        let line: Vec<String> = fam_use.iter().take(8)
            .map(|(n, a)| format!("{}:{:.0}", n, a)).collect();
        println!("avg usage/family: {}", line.join(" "));
    }
    // avg body size per family — per-lineage code size (compact vs bloated)
    let mut fam_size: Vec<(&String, usize)> = veldt.functions.iter()
        .map(|(n, vs)| {
            let avg = vs.iter()
                .map(|e| crate::fmt::format_program(&e.body).len())
                .sum::<usize>() / vs.len().max(1);
            (n, avg)
        }).collect();
    fam_size.sort_by(|a, b| b.1.cmp(&a.1));
    if !fam_size.is_empty() {
        let line: Vec<String> = fam_size.iter().take(8)
            .map(|(n, a)| format!("{}:{}", n, a)).collect();
        println!("avg size/family: {}", line.join(" "));
    }
    // avg args per family — per-lineage arity (signature complexity)
    let mut fam_args: Vec<(&String, f64)> = veldt.functions.iter()
        .map(|(n, vs)| {
            let avg = vs.iter().map(|e| e.params.len() as f64).sum::<f64>()
                / vs.len().max(1) as f64;
            (n, avg)
        }).collect();
    fam_args.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !fam_args.is_empty() {
        let line: Vec<String> = fam_args.iter().take(8)
            .map(|(n, a)| format!("{}:{:.0}", n, a)).collect();
        println!("avg args/family: {}", line.join(" "));
    }
    // avg lifespan per family — per-lineage longevity
    let mut fam_life: Vec<(&String, f64)> = veldt.functions.iter()
        .map(|(n, vs)| {
            let avg = vs.iter().map(|e| e.lifespan).sum::<f64>()
                / vs.len().max(1) as f64;
            (n, avg)
        }).collect();
    fam_life.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !fam_life.is_empty() {
        let line: Vec<String> = fam_life.iter().take(8)
            .map(|(n, a)| format!("{}:{:.0}", n, a)).collect();
        println!("avg lifespan/family: {}", line.join(" "));
    }
    // avg usage per niche — per-niche call pressure
    let mut niche_use: std::collections::HashMap<String, (u64, usize)> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            for (n, _) in &e.niche_scores {
                let ent = niche_use.entry(n.clone()).or_default();
                ent.0 += e.usage_count; ent.1 += 1;
            }
        }
    }
    let mut niche_use_list: Vec<(String, f64)> = niche_use.into_iter()
        .map(|(n, (u, c))| (n, u as f64 / c.max(1) as f64)).collect();
    niche_use_list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !niche_use_list.is_empty() {
        let line: Vec<String> = niche_use_list.iter().take(8)
            .map(|(n, a)| format!("{}:{:.0}", n, a)).collect();
        println!("avg usage/niche: {}", line.join(" "));
    }
    // avg age per niche — per-niche turnover (churn vs stable)
    let mut niche_age: std::collections::HashMap<String, (u64, usize)> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            for (n, _) in &e.niche_scores {
                let ent = niche_age.entry(n.clone()).or_default();
                ent.0 += e.age; ent.1 += 1;
            }
        }
    }
    let mut niche_age_list: Vec<(String, f64)> = niche_age.into_iter()
        .map(|(n, (a, c))| (n, a as f64 / c.max(1) as f64)).collect();
    niche_age_list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !niche_age_list.is_empty() {
        let line: Vec<String> = niche_age_list.iter().take(8)
            .map(|(n, a)| format!("{}:{:.0}", n, a)).collect();
        println!("avg age/niche: {}", line.join(" "));
    }
    // avg code size per niche — per-niche complexity
    let mut niche_size: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            let sz = crate::fmt::format_program(&e.body).len();
            for (n, _) in &e.niche_scores {
                let ent = niche_size.entry(n.clone()).or_default();
                ent.0 += sz; ent.1 += 1;
            }
        }
    }
    let mut niche_size_list: Vec<(String, f64)> = niche_size.into_iter()
        .map(|(n, (s, c))| (n, s as f64 / c.max(1) as f64)).collect();
    niche_size_list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !niche_size_list.is_empty() {
        let line: Vec<String> = niche_size_list.iter().take(8)
            .map(|(n, a)| format!("{}:{:.0}", n, a)).collect();
        println!("avg size/niche: {}", line.join(" "));
    }
    // avg lifespan per niche — per-niche longevity
    let mut niche_life: std::collections::HashMap<String, (f64, usize)> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            for (n, _) in &e.niche_scores {
                let ent = niche_life.entry(n.clone()).or_default();
                ent.0 += e.lifespan; ent.1 += 1;
            }
        }
    }
    let mut niche_life_list: Vec<(String, f64)> = niche_life.into_iter()
        .map(|(n, (l, c))| (n, l / c.max(1) as f64)).collect();
    niche_life_list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !niche_life_list.is_empty() {
        let line: Vec<String> = niche_life_list.iter().take(8)
            .map(|(n, a)| format!("{}:{:.0}", n, a)).collect();
        println!("avg lifespan/niche: {}", line.join(" "));
    }
    // avg args per niche — per-niche arity (signature complexity)
    let mut niche_args: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            for (n, _) in &e.niche_scores {
                let ent = niche_args.entry(n.clone()).or_default();
                ent.0 += e.params.len(); ent.1 += 1;
            }
        }
    }
    let mut niche_args_list: Vec<(String, f64)> = niche_args.into_iter()
        .map(|(n, (a, c))| (n, a as f64 / c.max(1) as f64)).collect();
    niche_args_list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !niche_args_list.is_empty() {
        let line: Vec<String> = niche_args_list.iter().take(8)
            .map(|(n, a)| format!("{}:{:.0}", n, a)).collect();
        println!("avg args/niche: {}", line.join(" "));
    }
    // avg niche score per niche — which niches are hardest to score on
    let mut niche_score: std::collections::HashMap<String, (f64, usize)> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            for (n, s) in &e.niche_scores {
                let ent = niche_score.entry(n.clone()).or_default();
                ent.0 += s; ent.1 += 1;
            }
        }
    }
    let mut niche_score_list: Vec<(String, f64)> = niche_score.into_iter()
        .map(|(n, (s, c))| (n, s / c.max(1) as f64)).collect();
    niche_score_list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !niche_score_list.is_empty() {
        let line: Vec<String> = niche_score_list.iter().take(8)
            .map(|(n, a)| format!("{}:{:.2}", n, a)).collect();
        println!("avg score/niche: {}", line.join(" "));
    }
    // avg fitness per niche — which niches are strongest
    let mut niche_fit: std::collections::HashMap<String, (f64, usize)> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            for (n, _) in &e.niche_scores {
                let ent = niche_fit.entry(n.clone()).or_default();
                ent.0 += e.fitness; ent.1 += 1;
            }
        }
    }
    let mut niche_fit_list: Vec<(String, f64)> = niche_fit.into_iter()
        .map(|(n, (f, c))| (n, f / c.max(1) as f64)).collect();
    niche_fit_list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !niche_fit_list.is_empty() {
        let line: Vec<String> = niche_fit_list.iter().take(8)
            .map(|(n, a)| format!("{}:{:.2}", n, a)).collect();
        println!("avg fitness/niche: {}", line.join(" "));
    }
    // avg niches per family — per-lineage niche diversity
    let mut fam_niche: Vec<(&String, f64)> = veldt.functions.iter()
        .map(|(n, vs)| {
            let all: std::collections::HashSet<&String> = vs.iter()
                .flat_map(|e| e.niche_scores.keys()).collect();
            (n, all.len() as f64 / vs.len().max(1) as f64)
        }).collect();
    fam_niche.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !fam_niche.is_empty() {
        let line: Vec<String> = fam_niche.iter().take(8)
            .map(|(n, a)| format!("{}:{:.0}", n, a)).collect();
        println!("avg niches/family: {}", line.join(" "));
    }
    // avg probes per family — per-lineage selection pressure depth
    let mut fam_probe: Vec<(&String, f64)> = veldt.functions.keys()
        .map(|n| (n, veldt.probes.get(n).map(|p| p.len() as f64).unwrap_or(0.0)))
        .collect();
    fam_probe.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !fam_probe.is_empty() {
        let line: Vec<String> = fam_probe.iter().take(8)
            .map(|(n, a)| format!("{}:{:.0}", n, a)).collect();
        println!("probes/family: {}", line.join(" "));
    }
    // avg probes per niche — per-niche selection pressure depth
    let mut niche_probe: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for probes in veldt.probes.values() {
        for p in probes {
            *niche_probe.entry(p.niche.clone()).or_default() += 1;
        }
    }
    let mut niche_probe_list: Vec<(String, usize)> = niche_probe.into_iter().collect();
    niche_probe_list.sort_by(|a, b| b.1.cmp(&a.1));
    if !niche_probe_list.is_empty() {
        let line: Vec<String> = niche_probe_list.iter().take(8)
            .map(|(n, c)| format!("{}:{}", n, c)).collect();
        println!("probes/niche: {}", line.join(" "));
    }
    // avg healing per family — per-lineage healing effort
    let mut fam_heal: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();
    for o in &veldt.obituaries {
        let ent = fam_heal.entry(o.name.clone()).or_default();
        ent.0 += o.healing_attempts.len(); ent.1 += 1;
    }
    let mut fam_heal_list: Vec<(String, f64)> = fam_heal.into_iter()
        .map(|(n, (h, c))| (n, h as f64 / c.max(1) as f64)).collect();
    fam_heal_list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !fam_heal_list.is_empty() {
        let line: Vec<String> = fam_heal_list.iter().take(8)
            .map(|(n, a)| format!("{}:{:.1}", n, a)).collect();
        println!("healing/family: {}", line.join(" "));
    }
    // avg death age per family — per-lineage turnover
    let mut fam_dage: std::collections::HashMap<String, (u64, usize)> =
        std::collections::HashMap::new();
    for o in &veldt.obituaries {
        let ent = fam_dage.entry(o.name.clone()).or_default();
        ent.0 += o.age; ent.1 += 1;
    }
    let mut fam_dage_list: Vec<(String, f64)> = fam_dage.into_iter()
        .map(|(n, (a, c))| (n, a as f64 / c.max(1) as f64)).collect();
    fam_dage_list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !fam_dage_list.is_empty() {
        let line: Vec<String> = fam_dage_list.iter().take(8)
            .map(|(n, a)| format!("{}:{:.0}", n, a)).collect();
        println!("death age/family: {}", line.join(" "));
    }
    // avg healing per niche — per-niche healing effort
    let mut niche_heal: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            for (n, _) in &e.niche_scores {
                let ent = niche_heal.entry(n.clone()).or_default();
                ent.0 += e.healing_attempts.len(); ent.1 += 1;
            }
        }
    }
    let mut niche_heal_list: Vec<(String, f64)> = niche_heal.into_iter()
        .map(|(n, (h, c))| (n, h as f64 / c.max(1) as f64)).collect();
    niche_heal_list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !niche_heal_list.is_empty() {
        let line: Vec<String> = niche_heal_list.iter().take(8)
            .map(|(n, a)| format!("{}:{:.1}", n, a)).collect();
        println!("healing/niche: {}", line.join(" "));
    }
    // avg staleness per family — per-lineage idle-before-death
    let mut fam_stale: std::collections::HashMap<String, (u64, usize)> =
        std::collections::HashMap::new();
    for o in &veldt.obituaries {
        let ent = fam_stale.entry(o.name.clone()).or_default();
        ent.0 += veldt.run_count.saturating_sub(o.last_used); ent.1 += 1;
    }
    let mut fam_stale_list: Vec<(String, f64)> = fam_stale.into_iter()
        .map(|(n, (s, c))| (n, s as f64 / c.max(1) as f64)).collect();
    fam_stale_list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !fam_stale_list.is_empty() {
        let line: Vec<String> = fam_stale_list.iter().take(8)
            .map(|(n, a)| format!("{}:{:.0}", n, a)).collect();
        println!("staleness/family: {}", line.join(" "));
    }
    // avg staleness per niche — per-niche idle-before-death
    let mut niche_stale: std::collections::HashMap<String, (u64, usize)> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            for (n, _) in &e.niche_scores {
                let ent = niche_stale.entry(n.clone()).or_default();
                ent.0 += veldt.run_count.saturating_sub(e.last_used); ent.1 += 1;
            }
        }
    }
    let mut niche_stale_list: Vec<(String, f64)> = niche_stale.into_iter()
        .map(|(n, (s, c))| (n, s as f64 / c.max(1) as f64)).collect();
    niche_stale_list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if !niche_stale_list.is_empty() {
        let line: Vec<String> = niche_stale_list.iter().take(8)
            .map(|(n, a)| format!("{}:{:.0}", n, a)).collect();
        println!("staleness/niche: {}", line.join(" "));
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
        for (name, _key, fit, n) in best.iter().take(10) {
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
    // no top-level statements — a file with only fn defs is a module
    let has_toplevel = stmts.iter().any(|s| !matches!(s,
        Stmt::FnDef(..) | Stmt::Grow(GrowItem::Fn(..)) | Stmt::Grow(GrowItem::Struct(..))
        | Stmt::StructDef(..) | Stmt::Import(..)));
    if !stmts.is_empty() && !has_toplevel {
        out.push("no top-level statements — file is a module, not a program".into());
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

fn run_program_vm(path: &str, evolve_after: bool) {
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
    if std::env::var("VELDT_JIT_STATS").is_ok() {
        v.jit_stats();
    }
    // --evolve — same as the tree-walk path: mutate, trial, save.
    if evolve_after {
        let mut evolver = evolve::Evolver::new();
        for event in evolver.evolve(&mut garden, 1) {
            eprintln!("{}", event.format());
        }
    }
    if let Err(e) = garden.save() {
        eprintln!("Error saving veldt: {}", e);
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
    let verify = std::env::args().any(|a| a == "--verify");
    if let Some(pos) = target.find('#') {
        let name = &target[..pos];
        let id: u32 = target[pos+1..].parse().unwrap_or(0);
        let mut veldt = veldt::Veldt::new();
        if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
        // --verify: bounded equivalence check against the fittest —
        // only promote if this variant agrees on every domain point.
        if verify {
            let mut interp = interpreter::Interpreter::new();
            veldt.load_into_interpreter(&mut interp);
            let variants = match interp.functions.get(name) {
                Some(v) => v.clone(),
                None => { println!("No lineage '{}'", name); return; }
            };
            let fittest = variants.iter()
                .max_by(|a, b| a.fitness.partial_cmp(&b.fitness)
                    .unwrap_or(std::cmp::Ordering::Equal)).unwrap();
            if fittest.id == id {
                println!("{}#{} is already the fittest — trivially verified",
                         name, id);
            } else {
                let arity = fittest.def.params.len();
                let mut ok = true;
                // int domain for int fns, float domain for float fns —
                // try float first, fall back to int on type error
                let mut is_float_fn = false;
                {
                    let probe = interp.call_function(
                        name, Some(fittest.id),
                        vec![crate::interpreter::Value::Float(1.5); arity.max(1)]);
                    is_float_fn = probe.is_ok();
                }
                for x in -10..=10i64 {
                    let args: Vec<crate::interpreter::Value> =
                        if is_float_fn {
                            vec![crate::interpreter::Value::Float(x as f64); arity.max(1)]
                        } else {
                            vec![crate::interpreter::Value::Int(x); arity.max(1)]
                        };
                    let a = interp.call_function(
                        name, Some(id), args.clone());
                    let b = interp.call_function(
                        name, Some(fittest.id), args.clone());
                    let same = match (&a, &b) {
                        (Ok(p), Ok(q)) => p == q,
                        (Err(_), Err(_)) => true,
                        _ => false,
                    };
                    if !same {
                        println!("{}#{} diverges from fittest at {:?} —                             NOT promoting", name, id, args);
                        ok = false;
                        break;
                    }
                }
                if !ok {
                    println!("Verification failed — {}#{} stays at its                         current trust", name, id);
                    return;
                }
                println!("{}#{} bounded-verified — safe to promote",
                         name, id);
            }
        }
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

fn verify_family(name: &str, args: &[String]) {
    let bound: i64 = args.iter().position(|a| a == "--domain")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok()).unwrap_or(10);
    let mut veldt = veldt::Veldt::new();
    if let Err(e) = veldt.load() { eprintln!("Error: {}", e); process::exit(1); }
    let mut interp = interpreter::Interpreter::new();
    veldt.load_into_interpreter(&mut interp);
    let variants = match interp.functions.get(name) {
        Some(v) => v.clone(),
        None => { println!("No lineage '{}'", name); return; }
    };
    if variants.len() < 2 {
        println!("'{}' has {} variant — nothing to check", name, variants.len());
        return;
    }
    let fittest = variants.iter()
        .max_by(|a, b| a.fitness.partial_cmp(&b.fitness)
            .unwrap_or(std::cmp::Ordering::Equal)).unwrap();
    let arity = fittest.def.params.len();
    // bounded domain: each int arg ranges over -bound..bound
    let mut domain: Vec<i64> = (-bound..=bound).collect();
    if domain.len() > 21 { domain.truncate(21); }
    println!("Verifying '{}' ({} variants, {}-arg, domain {} points)",
             name, variants.len(), arity, domain.len());
    let mut checked = 0;
    for v in &variants {
        if v.id == fittest.id { continue; }
        let mut diverge: Option<String> = None;
        // proper cross-product: every arg ranges independently for
        // arity<=2 (441 checks); same-value fill for higher arities
        let combos: Vec<Vec<i64>> = if arity <= 2 {
            let mut c = Vec::new();
            for a in &domain {
                if arity == 1 { c.push(vec![*a]); }
                else {
                    for b in &domain { c.push(vec![*a, *b]); }
                }
            }
            c
        } else {
            domain.iter().map(|x| vec![*x; arity]).collect()
        };
        let n_combos = combos.len();
        'outer: for combo in combos {
            // eval quietly — side-effect prints pollute the report
            let a = interp.call_function(name, Some(v.id), combo.iter().map(|n| crate::interpreter::Value::Int(*n)).collect());
            let b = interp.call_function(name, Some(fittest.id), combo.iter().map(|n| crate::interpreter::Value::Int(*n)).collect());
            checked += 1;
            let same = match (&a, &b) {
                (Ok(x), Ok(y)) => x == y,
                (Err(_), Err(_)) => true,
                _ => false,
            };
            if !same {
                diverge = Some(format!("at {:?}: v#{} = {:?} vs fit#{} = {:?}",
                    combo, v.id, a.map_err(|e| e.chars().take(20).collect::<String>()),
                    fittest.id, b.map_err(|e| e.chars().take(20).collect::<String>())));
                break 'outer;
            }
        }
        match diverge {
            Some(d) => println!("  {}#{}: DIVERGES {}", name, v.id, d),
            None => println!("  {}#{}: bounded-verified ({} checks)",
                             name, v.id, n_combos),
        }
    }
    println!("({} calls evaluated)", checked);
}

// Run the bounded check on `name`, return the first divergent
// variant id (or None if all agree). Used by verify --quarantine.
pub fn verify_family_find_divergent(name: &str) -> Option<u32> {
    let mut veldt = veldt::Veldt::new();
    if veldt.load().is_err() { return None; }
    let mut interp = interpreter::Interpreter::new();
    veldt.load_into_interpreter(&mut interp);
    let variants = interp.functions.get(name)?.clone();
    if variants.len() < 2 { return None; }
    let fittest = variants.iter()
        .max_by(|a, b| a.fitness.partial_cmp(&b.fitness)
            .unwrap_or(std::cmp::Ordering::Equal))?;
    let arity = fittest.def.params.len();
    let mut found = None;
    for v in &variants {
        if v.id == fittest.id { continue; }
        for x in -10..=10i64 {
            let args: Vec<crate::interpreter::Value> =
                vec![crate::interpreter::Value::Int(x); arity.max(1)];
            let a = interp.call_function(name, Some(v.id), args.clone());
            let b = interp.call_function(name, Some(fittest.id), args);
            let same = match (&a, &b) {
                (Ok(p), Ok(q)) => p == q,
                (Err(_), Err(_)) => true,
                _ => false,
            };
            if !same {
                println!("  {}#{}: DIVERGES", name, v.id);
                found = Some(v.id);
                break;
            }
        }
        if found.is_none() {
            println!("  {}#{}: bounded-verified", name, v.id);
        }
        if found.is_some() { break; }
    }
    found
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
