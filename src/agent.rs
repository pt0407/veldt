// The Gardener — a rule-based coding agent for Veldt Studio.
// No LLM, no API key: it writes Veldt from a template catalog, runs it
// through the real pipeline, observes the result, and narrates each step.
// Grown functions enter the garden as ordinary variants and compete like
// any other organism.

use crate::ast::GrowItem;
use crate::evolve::Evolver;
use crate::health::HealthChecker;
use crate::interpreter::Interpreter;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::veldt::{FunctionEntry, Health, Veldt};
use serde::Serialize;

#[derive(Serialize, Debug)]
pub struct AgentStep {
    pub kind: &'static str, // "plan" | "act" | "obs" | "ok" | "err"
    pub text: String,
}

#[derive(Serialize)]
pub struct AgentResponse {
    pub steps: Vec<AgentStep>,
    pub reply: String,
    pub code: Option<String>,
    pub ran: bool,
    pub output: Option<String>,
    pub success: Option<bool>,
    pub events: Vec<String>,
}

pub struct ExecResult {
    pub output: String,
    pub events: Vec<String>,
    pub success: bool,
}

fn step(kind: &'static str, text: impl Into<String>) -> AgentStep {
    AgentStep { kind, text: text.into() }
}

// === The shared run pipeline ===
// Same lifecycle as `veldt run`: lex, parse, interpret, record usage,
// grow + birth-check, heal the sick, age + reap. Used by both the
// /api/run endpoint and the agent.
pub fn execute_source(source: &str, veldt: &mut Veldt) -> ExecResult {
    execute_source_opts(source, veldt, crate::interpreter::Caps::allow_all())
}

/// execute_source with capability control — sandboxed callers get a
/// Stage-0 interpreter (fs/net/proc all denied).
pub fn execute_source_opts(source: &str, veldt: &mut Veldt, caps: crate::interpreter::Caps) -> ExecResult {
    execute_source_opts_full(source, veldt, caps, veldt.prefer_vm)
}

/// Shared run pipeline — `use_vm` routes execution through the
/// bytecode VM + JIT (`/api/run` `{"vm": true}`); same lex/parse/
/// capture/events semantics as the interpreter path.
pub fn execute_source_opts_full(source: &str, veldt: &mut Veldt, caps: crate::interpreter::Caps, use_vm: bool) -> ExecResult {
    let mut output = String::new();
    let mut events = Vec::new();

    let mut lexer = Lexer::new(source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(e) => {
            return ExecResult {
                output,
                events: vec![format!("Lex error: {}", e)],
                success: false,
            };
        }
    };

    let lines = lexer.token_lines.clone();
    let mut parser = Parser::new_with_lines(tokens, lines);
    let stmts = match parser.parse_program() {
        Ok(s) => s,
        Err(e) => {
            return ExecResult {
                output,
                events: vec![format!("Parse error: {}", e)],
                success: false,
            };
        }
    };
    let parser_lines = parser.statement_lines();

    let mut interp = Interpreter::new();
    interp.trace_calls = true;
    interp.caps = caps;
    interp.call_stack.clear();
    veldt.load_into_interpreter(&mut interp);
    interp.print_buffer = Some(String::new());

    // fast-engine path — same garden, same capture semantics
    let mut vm_res: Option<Result<(), String>> = None;
    if use_vm {
        let mut v = crate::vm::Vm::new();
        v.helper.print_buffer = Some(String::new());
        let _ = v.load_garden(veldt);
        vm_res = Some(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || v.run(&stmts, u64::MAX / 4)))
            .unwrap_or_else(|_| Err("vm panicked".into())));
        if let Some(buf) = v.helper.print_buffer.take() { output.push_str(&buf); }
    }

    let run_res = match vm_res {
        Some(r) => r,
        None => std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || interp.run_with_lines(&stmts, &parser_lines)))
            .unwrap_or_else(|_| Err("interpreter panicked".into())),
    };
    let success = match run_res {
        Ok(()) => true,
        Err(e) => {
            if e.starts_with("__veldt_exit__") {
                return ExecResult { output, events, success: true };
            }
            if let Some(buf) = interp.print_buffer.take() {
                output.push_str(&buf);
            }
            let line = interp.current_line;
            if line > 0 {
                output.push_str(&format!("Runtime error (line {}): {}\n", line, e));
            } else {
                output.push_str(&format!("Runtime error: {}\n", e));
            }
            for (n, l) in interp.call_stack.iter().rev() {
                output.push_str(&format!("  called from line {} in {}\n", l, n));
            }
            interp.call_stack.clear();
            false
        }
    };
    if let Some(buf) = interp.print_buffer.take() {
        output.push_str(&buf);
    }

    veldt.record_usage(&interp.usage_log);

    let grows = interp.collect_grows(&stmts);
    for item in &grows {
        match item {
            GrowItem::Fn(name, params, body, tests) => {
                let src = format!("fn {}(...) {{ ... }}", name);
                let id = veldt.grow_function(name, params.clone(), body.clone(), tests.clone(), &src);
                let mut entry = veldt.functions[name].iter()
                    .find(|e| e.id == id).cloned().unwrap();
                HealthChecker::check_at_birth(&mut entry, veldt);
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

    // Heal the sick — same hybrid-merge pass as the CLI
    let sick_entries: Vec<(String, u32)> = veldt.functions.iter()
        .flat_map(|(name, variants)| {
            variants.iter()
                .filter(|e| matches!(e.health, Health::Sick(_)))
                .map(move |e| (name.clone(), e.id))
        })
        .collect();
    for (name, id) in sick_entries {
        let mut entry = veldt.functions[&name].iter()
            .find(|e| e.id == id).cloned().unwrap();
        if HealthChecker::heal(&mut entry, veldt) {
            events.push(format!("[HEALED] {}#{} — hybrid merge successful", name, id));
            if let Some(variants) = veldt.functions.get_mut(&name) {
                for e in variants.iter_mut() {
                    if e.id == id { *e = entry.clone(); break; }
                }
            }
        }
    }

    let obituaries = veldt.age_and_reap();
    for obit in &obituaries {
        events.push(format!("[OBITUARY] {}#{} — {} (age {})", obit.name, obit.id, obit.cause_of_death, obit.age));
    }

    // Real call I/O becomes probe evidence (IRIS-style learning)
    veldt.merge_traces(&interp.traces);

    ExecResult { output, events, success }
}

// === Intent routing ===

pub fn respond(task: &str, editor_source: &str, veldt: &mut Veldt) -> AgentResponse {
    let t = task.to_lowercase();
    let words: Vec<&str> = t.split(|c: char| !c.is_alphanumeric() && c != '#' && c != '_')
        .filter(|w| !w.is_empty())
        .collect();

    let has = |pats: &[&str]| pats.iter().any(|p| t.contains(p));
    let hasw = |pats: &[&str]| pats.iter().any(|p| words.contains(p));
    let gen = || has(&["write", "make", "create", "grow", "function", "implement", "build", "code"]);

    if hasw(&["prune", "kill"]) {
        return prune_intent(&words, veldt);
    }
    if hasw(&["clear"]) {
        return plain("That one's on you — the Clear Garden button is up top. I don't do extinctions.");
    }
    if hasw(&["adopt"]) {
        return adopt_intent(&words, veldt);
    }
    if hasw(&["evolve", "mutate", "generation"]) {
        return evolve_intent(&words, veldt);
    }
    if hasw(&["fix", "repair"]) && !has(&["garden"]) {
        return fix_intent(&words, veldt);
    }
    if hasw(&["heal", "tend", "mend"]) || has(&["sick", "fix the garden"]) {
        return tend_intent(veldt);
    }
    if hasw(&["improve", "optimize", "optimise", "faster", "upgrade"]) {
        return improve_intent(&t, &words, veldt);
    }
    if hasw(&["fuzz"]) {
        return fuzz_intent(&words, veldt);
    }
    if hasw(&["absorb"]) {
        crate::absorb_traces(None);
        return plain("Absorbed live call traces into the probe pool — production evidence now feeds selection.");
    }
    if hasw(&["tournament"]) {
        let url = words.iter().find(|w| w.starts_with("http")).copied();
        match url {
            Some(u) => { crate::tournament(u, None); return plain(&format!("Tournament run against {} — champions crowned and pushed back.", u)); }
            None => return plain("Tournament against which pool? Give me a URL — \"tournament http://host:7878\"."),
        }
    }
    if has(&["sandbox", "isolated"]) && hasw(&["run", "try"]) {
        return sandbox_run_intent(editor_source, veldt);
    }
    if hasw(&["suggest", "surprise"]) {
        return suggest_intent(&words, veldt);
    }
    if hasw(&["benchmark", "bench", "benchmarks"]) {
        let (report, _) = bench_report(veldt);
        return plain(&report);
    }
    if has(&["stats", "stat", "how is", "how's", "doing", "health of", "tell me about", "about"]) {
        if find_lineage(&words, veldt).is_some() {
            return family_intent(&words, veldt);
        }
    }
    if has(&["attention", "triage", "weakest", "worst", "what needs", "needs work", "struggling"]) {
        return triage_intent(veldt);
    }
    if has(&["compare", "diff", "difference between", "vs", "versus"]) {
        if words.iter().any(|w| w.contains('#')) {
            return compare_intent(&words, veldt);
        }
    }
    if has(&["coverage", "covered", "tested", "probes", "test suite", "test coverage"]) {
        return coverage_intent(&words, veldt);
    }
    if has(&["lineage", "parents", "children", "ancestry", "descended", "born from", "family tree"]) {
        if words.iter().any(|w| w.contains('#') || veldt.functions.contains_key(w.split('#').next().unwrap_or(w))) {
            return lineage_intent(&words, veldt);
        }
    }
    if has(&["most used", "hot", "hottest", "busiest", "workload", "usage"]) {
        return usage_intent(&words, veldt);
    }
    if has(&["fastest", "most efficient", "efficient", "speed", "performant"]) {
        return efficiency_intent(&words, veldt);
    }
    if has(&["canonical", "trust", "trusted", "trust levels", "established"]) {
        return trust_intent(&words, veldt);
    }
    if has(&["oldest", "newest", "youngest", "eldest", "age of", "how old"]) {
        return age_intent(&words, veldt);
    }
    if has(&["niche", "niches", "gap", "gaps", "uncovered", "not tested", "missing coverage"]) {
        return niche_intent(&words, veldt);
    }
    if has(&["history", "what changed", "recent changes", "mutations", "what happened", "latest"]) {
        return history_intent(&words, veldt);
    }
    if has(&["turnover", "churn", "birth rate", "death rate", "mortality"]) {
        return turnover_intent(&words, veldt);
    }
    if has(&["pressure", "rivals", "competition", "who competes", "selection"]) {
        return pressure_intent(&words, veldt);
    }
    if has(&["healing", "heal", "recovering", "recovery", "being fixed"]) {
        return healing_intent(&words, veldt);
    }
    if has(&["where did", "origin", "origins", "sources", "bred from", "mutation source", "made of"]) {
        return sources_intent(&words, veldt);
    }
    if has(&["growing", "aging", "past lifespan", "lifecycle", "over the hill", "still young"]) {
        return lifecycle_intent(&words, veldt);
    }
    if has(&["biggest", "smallest", "largest", "size", "how big", "compact", "bloated"]) {
        return size_intent(&words, veldt);
    }
    if has(&["siblings", "cousins", "related", "same parents", "brothers", "sisters"]) {
        if words.iter().any(|w| w.contains('#')) {
            return siblings_intent(&words, veldt);
        }
    }
    if has(&["stale", "forgotten", "dormant", "unused", "not called", "idle"]) {
        return stale_intent(veldt);
    }
    // (dead-code check moved before obituary — "dead code" is not a death query)
    if has(&["garden", "status", "alive", "what's growing", "what is growing", "what have you", "ecosystem"]) {
        return status_intent(veldt);
    }
    if hasw(&["show", "explain", "source", "inspect"]) || has(&["what does", "look like"]) {
        return show_intent(&words, veldt);
    }
    if hasw(&["why"]) && !has(&["obituar", "why did", "why'd"]) {
        return why_intent(&words, veldt);
    }
    if hasw(&["lint", "warnings", "bugs"])
        || has(&["code health", "code quality", "static analysis", "lint report"]) {
        return lint_intent(&words, veldt);
    }
    if has(&["dead code", "deadcode", "unreachable", "stripped"]) {
        return dead_intent(veldt);
    }
    if has(&["best", "winner", "winning", "strongest", "fittest", "leaderboard", "top variant"]) {
        return best_intent(&words, veldt);
    }
    if has(&["proven", "production", "production-ready", "deployment", "stable", "safe to use"]) {
        return proven_intent(veldt);
    }
    if has(&["experimental", "untested", "unstable", "risky"]) {
        return risky_intent(veldt);
    }
    if has(&["verify", "verified", "agree", "consistent", "divergence", "same result"]) {
        return verify_intent(&words, veldt);
    }
    if has(&["trend", "trajectory", "improving", "getting better", "progress"]) {
        return trend_intent(veldt);
    }
    if has(&["obituar", "why did", "why'd"]) || hasw(&["deaths", "died", "dead"]) {
        return obituary_intent(&words, veldt);
    }
    if hasw(&["test", "trial", "retrial"]) && !gen() {
        return test_intent(&words, veldt);
    }
    if has(&["run", "execute", "try this", "try the"]) && !gen() {
        return run_intent(editor_source, veldt);
    }
    if gen() {
        return generate_intent(&t, &words, veldt);
    }
    if has(&["help", "what can you", "hi", "hello", "hey", "who are you"]) {
        return help_intent();
    }
    // last chance: maybe the task is a bare request like "fibonacci"
    if let Some(resp) = try_templates(&t, &words, veldt) {
        return resp;
    }
    help_intent()
}

pub fn fuzz_intent(words: &[&str], veldt: &mut Veldt) -> AgentResponse {
    // "fuzz <name>" — boundary corpus vs every variant, divergence report
    let name = words.iter().find(|w| !["fuzz","the","family","it","them","all"].contains(w)
        && veldt.functions.contains_key(**w)).copied();
    let Some(name) = name else {
        return plain("Fuzz which family? \"fuzz score\" — I'll sweep the boundary corpus and flag variants that disagree.");
    };
    let variants: Vec<u32> = veldt.functions[name].iter().map(|e| e.id).collect();
    let mut interp = crate::interpreter::Interpreter::with_step_limit(50000);
    interp.caps = crate::interpreter::Caps::sandboxed();
    veldt.load_into_interpreter(&mut interp);
    let mut divergent = 0u32;
    let mut report = String::new();
    for case in 0..8 {
        let params = &veldt.functions[name][0].params;
        let args: Vec<crate::interpreter::Value> = params.iter()
            .map(|p| crate::evolve::boundary_value(&p.typ, case))
            .collect::<Option<_>>().unwrap_or_default();
        if args.is_empty() && !params.is_empty() { break; }
        let mut outs: std::collections::HashMap<String, Vec<u32>> = std::collections::HashMap::new();
        for id in &variants {
            let r = interp.call_function(name, Some(*id), args.clone())
                .map(|v| serde_json::to_string(&v).unwrap_or_default())
                .unwrap_or_else(|e| format!("ERR:{}", e));
            outs.entry(r).or_default().push(*id);
        }
        if outs.len() > 1 {
            divergent += 1;
            report.push_str(&format!("case {}: {:?}\n", case,
                outs.iter().map(|(o, ids)| format!("{} <- #{}", o, ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(","))).collect::<Vec<_>>()));
        }
    }
    plain(&format!("fuzz {}: {} boundary cases, {} divergent.{}",
        name, 8, divergent,
        if report.is_empty() { " Family agrees everywhere — healthy.".into() } else { format!("\n{}", report) }))
}

fn sandbox_run_intent(editor_source: &str, veldt: &mut Veldt) -> AgentResponse {
    if editor_source.trim().is_empty() {
        return plain("Editor's empty — nothing to sandbox.");
    }
    // execute the editor source with all capabilities denied
    let mut interp = crate::interpreter::Interpreter::new();
    interp.caps = crate::interpreter::Caps::sandboxed();
    veldt.load_into_interpreter(&mut interp);
    let mut lx = crate::lexer::Lexer::new(editor_source);
    let result = lx.tokenize()
        .map_err(|e| e.to_string())
        .and_then(|toks| {
            let mut p = crate::parser::Parser::new_with_lines(toks, lx.token_lines.clone());
            p.parse_program().map_err(|e| e.to_string())
        })
        .and_then(|stmts| {
            interp.print_buffer = Some(String::new());
            stmts.iter().try_for_each(|s| interp.exec_stmt_pub(s).map(|_| ()))
                .map_err(|e| e.to_string())
        });
    let out = interp.print_buffer.unwrap_or_default();
    plain(&match result {
        Ok(()) => format!("Sandboxed run finished clean — no fs/net/proc touched.\n{}", out),
        Err(e) => format!("Sandboxed run stopped: {}\n{}", e, out),
    })
}

fn plain(text: &str) -> AgentResponse {
    AgentResponse {
        steps: Vec::new(),
        reply: text.into(),
        code: None,
        ran: false,
        output: None,
        success: None,
        events: Vec::new(),
    }
}

fn help_intent() -> AgentResponse {
    plain(
        "I'm the Gardener — I write Veldt code and tend the ecosystem. No LLM, just templates and the garden's own machinery.\n\
         Try:\n\
         • \"write a fibonacci function\" — generate, run, plant\n\
         • \"count primes\" — I notice the dependency and plant is_prime first\n\
         • \"parallel\" — pmap demo across threads\n\
         • \"improve fib\" — grow a faster strain into the lineage\n\
         • \"show fib\" — pretty-print the fittest variant\n\
         • \"test fib\" — re-trial every variant\n\
         • \"adopt fib#2\" — favor a variant in dispatch\n\
         • \"obituaries\" — recent deaths\n\
         • \"stats fib\" · \"how is fib doing\" — per-family dashboard\n\
         • \"what needs attention\" — triage sick/dying/weakest\n\
         • \"compare fib#1 fib#2\" — differing lines\n\
         • \"coverage for fib\" · \"what's tested\" — probe coverage\n\
         • \"fib#145 parents\" · \"fib lineage\" — genealogy\n\
         • \"most used fib\" · \"hot functions\" — workload leaders\n\
         • \"fastest fib\" · \"most efficient\" — speed ranking\n\
         • \"fib trust\" · \"what's canonical\" — trust levels\n\
         • \"newest fib\" · \"oldest\" — age-sorted variants\n\
         • \"fib niche gaps\" · \"uncovered\" — niche coverage\n\
         • \"what changed\" · \"mutations\" — recent births + deaths\n\
         • \"fib turnover\" · \"churn\" — birth/death rate + causes\n\
         • \"fib pressure\" · \"selection\" — rivalry + σ spread\n\
         • \"who's healing\" — the recovery log\n\
         • \"where did fib come from\" · \"sources\" — origin breakdown\n\
         • \"who's growing\" · \"aging\" — lifecycle split\n\
         • \"smallest fib\" · \"biggest\" — code-size ranking\n\
         • \"fib#216 siblings\" — shared-parent variants\n\
         • \"what's stale\" · \"forgotten\" — dormant variants\n\
         • \"dead code\" — dead-code-removal variants\n\
         • \"lint fib\" · \"code health\" — strict-lint warnings per variant\n\
         • \"who's winning\" · \"best fib\" — fitness leaderboard\n\
         • \"what's proven\" · \"production-ready\" — deployment subset\n\
         • \"what's risky\" · \"experimental\" — untested + unstable\n\
         • \"verify fib\" — do the variants agree on inputs?\n\
         • \"fitness trend\" · \"improving\" — the trajectory + verdict\n\
         • \"surprise me\" — plant a random new species\n\
         • \"run this\" · \"evolve 5\" · \"tend\" · \"prune x#N\" · \"what's alive?\"\n\
         Known species: fib, factorial, prime, count primes, nth prime, gcd, lcm, power, sort, quicksort, reverse, palindrome, sum, mean, max, min, fizzbuzz, collatz, evens, doubles, vowels, digits, leapyear, clamp, words, dedupe, anagram, sqrt, parallel."
    )
}

fn run_intent(editor_source: &str, veldt: &mut Veldt) -> AgentResponse {
    if editor_source.trim().is_empty() {
        return plain("The editor is empty — nothing to run.");
    }
    let mut steps = vec![step("plan", "Running the program in your editor.")];
    let result = execute_source(editor_source, veldt);
    for ev in &result.events {
        steps.push(step("obs", ev.clone()));
    }
    if result.success {
        steps.push(step("ok", "Program finished cleanly."));
    } else {
        steps.push(step("err", "The run failed — see output below."));
    }
    // Diagnose common failures into actionable hints
    let hint = if result.success { None } else {
        result.output.lines()
            .find(|l| l.contains("error") || l.contains("Error"))
            .and_then(|e| {
                if let Some(f) = e.split("Undefined function: ").nth(1) {
                    let f = f.trim();
                    Some(format!("{} isn't in the garden — say \"write {}\" or \"a function called {}\" and I'll plant it.", f, f, f))
                } else if e.contains("Parse") || e.contains("expected") || e.contains("Expected") {
                    Some("syntax issue — check brackets and 'test' clauses.".to_string())
                } else if e.contains("Cannot") {
                    Some("type mismatch — watch what you're adding/comparing.".to_string())
                } else { None }
            })
    };
    AgentResponse {
        steps,
        reply: match (result.success, hint) {
            (true, _) => "Ran it. Any grows were health-checked and planted.".into(),
            (false, Some(h)) => format!("It failed — {}", h),
            (false, None) => "It died on the vine — the error is in the output. Fix the code and I'll run it again.".into(),
        },
        code: None,
        ran: true,
        output: Some(result.output),
        success: Some(result.success),
        events: result.events,
    }
}

fn evolve_intent(words: &[&str], veldt: &mut Veldt) -> AgentResponse {
    let cycles: usize = words.iter()
        .find_map(|w| w.parse().ok())
        .unwrap_or(3);
    let llm = words.contains(&"llm") || words.contains(&"ai") || words.contains(&"llms");
    // "evolve fib" — scoped to one family when a known lineage is named
    let family = words.iter()
        .find(|w| veldt.functions.contains_key(**w))
        .map(|s| s.to_string());
    let mut steps = vec![step("plan", format!("Running {} evolution cycle{}{}{}.", cycles, if cycles == 1 { "" } else { "s" },
        if llm && crate::llm::configured() { " (LLM mutations)" } else { "" },
        family.as_deref().map(|f| format!(" on {}", f)).unwrap_or_default()))];
    let mut evolver = Evolver::new();
    evolver.llm = llm;
    let events = evolver.evolve_scoped(veldt, cycles, family.as_deref());
    for ev in &events {
        steps.push(step("obs", ev.format()));
    }
    let reply = if events.is_empty() {
        "Nothing to evolve — the garden needs grown functions first. Ask me to write one.".to_string()
    } else if llm && !crate::llm::configured() {
        format!("Evolution complete (mechanical — set VELDT_LLM_KEY for LLM mutations): {} event{}.", events.len(), if events.len() == 1 { "" } else { "s" })
    } else {
        format!("Evolution complete: {} event{}.", events.len(), if events.len() == 1 { "" } else { "s" })
    };
    AgentResponse {
        steps,
        reply,
        code: None,
        ran: false,
        output: None,
        success: None,
        events: events.iter().map(|e| e.format()).collect(),
    }
}

fn tend_intent(veldt: &mut Veldt) -> AgentResponse {
    let sick: Vec<(String, u32)> = veldt.functions.iter()
        .flat_map(|(name, variants)| {
            variants.iter()
                .filter(|e| matches!(e.health, Health::Sick(_)))
                .map(move |e| (name.clone(), e.id))
        })
        .collect();

    if sick.is_empty() {
        return plain("Everything in the garden is healthy — nothing to mend.");
    }

    let mut steps = vec![step("plan", format!("{} sick variant{} found. Attempting hybrid-merge healing.", sick.len(), if sick.len() == 1 { "" } else { "s" }))];
    let mut events = Vec::new();
    let mut healed = 0;
    for (name, id) in sick {
        let mut entry = veldt.functions[&name].iter()
            .find(|e| e.id == id).cloned().unwrap();
        if HealthChecker::heal(&mut entry, veldt) {
            healed += 1;
            let msg = format!("[HEALED] {}#{}", name, id);
            steps.push(step("ok", msg.clone()));
            events.push(msg);
            if let Some(variants) = veldt.functions.get_mut(&name) {
                for e in variants.iter_mut() {
                    if e.id == id { *e = entry.clone(); break; }
                }
            }
        } else {
            let msg = format!("{}#{} resisted healing.", name, id);
            steps.push(step("obs", msg.clone()));
            events.push(msg);
        }
    }
    AgentResponse {
        steps,
        reply: format!("Tended the garden: {}/{} sick variants healed.", healed, events.len()),
        code: None,
        ran: false,
        output: None,
        success: None,
        events,
    }
}

fn prune_intent(words: &[&str], veldt: &mut Veldt) -> AgentResponse {
    // find a token shaped like name#N
    for w in words {
        if let Some(pos) = w.find('#') {
            let name = &w[..pos];
            if let Ok(id) = w[pos + 1..].parse::<u32>() {
                if name.is_empty() {
                    break;
                }
                if veldt.prune(name, id) {
                    return AgentResponse {
                        steps: vec![
                            step("act", format!("Pruning {}#{}.", name, id)),
                            step("ok", format!("{}#{} is dead.", name, id)),
                        ],
                        reply: format!("Pruned {}#{} — it'll get an obituary at the next reap.", name, id),
                        code: None,
                        ran: false,
                        output: None,
                        success: None,
                        events: vec![format!("[PRUNED] {}#{}", name, id)],
                    };
                }
                return AgentResponse {
                    steps: vec![step("err", format!("{}#{} isn't in the garden.", name, id))],
                    reply: format!("No variant named {}#{} lives here.", name, id),
                    code: None,
                    ran: false,
                    output: None,
                    success: None,
                    events: Vec::new(),
                };
            }
        }
    }
    plain("Tell me which variant — e.g. \"prune fib#2\".")
}

fn status_intent(veldt: &Veldt) -> AgentResponse {
    if veldt.functions.is_empty() && veldt.variables.is_empty() && veldt.structs.is_empty() {
        return plain("The garden is bare soil. Ask me to write a function and I'll plant the first seed.");
    }
    let mut text = String::from("Garden census:\n");
    let mut names: Vec<&String> = veldt.functions.keys().collect();
    names.sort();
    for name in names {
        let variants = &veldt.functions[name];
        let healthy = variants.iter().filter(|e| matches!(e.health, Health::Healthy)).count();
        let sick = variants.iter().filter(|e| matches!(e.health, Health::Sick(_))).count();
        let fittest = veldt.get_fittest_variant(name).map(|e| e.id).unwrap_or(0);
        let probes = veldt.probes.get(name).map(|p| p.len()).unwrap_or(0);
        text.push_str(&format!(
            "  {} — {}v ({} healthy, {} sick), fittest #{}{}\n",
            name, variants.len(), healthy, sick, fittest,
            if probes > 0 { format!(", {} probes", probes) } else { String::new() }
        ));
    }
    if !veldt.variables.is_empty() {
        let mut vars: Vec<&String> = veldt.variables.keys().collect();
        vars.sort();
        text.push_str(&format!("  immortal vars: {}\n", vars.iter().map(|v| format!("${}", v)).collect::<Vec<_>>().join(", ")));
    }
    if !veldt.structs.is_empty() {
        let mut sts: Vec<&String> = veldt.structs.keys().collect();
        sts.sort();
        text.push_str(&format!("  immortal structs: {}\n", sts.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
    }
    text.push_str(&format!("  run count: {} | obituaries: {}", veldt.run_count, veldt.obituaries.len()));
    plain(&text)
}

// === Inspection & selection-pressure intents ===

/// First word that matches a garden lineage name, or a name#N pair.
fn find_lineage<'a>(words: &[&'a str], veldt: &Veldt) -> Option<String> {
    for w in words {
        let base = w.split('#').next().unwrap_or(w);
        if !base.is_empty() && veldt.functions.contains_key(base) {
            return Some(base.to_string());
        }
    }
    None
}

/// Per-family drill-down — the dashboard summary for one lineage.
fn family_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    let name = match find_lineage(words, veldt) {
        Some(n) => n,
        None => return plain("Which family? e.g. \"stats fib\" or \"how is fib doing\"."),
    };
    let vs = match veldt.functions.get(&name) {
        Some(v) if !v.is_empty() => v,
        _ => return plain(&format!("No lineage called '{}' lives in the garden.", name)),
    };
    let healthy = vs.iter().filter(|e| matches!(e.health, Health::Healthy)).count();
    let sick = vs.iter().filter(|e| matches!(e.health, Health::Sick(_))).count();
    let dying = vs.len() - healthy - sick;
    let canon = vs.iter().filter(|e| matches!(e.trust, crate::veldt::Trust::Canonical)).count();
    let estab = vs.iter().filter(|e| matches!(e.trust, crate::veldt::Trust::Established)).count();
    let peak = vs.iter().map(|e| e.fitness).fold(f64::NEG_INFINITY, f64::max);
    let avg = vs.iter().map(|e| e.fitness).sum::<f64>() / vs.len() as f64;
    let probes = veldt.probes.get(&name).map(|p| p.len()).unwrap_or(0);
    let deaths = veldt.obituaries.iter().filter(|o| o.name == name).count();
    let traces = veldt.traces.get(&name).map(|t| t.len()).unwrap_or(0);
    let mut niches: Vec<&String> = vs.iter()
        .flat_map(|e| e.niche_scores.keys()).collect();
    niches.sort(); niches.dedup();
    let mut origins: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for e in vs.iter() {
        *origins.entry(e.origin.split(' ').next().unwrap_or("seed")).or_insert(0) += 1;
    }
    let champ = veldt.get_fittest_variant(&name).map(|e| e.id).unwrap_or(0);
    let weak = vs.iter().min_by(|a, b| a.fitness.partial_cmp(&b.fitness)
        .unwrap_or(std::cmp::Ordering::Equal)).map(|e| e.id).unwrap_or(0);
    let mut text = format!("{} — {} variants ({} healthy, {} sick, {} dying)\n",
        name, vs.len(), healthy, sick, dying);
    text.push_str(&format!("  fitness: peak {:.2} avg {:.2} | champion #{} weakest #{}\n",
        peak, avg, champ, weak));
    text.push_str(&format!("  trust: {} canonical, {} established, {} seedling\n",
        canon, estab, vs.len() - canon - estab));
    text.push_str(&format!("  {} probes, {} traces, {} deaths\n", probes, traces, deaths));
    if !niches.is_empty() {
        text.push_str(&format!("  niches: {}\n", niches.iter().map(|n| n.as_str())
            .collect::<Vec<_>>().join(", ")));
    }
    text.push_str(&format!("  origins: {}", origins.iter()
        .map(|(o, c)| format!("{}×{}", o, c)).collect::<Vec<_>>().join(", ")));
    plain(&text)
}

/// Triage — the variants that need attention across the whole garden.
fn triage_intent(veldt: &Veldt) -> AgentResponse {
    let mut sick: Vec<(&String, &FunctionEntry)> = Vec::new();
    let mut dying: Vec<(&String, &FunctionEntry)> = Vec::new();
    let mut weakest: Vec<(&String, &FunctionEntry)> = Vec::new();
    for (name, vs) in &veldt.functions {
        for e in vs {
            if matches!(e.health, Health::Sick(_)) { sick.push((name, e)); }
            if matches!(e.health, Health::Dying) { dying.push((name, e)); }
        }
        if let Some(w) = vs.iter().min_by(|a, b| a.fitness.partial_cmp(&b.fitness)
            .unwrap_or(std::cmp::Ordering::Equal)) {
            weakest.push((name, w));
        }
    }
    if sick.is_empty() && dying.is_empty() {
        return plain("Nothing needs attention — the garden is healthy.");
    }
    let mut text = String::from("Triage:\n");
    for (n, e) in &dying {
        text.push_str(&format!("  DYING {}#{}\n", n, e.id));
    }
    for (n, e) in sick.iter().take(10) {
        let err = match &e.health { Health::Sick(m) => m.as_str(), _ => "" };
        text.push_str(&format!("  sick {}#{} — {}\n", n, e.id, err));
    }
    if sick.len() > 10 {
        text.push_str(&format!("  ... {} more sick\n", sick.len() - 10));
    }
    weakest.sort_by(|a, b| a.1.fitness.partial_cmp(&b.1.fitness)
        .unwrap_or(std::cmp::Ordering::Equal));
    text.push_str("  weakest per family:");
    for (n, e) in weakest.iter().take(5) {
        text.push_str(&format!(" {}#{}({:.2})", n, e.id, e.fitness));
    }
    plain(&text)
}

/// Compare two variants — "compare fib#1 fib#2" shows the differing lines.
fn compare_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    let refs: Vec<(String, u32)> = words.iter().filter_map(|w| {
        let (n, i) = w.split_once('#')?;
        i.parse::<u32>().ok().map(|id| (n.to_string(), id))
    }).collect();
    if refs.len() < 2 {
        return plain("Compare what? e.g. \"compare fib#1 fib#2\".");
    }
    let (name_a, id_a) = &refs[0];
    let (name_b, id_b) = &refs[1];
    if name_a != name_b {
        return plain("Comparison works inside one lineage — both refs must share a name.");
    }
    let vs = match veldt.functions.get(name_a) {
        Some(v) => v,
        None => return plain(&format!("No lineage called '{}'.", name_a)),
    };
    let ea = match vs.iter().find(|e| e.id == *id_a) {
        Some(e) => e,
        None => return plain(&format!("{}#{} isn't in the garden.", name_a, id_a)),
    };
    let eb = match vs.iter().find(|e| e.id == *id_b) {
        Some(e) => e,
        None => return plain(&format!("{}#{} isn't in the garden.", name_b, id_b)),
    };
    let sa = crate::fmt::format_program(&ea.body);
    let sb = crate::fmt::format_program(&eb.body);
    let la: std::collections::HashSet<&str> = sa.lines()
        .map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    let lb: std::collections::HashSet<&str> = sb.lines()
        .map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    let only_a: Vec<&str> = sa.lines().filter(|l| {
        let t = l.trim(); !t.is_empty() && !lb.contains(t)
    }).collect();
    let only_b: Vec<&str> = sb.lines().filter(|l| {
        let t = l.trim(); !t.is_empty() && !la.contains(t)
    }).collect();
    let mut text = format!("{}#{} (fit {:.2}) vs {}#{} (fit {:.2})\n",
        name_a, id_a, ea.fitness, name_b, id_b, eb.fitness);
    if only_a.is_empty() && only_b.is_empty() {
        text.push_str("  identical — no differing lines");
    } else {
        for l in &only_a { text.push_str(&format!("  - {}\n", l.trim())); }
        for l in &only_b { text.push_str(&format!("  + {}\n", l.trim())); }
        text.push_str(&format!("  {} lines only in #{}, {} only in #{}",
            only_a.len(), id_a, only_b.len(), id_b));
    }
    plain(&text)
}

/// Coverage — probe/test coverage for one family or the whole garden.
fn coverage_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    if let Some(name) = find_lineage(words, veldt) {
        // scoped — this family's probes + which niches they target
        let probes = veldt.probes.get(&name).map(|p| p.as_slice()).unwrap_or(&[]);
        let vs = match veldt.functions.get(&name) {
            Some(v) if !v.is_empty() => v,
            _ => return plain(&format!("No lineage called '{}'.", name)),
        };
        let mut niche_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for p in probes { *niche_counts.entry(p.niche.as_str()).or_insert(0) += 1; }
        let covered: std::collections::HashSet<&String> = vs.iter()
            .flat_map(|e| e.niche_scores.keys()).collect();
        let mut text = format!("{} — {} probes, {} niches covered\n",
            name, probes.len(), covered.len());
        let mut sorted: Vec<(&str, usize)> = niche_counts.iter()
            .map(|(n, c)| (*n, *c)).collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        for (n, c) in sorted.iter().take(8) {
            text.push_str(&format!("  {} — {} probes\n", n, c));
        }
        let gaps: Vec<&&String> = covered.iter()
            .filter(|n| !niche_counts.contains_key(n.as_str())).collect();
        if !gaps.is_empty() {
            text.push_str(&format!("  gaps: {}", gaps.iter().map(|n| n.as_str())
                .collect::<Vec<_>>().join(", ")));
        }
        return plain(&text);
    }
    // global — total probes, families covered, niche distribution
    let total: usize = veldt.probes.values().map(|p| p.len()).sum();
    let mut niche_hist: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for p in veldt.probes.values() {
        for pr in p { *niche_hist.entry(pr.niche.clone()).or_insert(0) += 1; }
    }
    let mut text = format!("{} probes across {} families\n", total, veldt.probes.len());
    let mut sorted: Vec<(&String, usize)> = niche_hist.iter()
        .map(|(n, c)| (n, *c)).collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    for (n, c) in sorted.iter().take(8) {
        text.push_str(&format!("  {} — {}\n", n, c));
    }
    plain(&text)
}

/// Lineage — "who are fib#145's parents" or "fib lineage" (the breeding map).
fn lineage_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    if let Some((name, Some(id))) = parse_variant_ref(words) {
        // one variant — parents + children
        let vs = match veldt.functions.get(&name) {
            Some(v) => v,
            None => return plain(&format!("No lineage called '{}'.", name)),
        };
        let e = match vs.iter().find(|e| e.id == id) {
            Some(e) => e,
            None => return plain(&format!("{}#{} isn't in the garden.", name, id)),
        };
        let mut text = format!("{}#{} — {}\n", name, id, e.origin);
        text.push_str(&format!("  parents: {}\n",
            if e.parents.is_empty() { "none (seed)".into() }
            else { e.parents.iter().map(|p| format!("#{}", p))
                .collect::<Vec<_>>().join(", ") }));
        let children: Vec<u32> = vs.iter()
            .filter(|v| v.parents.contains(&e.id)).map(|v| v.id).collect();
        text.push_str(&format!("  children: {}",
            if children.is_empty() { "none".into() }
            else { children.iter().map(|c| format!("#{}", c))
                .collect::<Vec<_>>().join(", ") }));
        return plain(&text);
    }
    if let Some(name) = find_lineage(words, veldt) {
        // family — the breeding map (parents → children)
        let vs = match veldt.functions.get(&name) {
            Some(v) => v,
            None => return plain(&format!("No lineage called '{}'.", name)),
        };
        let mut text = format!("{} lineage — {} variants\n", name, vs.len());
        for e in vs {
            text.push_str(&format!("  #{} ← {} ({})\n", e.id,
                if e.parents.is_empty() { "seed".into() }
                else { e.parents.iter().map(|p| format!("#{}", p))
                    .collect::<Vec<_>>().join(", ") },
                e.origin));
        }
        return plain(&text);
    }
    plain("Whose lineage? e.g. \"fib#145 parents\" or \"fib lineage\".")
}

/// Usage — the workload leaders (variants by usage_count).
fn usage_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    if let Some(name) = find_lineage(words, veldt) {
        let vs = match veldt.functions.get(&name) {
            Some(v) => v,
            None => return plain(&format!("No lineage called '{}'.", name)),
        };
        let mut sorted: Vec<&FunctionEntry> = vs.iter().collect();
        sorted.sort_by(|a, b| b.usage_count.cmp(&a.usage_count));
        let total: u64 = vs.iter().map(|e| e.usage_count).sum();
        let mut text = format!("{} — {} total calls\n", name, total);
        for e in sorted.iter().take(5) {
            text.push_str(&format!("  #{} — {} calls (fit {:.2})\n",
                e.id, e.usage_count, e.fitness));
        }
        return plain(&text);
    }
    let mut all: Vec<(&String, &FunctionEntry)> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().map(move |e| (n, e))).collect();
    all.sort_by(|a, b| b.1.usage_count.cmp(&a.1.usage_count));
    let mut text = String::from("Hottest variants:\n");
    for (n, e) in all.iter().take(8) {
        text.push_str(&format!("  {}#{} — {} calls\n", n, e.id, e.usage_count));
    }
    plain(&text)
}

/// Efficiency — the speed leaders (variants by efficiency = fitness/size).
fn efficiency_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    if let Some(name) = find_lineage(words, veldt) {
        let vs = match veldt.functions.get(&name) {
            Some(v) => v,
            None => return plain(&format!("No lineage called '{}'.", name)),
        };
        let mut sorted: Vec<&FunctionEntry> = vs.iter().collect();
        sorted.sort_by(|a, b| b.efficiency.partial_cmp(&a.efficiency)
            .unwrap_or(std::cmp::Ordering::Equal));
        let mut text = format!("{} — efficiency ranking\n", name);
        for e in sorted.iter().take(5) {
            text.push_str(&format!("  #{} — {:.4} eff (fit {:.2})\n",
                e.id, e.efficiency, e.fitness));
        }
        return plain(&text);
    }
    let mut all: Vec<(&String, &FunctionEntry)> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().map(move |e| (n, e))).collect();
    all.sort_by(|a, b| b.1.efficiency.partial_cmp(&a.1.efficiency)
        .unwrap_or(std::cmp::Ordering::Equal));
    let mut text = String::from("Most efficient variants:\n");
    for (n, e) in all.iter().take(8) {
        text.push_str(&format!("  {}#{} — {:.4} eff (fit {:.2})\n",
            n, e.id, e.efficiency, e.fitness));
    }
    plain(&text)
}

/// Trust — "what's canonical" / "trust levels" — scoped or global.
fn trust_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    use crate::veldt::Trust;
    let tag = |e: &FunctionEntry| match e.trust {
        Trust::Canonical => "canonical",
        Trust::Established => "established",
        Trust::Seedling => "seedling",
    };
    if let Some(name) = find_lineage(words, veldt) {
        let vs = match veldt.functions.get(&name) {
            Some(v) => v,
            None => return plain(&format!("No lineage called '{}'.", name)),
        };
        let mut text = format!("{} — trust levels\n", name);
        for e in vs {
            text.push_str(&format!("  #{} — {} (fit {:.2})\n", e.id, tag(e), e.fitness));
        }
        return plain(&text);
    }
    let canon: Vec<(&String, &FunctionEntry)> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().filter_map(move |e|
            if matches!(e.trust, Trust::Canonical) { Some((n, e)) } else { None })).collect();
    let estab: usize = veldt.functions.values().flatten()
        .filter(|e| matches!(e.trust, Trust::Established)).count();
    let seed: usize = veldt.functions.values().flatten()
        .filter(|e| matches!(e.trust, Trust::Seedling)).count();
    let mut text = format!("Trust: {} canonical, {} established, {} seedling\n",
        canon.len(), estab, seed);
    for (n, e) in &canon {
        text.push_str(&format!("  {}#{} — canonical (fit {:.2})\n", n, e.id, e.fitness));
    }
    plain(&text)
}

/// Age — "oldest variants" / "newest fib" — scoped or global.
fn age_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    let want_newest = words.iter().any(|w| matches!(*w, "newest" | "youngest"));
    if let Some(name) = find_lineage(words, veldt) {
        let vs = match veldt.functions.get(&name) {
            Some(v) => v,
            None => return plain(&format!("No lineage called '{}'.", name)),
        };
        let mut sorted: Vec<&FunctionEntry> = vs.iter().collect();
        if want_newest {
            sorted.sort_by(|a, b| a.age.cmp(&b.age));
        } else {
            sorted.sort_by(|a, b| b.age.cmp(&a.age));
        }
        let mut text = format!("{} — by age\n", name);
        for e in sorted.iter().take(5) {
            text.push_str(&format!("  #{} — {} runs (fit {:.2})\n",
                e.id, e.age, e.fitness));
        }
        return plain(&text);
    }
    let mut all: Vec<(&String, &FunctionEntry)> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().map(move |e| (n, e))).collect();
    if want_newest {
        all.sort_by(|a, b| a.1.age.cmp(&b.1.age));
    } else {
        all.sort_by(|a, b| b.1.age.cmp(&a.1.age));
    }
    let label = if want_newest { "Newest" } else { "Oldest" };
    let mut text = format!("{} variants:\n", label);
    for (n, e) in all.iter().take(8) {
        text.push_str(&format!("  {}#{} — {} runs\n", n, e.id, e.age));
    }
    plain(&text)
}

/// Niches — "niche gaps" / "uncovered" — coverage map per family or global.
fn niche_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    if let Some(name) = find_lineage(words, veldt) {
        let vs = match veldt.functions.get(&name) {
            Some(v) => v,
            None => return plain(&format!("No lineage called '{}'.", name)),
        };
        // niche -> (avg score, how many variants cover it)
        let mut map: std::collections::HashMap<&str, (f64, usize)> =
            std::collections::HashMap::new();
        for e in vs {
            for (n, s) in &e.niche_scores {
                let ent = map.entry(n.as_str()).or_insert((0.0, 0));
                ent.0 += s; ent.1 += 1;
            }
        }
        let mut text = format!("{} — {} niches\n", name, map.len());
        let mut sorted: Vec<(&str, (f64, usize))> = map.iter()
            .map(|(n, (s, c))| (*n, (*s, *c))).collect();
        sorted.sort_by(|a, b| (a.1.0 / a.1.1.max(1) as f64)
            .partial_cmp(&(b.1.0 / b.1.1.max(1) as f64))
            .unwrap_or(std::cmp::Ordering::Equal));
        for (n, (sum, count)) in &sorted {
            let avg = sum / (*count).max(1) as f64;
            let flag = if avg < 1.0 { " ← gap" }
                       else if *count < vs.len() { " ← partial" }
                       else { "" };
            text.push_str(&format!("  {} — avg {:.2} ({}/{} vars){}\n",
                n, avg, count, vs.len(), flag));
        }
        return plain(&text);
    }
    // global — niche coverage across the garden
    let mut all: std::collections::HashMap<String, (f64, usize)> =
        std::collections::HashMap::new();
    for vs in veldt.functions.values() {
        for e in vs {
            for (n, s) in &e.niche_scores {
                let ent = all.entry(n.clone()).or_insert((0.0, 0));
                ent.0 += s; ent.1 += 1;
            }
        }
    }
    let mut text = format!("{} niches across the garden\n", all.len());
    let mut sorted: Vec<(&String, (f64, usize))> = all.iter()
        .map(|(n, (s, c))| (n, (*s, *c))).collect();
    sorted.sort_by(|a, b| (a.1.0 / a.1.1.max(1) as f64)
        .partial_cmp(&(b.1.0 / b.1.1.max(1) as f64))
        .unwrap_or(std::cmp::Ordering::Equal));
    for (n, (sum, count)) in sorted.iter().take(10) {
        text.push_str(&format!("  {} — avg {:.2} ({} entries)\n",
            n, sum / (*count).max(1) as f64, count));
    }
    plain(&text)
}

/// History — "what changed" — recent births + deaths across the garden.
fn history_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    // scoped to a family if one is named
    let scoped = find_lineage(words, veldt);
    // collect events: (timestamp, "born name#id origin" | "died name#id cause")
    let mut events: Vec<(u64, String)> = Vec::new();
    for (n, vs) in &veldt.functions {
        if let Some(f) = &scoped { if n != f { continue; } }
        for e in vs {
            events.push((e.born, format!("born {}#{} — {}", n, e.id, e.origin)));
        }
    }
    for o in &veldt.obituaries {
        if let Some(f) = &scoped { if o.name != *f { continue; } }
        events.push((o.timestamp, format!("died {}#{} — {}", o.name, o.id, o.cause_of_death)));
    }
    events.sort_by(|a, b| b.0.cmp(&a.0)); // newest first
    if events.is_empty() {
        return plain("No history yet — the garden is still a seedbed.");
    }
    let mut text = if let Some(f) = &scoped {
        format!("{} — recent events\n", f)
    } else {
        String::from("Recent garden events\n")
    };
    for (_, ev) in events.iter().take(12) {
        text.push_str(&format!("  {}\n", ev));
    }
    plain(&text)
}

/// Turnover — births vs deaths per family ("churn", "mortality").
fn turnover_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    if let Some(name) = find_lineage(words, veldt) {
        let born = veldt.functions.get(&name).map(|v| v.len()).unwrap_or(0);
        let died = veldt.obituaries.iter().filter(|o| o.name == name).count();
        let rate = if born + died > 0 { died as f64 / (born + died) as f64 } else { 0.0 };
        let mut text = format!("{} — {} alive, {} died (turnover {:.0}%)\n",
            name, born, died, rate * 100.0);
        let mut causes: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for o in veldt.obituaries.iter().filter(|o| o.name == name) {
            *causes.entry(o.cause_of_death.as_str()).or_insert(0) += 1;
        }
        for (c, n) in causes {
            text.push_str(&format!("  {} — {}\n", c, n));
        }
        return plain(&text);
    }
    let mut rows: Vec<(&String, usize, usize)> = veldt.functions.iter()
        .map(|(n, vs)| {
            let died = veldt.obituaries.iter().filter(|o| o.name == *n).count();
            (n, vs.len(), died)
        }).collect();
    rows.sort_by(|a, b| (b.2 as f64 / (b.1 + b.2).max(1) as f64)
        .partial_cmp(&(a.2 as f64 / (a.1 + a.2).max(1) as f64))
        .unwrap_or(std::cmp::Ordering::Equal));
    let mut text = String::from("Turnover (alive | dead | rate):\n");
    for (n, alive, dead) in rows.iter().take(8) {
        let rate = if alive + dead > 0 { *dead as f64 / (alive + dead) as f64 } else { 0.0 };
        text.push_str(&format!("  {} — {} | {} | {:.0}%\n", n, alive, dead, rate * 100.0));
    }
    plain(&text)
}

/// Pressure — fitness spread + rivals per family ("selection pressure").
fn pressure_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    let pressure_of = |vs: &[FunctionEntry]| -> (f64, usize) {
        if vs.len() < 2 { return (0.0, 0); }
        let mean = vs.iter().map(|e| e.fitness).sum::<f64>() / vs.len() as f64;
        let sd = (vs.iter().map(|e| (e.fitness - mean).powi(2)).sum::<f64>()
            / vs.len() as f64).sqrt();
        (sd, vs.len() - 1)
    };
    if let Some(name) = find_lineage(words, veldt) {
        let vs = match veldt.functions.get(&name) {
            Some(v) => v,
            None => return plain(&format!("No lineage called '{}'.", name)),
        };
        let (sd, rivals) = pressure_of(vs);
        let peak = vs.iter().map(|e| e.fitness).fold(f64::NEG_INFINITY, f64::max);
        let label = if sd < 0.01 { "tight" } else if sd < 0.1 { "moderate" } else { "loose" };
        let mut text = format!("{} — {} rivals, σ {:.4} ({} pressure, peak {:.2})\n",
            name, rivals, sd, label, peak);
        let mut sorted: Vec<&FunctionEntry> = vs.iter().collect();
        sorted.sort_by(|a, b| b.fitness.partial_cmp(&a.fitness)
            .unwrap_or(std::cmp::Ordering::Equal));
        for e in sorted.iter().take(5) {
            text.push_str(&format!("  #{} — {:.2}\n", e.id, e.fitness));
        }
        return plain(&text);
    }
    let mut rows: Vec<(&String, f64, usize)> = veldt.functions.iter()
        .map(|(n, vs)| { let (sd, r) = pressure_of(vs); (n, sd, r) }).collect();
    rows.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut text = String::from("Selection pressure (tightest first):\n");
    for (n, sd, r) in rows.iter().take(8) {
        text.push_str(&format!("  {} — σ {:.4} ({} rivals)\n", n, sd, r));
    }
    plain(&text)
}

/// Healing — "who's healing" — variants with healing attempts logged.
fn healing_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    let scoped = find_lineage(words, veldt);
    let scoped_ref = &scoped;
    let mut rows: Vec<(&String, &FunctionEntry)> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().filter_map(move |e|
            if !e.healing_attempts.is_empty()
                && scoped_ref.as_ref().map(|f| f == n).unwrap_or(true) {
                Some((n, e))
            } else { None })).collect();
    if rows.is_empty() {
        return plain("Nobody's healing — no healing attempts logged.");
    }
    rows.sort_by(|a, b| b.1.healing_attempts.len().cmp(&a.1.healing_attempts.len()));
    let mut text = String::from("Healing log:\n");
    for (n, e) in rows.iter().take(10) {
        let state = if matches!(e.health, Health::Healthy) { "recovered" } else { "still sick" };
        text.push_str(&format!("  {}#{} — {} attempts, {} (last: {})\n",
            n, e.id, e.healing_attempts.len(), state,
            e.healing_attempts.last().map(|s| s.as_str()).unwrap_or("")));
    }
    plain(&text)
}

/// Sources — "where did fib come from" / "mutation sources" — origin breakdown.
fn sources_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    let scoped = find_lineage(words, veldt);
    // origin-prefix -> (count, sum fitness)
    let mut map: std::collections::HashMap<String, (usize, f64)> =
        std::collections::HashMap::new();
    for (n, vs) in &veldt.functions {
        if let Some(f) = &scoped { if n != f { continue; } }
        for e in vs {
            let src = e.origin.split(' ').next().unwrap_or("seed").to_string();
            let ent = map.entry(src).or_insert((0, 0.0));
            ent.0 += 1; ent.1 += e.fitness;
        }
    }
    if map.is_empty() {
        return plain("No variants to report on.");
    }
    let mut sorted: Vec<(String, usize, f64)> = map.iter()
        .map(|(s, (c, f))| (s.clone(), *c, *f / (*c).max(1) as f64)).collect();
    sorted.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    let mut text = if let Some(f) = &scoped {
        format!("{} — mutation sources\n", f)
    } else {
        String::from("Mutation sources (best avg fitness first)\n")
    };
    for (src, count, avg) in sorted {
        text.push_str(&format!("  {} — {} variants, avg fit {:.2}\n", src, count, avg));
    }
    plain(&text)
}

/// Lifecycle — "who's growing" / "aging" — age vs lifespan split.
fn lifecycle_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    let scoped = find_lineage(words, veldt);
    let scoped_ref = &scoped;
    let want_aging = words.iter().any(|w|
        matches!(*w, "aging" | "old" | "over" | "past"));
    let mut rows: Vec<(&String, &FunctionEntry)> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().filter_map(move |e|
            if scoped_ref.as_ref().map(|f| f == n).unwrap_or(true)
                && ((e.age as f64) < e.lifespan) != want_aging {
                Some((n, e))
            } else { None })).collect();
    rows.sort_by(|a, b| b.1.age.cmp(&a.1.age));
    let label = if want_aging { "Aging (past lifespan)" } else { "Growing (within lifespan)" };
    if rows.is_empty() {
        return plain(&format!("{} — none.", label));
    }
    let mut text = format!("{}:\n", label);
    for (n, e) in rows.iter().take(10) {
        text.push_str(&format!("  {}#{} — age {} (lifespan {:.0})\n",
            n, e.id, e.age, e.lifespan));
    }
    if rows.len() > 10 {
        text.push_str(&format!("  ... {} more\n", rows.len() - 10));
    }
    plain(&text)
}

/// Size — "biggest fib" / "smallest code" — code-size ranking.
fn size_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    let scoped = find_lineage(words, veldt);
    let want_big = words.iter().any(|w| matches!(*w, "biggest" | "largest" | "bloated"));
    let size_of = |e: &FunctionEntry| crate::fmt::format_program(&e.body).len();
    if let Some(name) = &scoped {
        let vs = match veldt.functions.get(name) {
            Some(v) => v,
            None => return plain(&format!("No lineage called '{}'.", name)),
        };
        let mut sorted: Vec<&FunctionEntry> = vs.iter().collect();
        if want_big {
            sorted.sort_by_key(|e| std::cmp::Reverse(size_of(e)));
        } else {
            sorted.sort_by_key(|e| size_of(e));
        }
        let mut text = format!("{} — by code size ({} first)\n",
            name, if want_big { "largest" } else { "smallest" });
        for e in sorted.iter().take(5) {
            text.push_str(&format!("  #{} — {} chars (fit {:.2})\n",
                e.id, size_of(e), e.fitness));
        }
        return plain(&text);
    }
    let mut all: Vec<(&String, &FunctionEntry)> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().map(move |e| (n, e))).collect();
    if want_big {
        all.sort_by_key(|(_, e)| std::cmp::Reverse(size_of(e)));
    } else {
        all.sort_by_key(|(_, e)| size_of(e));
    }
    let label = if want_big { "Largest" } else { "Smallest" };
    let mut text = format!("{} variants:\n", label);
    for (n, e) in all.iter().take(8) {
        text.push_str(&format!("  {}#{} — {} chars (fit {:.2})\n",
            n, e.id, size_of(e), e.fitness));
    }
    plain(&text)
}

/// Siblings — "fib#216 siblings" — variants sharing a parent.
fn siblings_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    let (name, id) = match parse_variant_ref(words) {
        Some((n, Some(i))) => (n, i),
        _ => return plain("Whose siblings? e.g. \"fib#216 siblings\"."),
    };
    let vs = match veldt.functions.get(&name) {
        Some(v) => v,
        None => return plain(&format!("No lineage called '{}'.", name)),
    };
    let e = match vs.iter().find(|e| e.id == id) {
        Some(e) => e,
        None => return plain(&format!("{}#{} isn't in the garden.", name, id)),
    };
    if e.parents.is_empty() {
        return plain(&format!("{}#{} is a seed — no parents, no siblings.", name, id));
    }
    let sibs: Vec<&FunctionEntry> = vs.iter()
        .filter(|v| v.id != e.id
            && v.parents.iter().any(|p| e.parents.contains(p)))
        .collect();
    let mut text = format!("{}#{} — parents {:?}\n", name, id, e.parents);
    if sibs.is_empty() {
        text.push_str("  no siblings — only child");
    } else {
        for s in &sibs {
            let shared: Vec<u32> = s.parents.iter()
                .filter(|p| e.parents.contains(p)).copied().collect();
            text.push_str(&format!("  #{} (fit {:.2}) — shared parents {:?}\n",
                s.id, s.fitness, shared));
        }
    }
    plain(&text)
}

/// Stale — "forgotten" / "dormant" — variants by runs-since-last-call.
fn stale_intent(veldt: &Veldt) -> AgentResponse {
    let mut rows: Vec<(&String, &FunctionEntry, u64)> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().map(move |e|
            (n, e, veldt.run_count.saturating_sub(e.last_used)))).collect();
    rows.sort_by(|a, b| b.2.cmp(&a.2));
    let active = rows.iter().take(10).filter(|(_, _, s)| *s == 0).count();
    if rows.iter().all(|(_, _, s)| *s == 0) {
        return plain("Nothing is stale — every variant was called this run.");
    }
    let mut text = String::from("Stalest variants (runs since last call):\n");
    for (n, e, stale) in rows.iter().take(8) {
        text.push_str(&format!("  {}#{} — {} runs stale (fit {:.2})\n",
            n, e.id, stale, e.fitness));
    }
    text.push_str(&format!("({}/{} variants called this run)", active, rows.len()));
    plain(&text)
}

/// Dead — variants born from dead-code-removal mutations.
fn dead_intent(veldt: &Veldt) -> AgentResponse {
    let mut rows: Vec<(&String, &FunctionEntry)> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().filter_map(move |e|
            if e.origin.contains("dead") { Some((n, e)) } else { None })).collect();
    if rows.is_empty() {
        return plain("No dead-code-removal variants in the garden.");
    }
    rows.sort_by(|a, b| b.1.fitness.partial_cmp(&a.1.fitness)
        .unwrap_or(std::cmp::Ordering::Equal));
    let mut text = format!("{} dead-code-removal variants:\n", rows.len());
    for (n, e) in rows.iter().take(8) {
        text.push_str(&format!("  {}#{} — fit {:.2} ({})\n", n, e.id, e.fitness, e.origin));
    }
    plain(&text)
}

/// Lint — "lint fib" / "code health" — strict-lint variant bodies:
/// per-family warning counts, defect-class called out, or a
/// garden-wide census when no family is named.
fn lint_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    let lint_entry = |name: &str, e: &FunctionEntry| -> Vec<String> {
        let wrapped = crate::ast::Stmt::Grow(crate::ast::GrowItem::Fn(
            name.to_string(), e.params.clone(),
            e.body.clone(), e.tests.clone()));
        crate::lint(&[wrapped]).into_iter()
            .filter(|w| !w.contains("defined but never called")
                && !w.contains("no top-level statements"))
            .collect()
    };
    if let Some(name) = find_lineage(words, veldt) {
        let vs = match veldt.functions.get(&name) {
            Some(v) => v,
            None => return plain(&format!("No lineage called '{}'.", name)),
        };
        let mut bugs = 0usize;
        let mut total = 0usize;
        let mut text = String::new();
        for e in vs {
            let warns = lint_entry(&name, e);
            if warns.is_empty() { continue; }
            total += warns.len();
            let n_bugs = warns.iter()
                .filter(|w| crate::evolve::is_realbug_warning(w)).count();
            bugs += n_bugs;
            text.push_str(&format!("  {}#{} — {} warnings ({} defect-class)\n",
                                   name, e.id, warns.len(), n_bugs));
            for w in warns.iter().take(3) {
                text.push_str(&format!("    ⚠ {}\n", w));
            }
        }
        if total == 0 {
            return plain(&format!("{} is lint-clean — no warnings across {} variant(s).",
                                  name, vs.len()));
        }
        return plain(&format!("{}: {} warnings ({} defect-class) across the family:\n{}",
                              name, total, bugs, text));
    }
    // garden-wide census — worst families first
    let mut per_fam: Vec<(&String, usize, usize)> = Vec::new();
    let mut total = 0usize;
    let mut bugs = 0usize;
    for (name, vs) in &veldt.functions {
        let (mut fw, mut fb) = (0usize, 0usize);
        for e in vs {
            for w in lint_entry(name, e) {
                fw += 1;
                if crate::evolve::is_realbug_warning(&w) { fb += 1; }
            }
        }
        if fw > 0 { per_fam.push((name, fw, fb)); }
        total += fw;
        bugs += fb;
    }
    if total == 0 {
        return plain("The whole garden is lint-clean — zero warnings.");
    }
    per_fam.sort_by(|a, b| b.1.cmp(&a.1));
    let mut text = format!("Garden lint: {} warnings ({} defect-class):\n", total, bugs);
    for (n, w, b) in per_fam.iter().take(8) {
        text.push_str(&format!("  {} — {} warnings ({} defect)\n", n, w, b));
    }
    plain(&text)
}

/// Best — "who's winning" / "leaderboard" — fitness ranking.
fn best_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    if let Some(name) = find_lineage(words, veldt) {
        let vs = match veldt.functions.get(&name) {
            Some(v) => v,
            None => return plain(&format!("No lineage called '{}'.", name)),
        };
        let mut sorted: Vec<&FunctionEntry> = vs.iter().collect();
        sorted.sort_by(|a, b| b.fitness.partial_cmp(&a.fitness)
            .unwrap_or(std::cmp::Ordering::Equal));
        let mut text = format!("{} — fitness leaderboard\n", name);
        for e in sorted.iter().take(3) {
            text.push_str(&format!("  #{} — {:.2} ({})\n", e.id, e.fitness, e.origin));
        }
        return plain(&text);
    }
    // global — peak variant per family
    let mut rows: Vec<(&String, &FunctionEntry)> = veldt.functions.iter()
        .filter_map(|(n, vs)| vs.iter()
            .max_by(|a, b| a.fitness.partial_cmp(&b.fitness)
                .unwrap_or(std::cmp::Ordering::Equal)).map(|e| (n, e)))
        .collect();
    rows.sort_by(|a, b| b.1.fitness.partial_cmp(&a.1.fitness)
        .unwrap_or(std::cmp::Ordering::Equal));
    let mut text = String::from("Garden leaderboard (peak per family):\n");
    for (n, e) in rows.iter().take(8) {
        text.push_str(&format!("  {}#{} — {:.2}\n", n, e.id, e.fitness));
    }
    plain(&text)
}

/// Proven — "what's proven" / "production-ready" — healthy + established/canonical.
fn proven_intent(veldt: &Veldt) -> AgentResponse {
    use crate::veldt::Trust;
    let mut rows: Vec<(&String, &FunctionEntry)> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().filter_map(move |e|
            if matches!(e.health, Health::Healthy)
                && matches!(e.trust, Trust::Established | Trust::Canonical) {
                Some((n, e))
            } else { None })).collect();
    rows.sort_by(|a, b| b.1.fitness.partial_cmp(&a.1.fitness)
        .unwrap_or(std::cmp::Ordering::Equal));
    let total: usize = veldt.functions.values().map(|v| v.len()).sum();
    if rows.is_empty() {
        return plain("Nothing is proven yet — the garden is still experimental.");
    }
    let mut text = format!("Proven ({} of {} variants):\n", rows.len(), total);
    for (n, e) in rows.iter().take(10) {
        let t = match e.trust {
            Trust::Canonical => "canonical",
            Trust::Established => "established",
            _ => "",
        };
        text.push_str(&format!("  {}#{} — {:.2} {}\n", n, e.id, e.fitness, t));
    }
    plain(&text)
}

/// Risky — "experimental" / "untested" — sick/dying or seedling variants.
fn risky_intent(veldt: &Veldt) -> AgentResponse {
    use crate::veldt::Trust;
    let mut rows: Vec<(&String, &FunctionEntry)> = veldt.functions.iter()
        .flat_map(|(n, vs)| vs.iter().filter_map(move |e|
            if !matches!(e.health, Health::Healthy)
                || matches!(e.trust, Trust::Seedling) {
                Some((n, e))
            } else { None })).collect();
    rows.sort_by(|a, b| a.1.fitness.partial_cmp(&b.1.fitness)
        .unwrap_or(std::cmp::Ordering::Equal));
    if rows.is_empty() {
        return plain("Nothing risky — every variant is healthy + established.");
    }
    let total: usize = veldt.functions.values().map(|v| v.len()).sum();
    let mut text = format!("Risky ({} of {} variants):\n", rows.len(), total);
    for (n, e) in rows.iter().take(10) {
        let tag = if matches!(e.trust, Trust::Seedling) { "seedling" }
            else { "unhealthy" };
        text.push_str(&format!("  {}#{} — {:.2} ({})\n", n, e.id, e.fitness, tag));
    }
    plain(&text)
}

/// Verify — "verify fib" — run the domain-verification, report divergence.
fn verify_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    let name = match find_lineage(words, veldt) {
        Some(n) => n,
        None => return plain("Verify which family? e.g. \"verify fib\"."),
    };
    match crate::verify_family_find_divergent(&name) {
        Some(id) => plain(&format!(
            "{} — variant #{} diverges on a sampled input. The family doesn't agree.", name, id)),
        None => plain(&format!(
            "{} — all variants agree on sampled inputs. Verified.", name)),
    }
}

/// Trend — "fitness trend" / "improving" — the garden's fitness trajectory.
fn trend_intent(veldt: &Veldt) -> AgentResponse {
    let h = &veldt.fitness_history;
    if h.len() < 2 {
        return plain("Not enough fitness history to trend — keep evolving.");
    }
    let first = h.first().unwrap();
    let last = h.last().unwrap();
    let arrow = if last > first { "↑" } else if last < first { "↓" } else { "→" };
    let verdict = if last > first { "the garden is improving" }
        else if last < first { "the garden is declining — maybe tend it" }
        else { "holding steady" };
    plain(&format!("Fitness trend: {:.2} → {:.2} {}  {}\n{} over {} runs.",
        first, last, arrow, crate::spark(h), verdict, h.len()))
}

/// Parse a bare word that looks like a function name (optionally name#N).
fn parse_variant_ref(words: &[&str]) -> Option<(String, Option<u32>)> {
    for w in words {
        if let Some(pos) = w.find('#') {
            let name = &w[..pos];
            if let Ok(id) = w[pos + 1..].parse::<u32>() {
                if !name.is_empty() {
                    return Some((name.to_string(), Some(id)));
                }
            }
        }
    }
    None
}

fn show_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    let (name, want_id) = match parse_variant_ref(words) {
        Some(x) => x,
        None => match find_lineage(words, veldt) {
            Some(n) => (n, None),
            None => {
                let cand = words.iter().skip_while(|w| **w == "show" || **w == "me" || **w == "explain" || **w == "the" || **w == "source" || **w == "of" || **w == "inspect").next();
                return match cand {
                    Some(c) => plain(&format!("No lineage called '{}' lives in the garden.", c)),
                    None => plain("Show what? Name a lineage — e.g. \"show fib\" or \"show fib#2\"."),
                };
            }
        },
    };
    let variants = match veldt.functions.get(&name) {
        Some(v) if !v.is_empty() => v,
        _ => return plain(&format!("No lineage called '{}' lives in the garden.", name)),
    };
    let entry = match want_id {
        Some(id) => match variants.iter().find(|e| e.id == id) {
            Some(e) => e,
            None => return plain(&format!("{}#{} isn't in the garden.", name, id)),
        },
        None => match veldt.get_fittest_variant(&name) {
            Some(e) => e,
            None => return plain(&format!("Every {} variant is dead — check the obituaries.", name)),
        },
    };
    let body = crate::fmt::format_program(&entry.body);
    let params: Vec<String> = entry.params.iter()
        .map(|p| format!("{}", p.name))
        .collect();
    let health_str = match &entry.health {
        Health::Healthy => "healthy".into(),
        Health::Sick(e) => format!("sick: {}", e),
        Health::Dying => "dying".into(),
    };
    AgentResponse {
        steps: vec![step("obs", format!("{}#{} — {} | fitness {:.2} | {}", name, entry.id, health_str, entry.fitness, entry.origin))],
        reply: format!("{}#{} — {} | {} variant{} in lineage:", name, entry.id, health_str, variants.len(), if variants.len() == 1 { "" } else { "s" }),
        code: Some(format!("fn {}({}) {{\n{}\n}}", name, params.join(", "), body)),
        ran: false,
        output: None,
        success: None,
        events: Vec::new(),
    }
}

fn adopt_intent(words: &[&str], veldt: &mut Veldt) -> AgentResponse {
    let (name, id) = match parse_variant_ref(words) {
        Some((n, Some(i))) => (n, i),
        _ => return plain("Adopt which? Give me a variant — e.g. \"adopt fib#2\"."),
    };
    let mut found = false;
    if let Some(variants) = veldt.functions.get_mut(&name) {
        for v in variants.iter_mut() {
            if v.id == id {
                v.fitness = 1_000_000.0;
                found = true;
            } else {
                v.fitness = v.fitness.min(1_000.0);
            }
        }
    }
    if !found {
        return AgentResponse {
            steps: vec![step("err", format!("{}#{} isn't in the garden.", name, id))],
            reply: format!("No variant named {}#{} lives here.", name, id),
            code: None, ran: false, output: None, success: None, events: Vec::new(),
        };
    }
    AgentResponse {
        steps: vec![
            step("act", format!("Boosting {}#{} fitness to dominance.", name, id)),
            step("ok", "Dispatch will prefer it until it dies or is un-adopted by a fitter birth."),
        ],
        reply: format!("Adopted {}#{} — plain `{}(...)` calls now route to it.", name, id, name),
        code: None, ran: false, output: None, success: None,
        events: vec![format!("[ADOPTED] {}#{}", name, id)],
    }
}

// Heal sick variants of one specific lineage (or variant) — the
// targeted version of "tend".
fn fix_intent(words: &[&str], veldt: &mut Veldt) -> AgentResponse {
    let (name, want_id) = match parse_variant_ref(words) {
        Some((n, i)) => (n, i),
        None => match find_lineage(words, veldt) {
            Some(n) => (n, None),
            None => return plain("Fix what? Name a lineage or variant — e.g. \"fix fib\" or \"fix fib#2\"."),
        },
    };
    let sick: Vec<u32> = veldt.functions.get(&name)
        .map(|vs| vs.iter()
            .filter(|e| matches!(e.health, Health::Sick(_)) && want_id.map(|i| i == e.id).unwrap_or(true))
            .map(|e| e.id).collect())
        .unwrap_or_default();
    if sick.is_empty() {
        return match want_id {
            Some(id) => plain(&format!("{}#{} isn't sick (or doesn't exist).", name, id)),
            None => plain(&format!("No sick variants in the {} lineage — it's healthy.", name)),
        };
    }
    let mut steps = vec![step("plan", format!("Attempting hybrid-merge heal on {} sick variant{} of {}.", sick.len(), if sick.len() == 1 { "" } else { "s" }, name))];
    let mut events = Vec::new();
    let mut healed = 0;
    for id in sick {
        let mut entry = veldt.functions[&name].iter().find(|e| e.id == id).cloned().unwrap();
        if HealthChecker::heal(&mut entry, veldt) {
            healed += 1;
            let msg = format!("[HEALED] {}#{}", name, id);
            steps.push(step("ok", msg.clone()));
            events.push(msg);
            if let Some(vs) = veldt.functions.get_mut(&name) {
                for e in vs.iter_mut() {
                    if e.id == id { *e = entry.clone(); break; }
                }
            }
        } else {
            let msg = format!("{}#{} resisted healing.", name, id);
            steps.push(step("obs", msg.clone()));
            events.push(msg);
        }
    }
    AgentResponse {
        steps,
        reply: format!("Fixed {}/{} sick variant{} of {}.", healed, events.len(), if events.len() == 1 { "" } else { "s" }, name),
        code: None, ran: false, output: None, success: None, events,
    }
}

fn test_intent(words: &[&str], veldt: &mut Veldt) -> AgentResponse {
    // "test all" — every lineage
    if words.contains(&"all") || words.contains(&"everything") {
        let mut names: Vec<String> = veldt.functions.keys().cloned().collect();
        names.sort();
        if names.is_empty() {
            return plain("Nothing to test — the garden is empty.");
        }
        let mut steps = vec![step("plan", format!("Re-trialing all {} lineages.", names.len()))];
        let mut events = Vec::new();
        let mut totals = (0, 0);
        for name in &names {
            for e in veldt.functions[name].clone().iter() {
                let mut entry = e.clone();
                HealthChecker::check_at_birth(&mut entry, veldt);
                let ok = matches!(entry.health, Health::Healthy);
                if ok { totals.0 += 1; }
                totals.1 += 1;
                let msg = format!("{}#{} — {} (trial: {:.2})", name, e.id,
                    match &entry.health { Health::Healthy => "healthy".into(), Health::Sick(m) => format!("sick: {}", m), Health::Dying => "dying".into() },
                    entry.trial_score);
                steps.push(step(if ok { "ok" } else { "err" }, msg.clone()));
                events.push(format!("[TRIAL] {}", msg));
                if let Some(vs) = veldt.functions.get_mut(name) {
                    for v in vs.iter_mut() {
                        if v.id == e.id { *v = entry.clone(); break; }
                    }
                }
            }
        }
        return AgentResponse {
            steps,
            reply: format!("Trials done: {}/{} variants healthy across {} lineages.", totals.0, totals.1, names.len()),
            code: None, ran: false, output: None, success: None, events,
        };
    }
    let name = match find_lineage(words, veldt) {
        Some(n) => n,
        None => {
            let cand = words.iter().find(|w| **w != "test" && **w != "the" && **w != "family" && **w != "trial").map(|s| s.to_string());
            return match cand {
                Some(c) => plain(&format!("No lineage called '{}' lives in the garden.", c)),
                None => plain("Test which family? e.g. \"test fib\" — re-trials every variant."),
            };
        }
    };
    let ids: Vec<u32> = veldt.functions[&name].iter().map(|e| e.id).collect();
    let mut steps = vec![step("plan", format!("Re-trialing {} variants of {}.", ids.len(), name))];
    let mut events = Vec::new();
    let mut healthy = 0;
    for id in ids {
        let mut entry = veldt.functions[&name].iter().find(|e| e.id == id).cloned().unwrap();
        HealthChecker::check_at_birth(&mut entry, veldt);
        let ok = matches!(entry.health, Health::Healthy);
        if ok { healthy += 1; }
        let health_str = match &entry.health {
            Health::Healthy => "healthy".into(),
            Health::Sick(e) => format!("sick: {}", e),
            Health::Dying => "dying".into(),
        };
        let msg = format!("{}#{} — {} (trial: {:.2})", name, id, health_str, entry.trial_score);
        steps.push(step(if ok { "ok" } else { "err" }, msg.clone()));
        events.push(format!("[TRIAL] {}", msg));
        if let Some(vs) = veldt.functions.get_mut(&name) {
            for e in vs.iter_mut() {
                if e.id == id { *e = entry.clone(); break; }
            }
        }
    }
    AgentResponse {
        steps,
        reply: format!("Trials done: {}/{} variants of {} are healthy.", healthy, events.len(), name),
        code: None, ran: false, output: None, success: None, events,
    }
}

/// "why is X sick" / "why does Y fail" — for a LIVE variant, show
/// which probes it fails (expected vs actual output), which niches
/// it owns, and whether it's being outcompeted.
fn why_intent(words: &[&str], veldt: &mut Veldt) -> AgentResponse {
    let Some((name, id_opt)) = words.iter().find_map(|w| {
        let (n, i) = if w.contains('#') {
            let mut it = w.splitn(2, '#');
            (it.next()?.to_string(), it.next().and_then(|s| s.parse::<u32>().ok()))
        } else { ((*w).to_string(), None) };
        veldt.functions.get(n.as_str()).map(|_| (n, i))
    }) else {
        return plain("Why what? \"why score#3\" or \"why is pure sick\" — I'll show its failing probes.");
    };
    let vs = &veldt.functions[&name];
    let e = match id_opt {
        Some(id) => vs.iter().find(|v| v.id == id),
        None => vs.iter().max_by(|a, b| a.fitness.partial_cmp(&b.fitness).unwrap_or(std::cmp::Ordering::Equal)),
    };
    let Some(e) = e else {
        return plain(&format!("No variant {}#{}", name, id_opt.unwrap_or(0)));
    };
    let mut text = format!("{}#{} — fitness {:.2}  |  trust: {:?}  |  health: {:?}\n",
        name, e.id, e.fitness, e.trust, e.health);
    // which probes does it fail?
    let probes = veldt.probes.get(&name).map(|p| p.as_slice()).unwrap_or(&[]);
    let mut interp = crate::interpreter::Interpreter::new();
    interp.caps = crate::interpreter::Caps::sandboxed();
    veldt.load_into_interpreter(&mut interp);
    let mut fails = Vec::new();
    for p in probes {
        let args: Vec<crate::interpreter::Value> = p.args.iter()
            .filter_map(|a| serde_json::from_str(a).ok()).collect();
        if args.len() != p.args.len() { continue; }
        if let Ok(out) = interp.call_function(&name, Some(e.id), args) {
            let got = serde_json::to_string(&out).unwrap_or_default();
            if got != p.expected {
                fails.push(format!("  probe {} — expected {}, got {}", p.niche, p.expected, got));
            }
        } else {
            fails.push(format!("  probe {} — ERROR (crashes on this input)", p.niche));
        }
    }
    if fails.is_empty() {
        text.push_str("passes every probe — no failures on record\n");
    } else {
        text.push_str(&format!("{} failing probe(s):\n{}\n", fails.len(), fails.join("\n")));
    }
    // niche ownership — which signatures it doesn't win
    if !e.niche_scores.is_empty() {
        let weak: Vec<_> = e.niche_scores.iter().filter(|(_, s)| **s < 1.0).collect();
        if !weak.is_empty() {
            text.push_str(&format!("weak niches: {}\n",
                weak.iter().map(|(n, s)| format!("{} {:.0}%", n, **s * 100.0)).collect::<Vec<_>>().join(", ")));
        }
    }
    plain(&text)
}

fn obituary_intent(words: &[&str], veldt: &Veldt) -> AgentResponse {
    // optional lineage filter: any word matching a name that has obituaries
    let filter: Option<String> = words.iter()
        .find(|w| veldt.obituaries.iter().any(|o| &o.name == **w))
        .map(|s| s.to_string());
    let obits: Vec<&crate::veldt::Obituary> = veldt.obituaries.iter().rev()
        .filter(|o| filter.as_ref().map(|f| &o.name == f).unwrap_or(true))
        .take(10)
        .collect();
    if obits.is_empty() {
        return match &filter {
            Some(f) => plain(&format!("Nothing named '{}' has ever died.", f)),
            None => plain("No deaths yet — the garden has only known life."),
        };
    }
    let mut text = String::from("Recent deaths:\n");
    for o in &obits {
        text.push_str(&format!("  {}#{} — died at age {} | {}\n", o.name, o.id, o.age, o.cause_of_death));
    }
    plain(text.trim_end())
}

fn suggest_intent(words: &[&str], veldt: &mut Veldt) -> AgentResponse {
    let surprise = words.contains(&"surprise") || words.contains(&"plant") || words.contains(&"random");
    let unplanted: Vec<&'static str> = catalog().iter()
        .map(|t| t.canonical)
        .filter(|c| !veldt.functions.contains_key(*c))
        .collect();
    if !surprise {
        return if unplanted.is_empty() {
            plain("You've grown every species I know. Try \"improve fib\" for a faster strain, or \"evolve\" to breed rivals.")
        } else {
            plain(&format!("Species not yet in your garden: {}\nSay \"surprise me\" and I'll plant one.", unplanted.join(", ")))
        };
    }
    // deterministic pseudo-random pick seeded by run_count
    let pick = unplanted.get((veldt.run_count as usize) % unplanted.len().max(1));
    match pick {
        Some(&canonical) => {
            let tmpl = catalog().into_iter().find(|t| t.canonical == canonical).unwrap();
            generate_from_template(&tmpl, canonical.to_string(), veldt)
        }
        None => plain("You've grown every species I know. Try \"improve fib\" for a faster strain, or \"evolve\" to breed rivals."),
    }
}

fn improve_intent(t: &str, words: &[&str], veldt: &mut Veldt) -> AgentResponse {
    // Known fast strains: lineage keyword -> (improved body template, planted-into-lineage)
    // The improved organism enters the SAME lineage and competes on merit.
    if t.contains("fib") {
        let name = "fib";
        let code = r#"fn @N@_pair(n: int) {
    if n == 0 {
        return [0, 1]
    }
    let half = @N@_pair(n / 2)
    let a = half[0]
    let b = half[1]
    let c = a * (2 * b - a)
    let d = a * a + b * b
    if n % 2 == 0 {
        return [c, d]
    }
    return [d, c + d]
}

grow fn @N@(n: int) test @N@(0) == 0, @N@(1) == 1, @N@(10) == 55 {
    return @N@_pair(n)[0]
}

print("@N@(30) = " + @N@(30))
"#.replace("@N@", name);
        let note = if veldt.functions.contains_key(name) {
            "This strain is O(log n) — it should out-compete the naive one on trial speed. Run \"evolve\" to watch."
        } else {
            "No fib lineage yet, so this strain plants as the founding variant."
        };
        return grow_and_report_stages(
            vec![StageSpec { label: "fast-doubling fib".into(), code }],
            Some(note.to_string()),
            veldt,
        );
    }
    if t.contains("prime") {
        let name = "is_prime";
        let code = r#"grow fn @N@(n: int) test @N@(2) == true, @N@(7) == true, @N@(4) == false, @N@(97) == true {
    if n < 2 {
        return false
    }
    if n == 2 {
        return true
    }
    if n % 2 == 0 {
        return false
    }
    let i = 3
    while i * i <= n {
        if n % i == 0 {
            return false
        }
        i = i + 2
    }
    return true
}

print("@N@(97) = " + @N@(97))
"#.replace("@N@", name);
        let note = if veldt.functions.contains_key(name) {
            "This strain skips even candidates — half the trial work. Should out-compete the naive one."
        } else {
            "No is_prime lineage yet — this plants as the founding variant."
        };
        return grow_and_report_stages(
            vec![StageSpec { label: "odd-only is_prime".into(), code }],
            Some(note.to_string()), veldt,
        );
    }
    // builtin-stdlib strains — mutations can't invent these calls;
    // planting them gives evolution the faster building block directly.
    for (kw, name, body) in [
        ("sort", "sort", "grow fn @N@(xs: list) test @N@([3,1,2]) == [1,2,3], @N@([]) == [] {\n    return xs.sort()\n}\n\nprint(@N@([9, 1, 5]))"),
        ("reverse", "reverse", "grow fn @N@(xs: list) test @N@([1,2,3]) == [3,2,1], @N@([]) == [] {\n    return xs.reverse()\n}\n\nprint(@N@([1, 2, 3]))"),
        ("join", "join", "grow fn @N@(xs: list, sep: str) test @N@([\"a\",\"b\"], \"-\") == \"a-b\", @N@([], \"-\") == \"\" {\n    return xs.join(sep)\n}\n\nprint(@N@([\"x\", \"y\"], \"+\"))"),
    ] {
        if t.contains(kw) {
            let code = body.replace("@N@", name);
            let note = if veldt.functions.contains_key(name) {
                format!("{} via the builtin — one call, no loop. Should out-compete a manual {} on trial speed.", name, kw)
            } else {
                format!("No {} lineage yet — the builtin strain plants as founder.", name)
            };
            return grow_and_report_stages(
                vec![StageSpec { label: format!("builtin {} strain", kw), code }],
                Some(note), veldt,
            );
        }
    }
    let target = find_lineage(words, veldt);
    match target {
        Some(n) => plain(&format!(
            "I don't know a faster strain for {} — my improvements cover fib (fast-doubling), is_prime (odd-only), and the builtin-stdlib strains (sort, reverse, join). Try \"evolve\" and let mutation try instead.", n)),
        None => plain("Improve what? I know fast strains for fib and is_prime, plus builtin strains for sort/reverse/join — say \"improve sort\". For others, \"evolve\" is the path."),
    }
}

// === Code generation ===

pub struct Template {
    pub keywords: &'static [&'static str],
    pub canonical: &'static str,
    pub desc: &'static str,
    // canonical names of garden lineages this one calls — planted first if absent
    pub needs: &'static [&'static str],
    // Veldt source with @N@ as the function-name placeholder
    pub make: &'static str,
}

pub fn catalog() -> Vec<Template> {
    vec![
        Template {
            keywords: &["fib", "fibonacci"],
            canonical: "fib",
            desc: "fibonacci number",
            needs: &[],
            make: r#"grow fn @N@(n: int) test @N@(0) == 0, @N@(1) == 1, @N@(10) == 55 {
    if n <= 1 {
        return n
    }
    let a = 0
    let b = 1
    let i = 2
    while i <= n {
        let temp = a + b
        a = b
        b = temp
        i = i + 1
    }
    return b
}

print("@N@(10) = " + @N@(10))
"#,
        },
        Template {
            keywords: &["factorial", "fact"],
            canonical: "factorial",
            desc: "factorial",
            needs: &[],
            make: r#"grow fn @N@(n: int) test @N@(0) == 1, @N@(5) == 120 {
    if n <= 1 {
        return 1
    }
    let result = 1
    let i = 2
    while i <= n {
        result = result * i
        i = i + 1
    }
    return result
}

print("@N@(10) = " + @N@(10))
"#,
        },
        Template {
            keywords: &["prime"],
            canonical: "is_prime",
            desc: "primality test",
            needs: &[],
            make: r#"grow fn @N@(n: int) test @N@(2) == true, @N@(7) == true, @N@(4) == false {
    if n < 2 {
        return false
    }
    if n == 2 {
        return true
    }
    let i = 2
    while i * i <= n {
        if n % i == 0 {
            return false
        }
        i = i + 1
    }
    return true
}

print("@N@(97) = " + @N@(97))
print("@N@(100) = " + @N@(100))
"#,
        },
        Template {
            keywords: &["gcd", "greatest common", "common divisor"],
            canonical: "gcd",
            desc: "greatest common divisor",
            needs: &[],
            make: r#"grow fn @N@(a: int, b: int) test @N@(12, 8) == 4, @N@(17, 5) == 1 {
    while b != 0 {
        let temp = b
        b = a % b
        a = temp
    }
    return a
}

print("@N@(48, 36) = " + @N@(48, 36))
"#,
        },
        Template {
            keywords: &["power", "exponent", "pow"],
            canonical: "power",
            desc: "integer power",
            needs: &[],
            make: r#"grow fn @N@(base: int, exp: int) test @N@(2, 10) == 1024, @N@(3, 3) == 27 {
    let result = 1
    let i = 0
    while i < exp {
        result = result * base
        i = i + 1
    }
    return result
}

print("@N@(2, 16) = " + @N@(2, 16))
"#,
        },
        Template {
            keywords: &["sort", "ordered", "ascending"],
            canonical: "bubble_sort",
            desc: "bubble sort",
            needs: &[],
            make: r#"grow fn @N@(nums: list<int>) test @N@([3, 1, 2]) == [1, 2, 3] {
    let result = nums
    let n = len(result)
    let i = 0
    while i < n {
        let j = 0
        while j < n - 1 - i {
            if result[j] > result[j + 1] {
                let temp = result[j]
                result[j] = result[j + 1]
                result[j + 1] = temp
            }
            j = j + 1
        }
        i = i + 1
    }
    return result
}

print("@N@([5, 2, 9, 1, 7]) = " + @N@([5, 2, 9, 1, 7]))
"#,
        },
        Template {
            keywords: &["reverse"],
            canonical: "reverse_str",
            desc: "string reversal",
            needs: &[],
            make: r#"grow fn @N@(s: str) test @N@("hello") == "olleh" {
    let chars = s.chars()
    let result = ""
    let i = len(chars) - 1
    while i >= 0 {
        result = result + chars[i]
        i = i - 1
    }
    return result
}

print(@N@("veldt"))
"#,
        },
        Template {
            keywords: &["palindrome"],
            canonical: "is_palindrome",
            desc: "palindrome check",
            needs: &[],
            make: r#"grow fn @N@(s: str) test @N@("racecar") == true, @N@("hello") == false {
    let chars = s.chars()
    let n = len(chars)
    let i = 0
    while i < n / 2 {
        if chars[i] != chars[n - 1 - i] {
            return false
        }
        i = i + 1
    }
    return true
}

print("@N@(racecar) = " + @N@("racecar"))
"#,
        },
        Template {
            keywords: &["sum", "total", "add up"],
            canonical: "sum",
            desc: "list sum",
            needs: &[],
            make: r#"grow fn @N@(nums: list<int>) test @N@([1, 2, 3]) == 6 {
    let total = 0
    for n in nums {
        total = total + n
    }
    return total
}

print("@N@([10, 20, 30]) = " + @N@([10, 20, 30]))
"#,
        },
        Template {
            keywords: &["mean", "average"],
            canonical: "mean",
            desc: "list average",
            needs: &[],
            make: r#"grow fn @N@(nums: list<int>) test @N@([1, 2, 3, 4, 5]) == 3 {
    let total = 0
    let count = 0
    for n in nums {
        total = total + n
        count = count + 1
    }
    if count == 0 {
        return 0
    }
    return total / count
}

print("@N@([2, 4, 6, 8]) = " + @N@([2, 4, 6, 8]))
"#,
        },
        Template {
            keywords: &["maximum", "largest", "max"],
            canonical: "max_of",
            desc: "list maximum",
            needs: &[],
            make: r#"grow fn @N@(nums: list<int>) test @N@([3, 7, 2, 9, 1]) == 9 {
    let best = nums[0]
    for n in nums {
        if n > best {
            best = n
        }
    }
    return best
}

print("@N@([4, 8, 15, 16, 23, 42]) = " + @N@([4, 8, 15, 16, 23, 42]))
"#,
        },
        Template {
            keywords: &["minimum", "smallest", "min"],
            canonical: "min_of",
            desc: "list minimum",
            needs: &[],
            make: r#"grow fn @N@(nums: list<int>) test @N@([3, 7, 2, 9, 1]) == 1 {
    let best = nums[0]
    for n in nums {
        if n < best {
            best = n
        }
    }
    return best
}

print("@N@([4, 8, 15, 16, 23, 42]) = " + @N@([4, 8, 15, 16, 23, 42]))
"#,
        },
        Template {
            keywords: &["fizzbuzz", "fizz"],
            canonical: "fizzbuzz",
            desc: "fizzbuzz",
            needs: &[],
            make: r#"grow fn @N@(n: int) test @N@(3) == "fizz", @N@(5) == "buzz", @N@(15) == "fizzbuzz", @N@(7) == "7" {
    if n % 15 == 0 {
        return "fizzbuzz"
    }
    if n % 3 == 0 {
        return "fizz"
    }
    if n % 5 == 0 {
        return "buzz"
    }
    return "" + n
}

let i = 1
while i <= 15 {
    print(@N@(i))
    i = i + 1
}
"#,
        },
        Template {
            keywords: &["collatz", "hailstone"],
            canonical: "collatz_steps",
            desc: "collatz step counter",
            needs: &[],
            make: r#"grow fn @N@(n: int) test @N@(6) == 8, @N@(1) == 0 {
    let steps = 0
    let x = n
    while x != 1 {
        if x % 2 == 0 {
            x = x / 2
        } else {
            x = 3 * x + 1
        }
        steps = steps + 1
    }
    return steps
}

print("@N@(27) = " + @N@(27))
"#,
        },
        Template {
            keywords: &["even", "evens", "filter"],
            canonical: "evens",
            desc: "filter even numbers",
            needs: &[],
            make: r#"grow fn @N@(nums: list<int>) test @N@([1, 2, 3, 4]) == [2, 4] {
    let result = []
    for n in nums {
        if n % 2 == 0 {
            result = result.push(n)
        }
    }
    return result
}

print("@N@([1, 2, 3, 4, 5, 6]) = " + @N@([1, 2, 3, 4, 5, 6]))
"#,
        },
        Template {
            keywords: &["double", "map"],
            canonical: "doubles",
            desc: "double every element",
            needs: &[],
            make: r#"grow fn @N@(nums: list<int>) test @N@([1, 2, 3]) == [2, 4, 6] {
    let result = []
    for n in nums {
        result = result.push(n * 2)
    }
    return result
}

print("@N@([1, 2, 3, 4]) = " + @N@([1, 2, 3, 4]))
"#,
        },
        Template {
            keywords: &["vowel"],
            canonical: "count_vowels",
            desc: "vowel counter",
            needs: &[],
            make: r#"grow fn @N@(s: str) test @N@("hello") == 2 {
    let vowels = "aeiou"
    let count = 0
    for c in s.chars() {
        if vowels.contains(c) {
            count = count + 1
        }
    }
    return count
}

print(@N@("the quick brown fox"))
"#,
        },
        Template {
            keywords: &["digit", "digits"],
            canonical: "digit_sum",
            desc: "digit sum",
            needs: &[],
            make: r#"grow fn @N@(n: int) test @N@(123) == 6, @N@(0) == 0 {
    let total = 0
    let x = n
    while x > 0 {
        total = total + x % 10
        x = x / 10
    }
    return total
}

print("@N@(9875) = " + @N@(9875))
"#,
        },
        Template {
            keywords: &["leap", "leapyear", "leap year"],
            canonical: "is_leap_year",
            desc: "leap year check",
            needs: &[],
            make: r#"grow fn @N@(y: int) test @N@(2000) == true, @N@(1900) == false, @N@(2024) == true {
    if y % 400 == 0 {
        return true
    }
    if y % 100 == 0 {
        return false
    }
    return y % 4 == 0
}

print("@N@(2026) = " + @N@(2026))
"#,
        },
        Template {
            keywords: &["clamp", "bound", "limit"],
            canonical: "clamp",
            desc: "clamp to a range",
            needs: &[],
            make: r#"grow fn @N@(x: int, lo: int, hi: int) test @N@(5, 0, 10) == 5, @N@(-3, 0, 10) == 0, @N@(99, 0, 10) == 10 {
    if x < lo {
        return lo
    }
    if x > hi {
        return hi
    }
    return x
}

print("@N@(42, 0, 10) = " + @N@(42, 0, 10))
"#,
        },
        Template {
            keywords: &["word", "words", "count words"],
            canonical: "count_words",
            desc: "word counter",
            needs: &[],
            make: r#"grow fn @N@(text: str) test @N@("the quick brown fox") == 4 {
    let words = text.split(" ")
    return len(words)
}

print(@N@("code is alive"))
"#,
        },
        Template {
            keywords: &["dedupe", "unique", "distinct", "duplicates"],
            canonical: "dedupe",
            desc: "remove duplicates",
            needs: &[],
            make: r#"grow fn @N@(nums: list<int>) test @N@([1, 2, 2, 3]) == [1, 2, 3], @N@([]) == [] {
    let result = []
    for n in nums {
        if not result.contains(n) {
            result = result.push(n)
        }
    }
    return result
}

print(@N@([4, 4, 1, 4, 2, 1]))
"#,
        },
        Template {
            keywords: &["anagram", "scramble"],
            canonical: "is_anagram",
            desc: "anagram check",
            needs: &[],
            make: r#"grow fn @N@(a: str, b: str) test @N@("listen", "silent") == true, @N@("abc", "abd") == false {
    return a.chars().sort() == b.chars().sort()
}

print(@N@("veldt", "ldtev"))
"#,
        },
        Template {
            keywords: &["repeat", "times str", "string times"],
            canonical: "repeat_str",
            desc: "repeat a string n times",
            needs: &[],
            make: r#"grow fn @N@(s: str, n: int) test @N@("ab", 3) == "ababab", @N@("x", 0) == "" {
    let result = ""
    let i = 0
    while i < n {
        result = result + s
        i = i + 1
    }
    return result
}

print(@N@("veldt ", 3))
"#,
        },
        Template {
            keywords: &["sum squares", "squares", "sum_sq"],
            canonical: "sum_sq",
            desc: "sum of squares",
            needs: &[],
            make: r#"grow fn @N@(nums: list<int>) test @N@([1, 2, 3]) == 14, @N@([]) == 0 {
    let total = 0
    for n in nums {
        total = total + n * n
    }
    return total
}

print(@N@([3, 4]))
"#,
        },
        Template {
            keywords: &["collatz seq", "hailstone seq", "collatz sequence"],
            canonical: "collatz_seq",
            desc: "collatz sequence",
            needs: &[],
            make: r#"grow fn @N@(n: int) test @N@(6) == [6, 3, 10, 5, 16, 8, 4, 2, 1] {
    let seq = [n]
    let x = n
    while x != 1 {
        if x % 2 == 0 {
            x = x / 2
        } else {
            x = 3 * x + 1
        }
        seq = seq.push(x)
    }
    return seq
}

print(@N@(27))
"#,
        },
        Template {
            keywords: &["lcm", "least common", "lowest common"],
            canonical: "lcm",
            desc: "least common multiple — depends on gcd",
            needs: &["gcd"],
            make: r#"grow fn @N@(a: int, b: int) test @N@(4, 6) == 12, @N@(7, 3) == 21 {
    if a == 0 or b == 0 {
        return 0
    }
    return a * b / gcd(a, b)
}

print("@N@(12, 18) = " + @N@(12, 18))
"#,
        },
        Template {
            keywords: &["nth prime", "nth_prime", "prime number"],
            canonical: "nth_prime",
            desc: "nth prime — depends on is_prime",
            needs: &["is_prime"],
            make: r#"grow fn @N@(n: int) test @N@(1) == 2, @N@(6) == 13 {
    let found = 0
    let candidate = 2
    while found < n {
        if is_prime(candidate) {
            found = found + 1
        }
        if found == n {
            return candidate
        }
        candidate = candidate + 1
    }
    return candidate
}

print("@N@(10) = " + @N@(10))
"#,
        },
        Template {
            keywords: &["sqrt", "square root", "isqrt"],
            canonical: "isqrt",
            desc: "integer square root",
            needs: &[],
            make: r#"grow fn @N@(n: int) test @N@(16) == 4, @N@(17) == 4, @N@(0) == 0 {
    if n < 2 {
        return n
    }
    let lo = 1
    let hi = n
    while lo < hi {
        let mid = (lo + hi + 1) / 2
        if mid * mid <= n {
            lo = mid
        } else {
            hi = mid - 1
        }
    }
    return lo
}

print("@N@(1000000) = " + @N@(1000000))
"#,
        },
        Template {
            keywords: &["count prime", "prime count", "primes under", "how many prime"],
            canonical: "count_primes",
            desc: "prime counter — depends on is_prime",
            needs: &["is_prime"],
            make: r#"grow fn @N@(limit: int) test @N@(10) == 4, @N@(2) == 0 {
    let count = 0
    let i = 2
    while i < limit {
        if is_prime(i) {
            count = count + 1
        }
        i = i + 1
    }
    return count
}

print("@N@(100) = " + @N@(100))
"#,
        },
        Template {
            keywords: &["parallel", "pmap", "spawn", "threads", "concurrent"],
            canonical: "parallel_primes",
            desc: "pmap demo — runs garden is_prime on a thread per item",
            needs: &["is_prime"],
            make: r#"let inputs = [2, 3, 4, 5, 10, 13, 17, 97, 100, 101]
let results = pmap("is_prime", inputs)
print("inputs:  " + inputs)
print("results: " + results)
"#,
        },
        Template {
            keywords: &["quicksort", "quick sort", "qsort", "hoare"],
            canonical: "quick_sort",
            desc: "quicksort",
            needs: &[],
            make: r#"grow fn @N@(nums: list<int>) test @N@([3, 1, 2]) == [1, 2, 3], @N@([]) == [], @N@([5, 2, 9, 1, 7]) == [1, 2, 5, 7, 9] {
    if len(nums) <= 1 {
        return nums
    }
    let pivot = nums[0]
    let less = []
    let more = []
    let i = 1
    while i < len(nums) {
        if nums[i] < pivot {
            less = less.push(nums[i])
        } else {
            more = more.push(nums[i])
        }
        i = i + 1
    }
    let result = @N@(less)
    result = result.push(pivot)
    let right = @N@(more)
    for x in right {
        result = result.push(x)
    }
    return result
}

print(@N@([5, 2, 9, 1, 7, 3]))
"#,
        },
        Template {
            keywords: &["median"],
            canonical: "median",
            desc: "median of a list",
            needs: &[],
            make: r#"grow fn @N@(xs: list) test @N@([3, 1, 2]) == 2, @N@([4, 1, 3, 2]) == 2 {
    let s = xs.sort()
    let n = s.len()
    if n == 0 { return 0 }
    return s[(n - 1) / 2]
}

print(@N@([7, 3, 1, 9, 5]))
"#,
        },
        Template {
            keywords: &["sorted", "is_sorted", "check sorted"],
            canonical: "is_sorted",
            desc: "sorted-ness check",
            needs: &[],
            make: r#"grow fn @N@(xs: list) test @N@([1, 2, 3]) == true, @N@([3, 1]) == false, @N@([]) == true {
    let i = 1
    while i < xs.len() {
        if xs[i] < xs[i - 1] { return false }
        i = i + 1
    }
    return true
}

print(@N@([1, 2, 2, 5]))
"#,
        },
        Template {
            keywords: &["binary search", "bsearch", "binary_search"],
            canonical: "binary_search",
            desc: "binary search on a sorted list",
            needs: &[],
            make: r#"grow fn @N@(xs: list, target: int) test @N@([1, 3, 5, 7], 5) == 2, @N@([1, 3, 5, 7], 4) == -1 {
    let lo = 0
    let hi = xs.len() - 1
    while lo <= hi {
        let mid = (lo + hi) / 2
        if xs[mid] == target { return mid }
        if xs[mid] < target { lo = mid + 1 } else { hi = mid - 1 }
    }
    return -1
}

print(@N@([1, 3, 5, 7, 9], 7))
"#,
        },
        Template {
            keywords: &["flatten", "concat lists", "join lists"],
            canonical: "flatten",
            desc: "flatten a list of lists",
            needs: &[],
            make: r#"grow fn @N@(xss: list) test @N@([[1, 2], [3], []]) == [1, 2, 3], @N@([[]]) == [] {
    let out = []
    for xs in xss {
        for x in xs {
            out = out.push(x)
        }
    }
    return out
}

print(@N@([[1, 2], [3, 4], [5]]))
"#,
        },
        Template {
            keywords: &["chunk", "chunked", "batches"],
            canonical: "chunk",
            desc: "split a list into fixed-size chunks",
            needs: &[],
            make: r#"grow fn @N@(xs: list, n: int) test @N@([1,2,3,4,5], 2).len() == 3 {
    let out = []
    let cur = []
    for x in xs {
        cur.append(x)
        if cur.len() == n {
            out.append(cur)
            cur = []
        }
    }
    if cur.len() > 0 { out.append(cur) }
    return out
}

print(@N@([1, 2, 3, 4, 5], 2))
"#,
        },
        Template {
            keywords: &["range", "range list", "count up", "upto"],
            canonical: "range_list",
            desc: "list of ints 0..n-1",
            needs: &[],
            make: r#"grow fn @N@(n: int) test @N@(4) == [0, 1, 2, 3], @N@(0) == [] {
    let out = []
    let i = 0
    while i < n {
        out.append(i)
        i = i + 1
    }
    return out
}

print(@N@(6))
"#,
        },
        Template {
            keywords: &["count", "countif", "count by", "count_by"],
            canonical: "count_by",
            desc: "count items matching a predicate",
            needs: &[],
            make: r#"grow fn @N@(xs: list, pred: fn) test @N@([1,2,3,4], fn(x) { return x > 2 }) == 2 {
    let n = 0
    for x in xs {
        if pred(x) { n = n + 1 }
    }
    return n
}

print(@N@([1, 2, 3, 4, 5], fn(x) { return x % 2 == 0 }))
"#,
        },
        Template {
            keywords: &["intersperse", "join with sep", "insert between"],
            canonical: "intersperse",
            desc: "insert a separator between every element",
            needs: &[],
            make: r#"grow fn @N@(xs: list, sep: int) test @N@([1,2,3], 0) == [1,0,2,0,3], @N@([], 0) == [] {
    let out = []
    let i = 0
    for x in xs {
        if i > 0 { out.append(sep) }
        out.append(x)
        i = i + 1
    }
    return out
}

print(@N@([1, 2, 3], 0))
"#,
        },
        Template {
            keywords: &["zip with", "zip_with", "map2", "combine lists"],
            canonical: "zip_with",
            desc: "combine two lists element-wise with a fn",
            needs: &[],
            make: r#"grow fn @N@(a: list, b: list, f: fn) test @N@([1,2],[3,4], fn(x,y) { return x + y }) == [4,6], @N@([],[], fn(x,y) { return 0 }) == [] {
    let out = []
    let i = 0
    let n = a.len()
    if b.len() < n { n = b.len() }
    while i < n {
        out.append(f(a[i], b[i]))
        i = i + 1
    }
    return out
}

print(@N@([1, 2, 3], [10, 20, 30], fn(x,y) { return x * y }))
"#,
        },
        Template {
            keywords: &["scan", "accumulate", "prefix sums", "running"],
            canonical: "scan",
            desc: "running accumulator — prefix results of f",
            needs: &[],
            make: r#"grow fn @N@(xs: list, acc: int, f: fn) test @N@([1,2,3], 0, fn(a,x) { return a + x }) == [0,1,3,6] {
    let out = [acc]
    let cur = acc
    for x in xs {
        cur = f(cur, x)
        out.append(cur)
    }
    return out
}

print(@N@([1, 2, 3, 4], 0, fn(a,x) { return a + x }))
"#,
        },
        Template {
            keywords: &["window", "sliding", "pairs", "adjacent"],
            canonical: "window",
            desc: "sliding window pairs over a list",
            needs: &[],
            make: r#"grow fn @N@(xs: list) test @N@([1,2,3,4]).len() == 3 {
    let out = []
    let i = 1
    while i < xs.len() {
        out.append([xs[i - 1], xs[i]])
        i = i + 1
    }
    return out
}

print(@N@([1, 2, 3, 4]))
"#,
        },
    ]
}

pub struct StageSpec {
    pub label: String,
    pub code: String,
}

fn generate_intent(t: &str, words: &[&str], veldt: &mut Veldt) -> AgentResponse {
    if let Some(resp) = try_templates(t, words, veldt) {
        return resp;
    }
    // generic: "a function called NAME" / "function NAME" / "named NAME"
    if let Some(name) = extract_name(words) {
        let code = format!(r#"# stub grown by the Gardener — write the body, run again to plant a new variant
grow fn {}(x: int) test {}(0) == 0 {{
    return x
}}

print("{}(0) = " + {}(0))
"#, name, name, name, name);
        return grow_and_report_stages(
            vec![StageSpec { label: format!("stub function {}", name), code }],
            None, veldt,
        );
    }
    plain(
        "I'm not sure what to write. Name a species I know — fib, factorial, prime, count primes, nth prime, gcd, lcm, power, sort, quicksort, reverse, palindrome, sum, mean, max, min, fizzbuzz, collatz, evens, doubles, vowels, digits, leapyear, clamp, words, dedupe, anagram, sqrt, parallel — or say \"a function called NAME\" for a stub."
    )
}

/// A keyword matches if it's a substring of the task, OR if its words
/// appear consecutively in the task where each pair shares a prefix of
/// the shorter word (so "counts primes" hits "count prime", "digit"
/// hits "digits"). Shorter words must be >=3 chars to match.
fn keyword_match(kw: &str, t: &str, words: &[&str]) -> bool {
    if t.contains(kw) {
        return true;
    }
    let kw_words: Vec<&str> = kw.split(' ').filter(|w| !w.is_empty()).collect();
    'outer: for start in 0..words.len() {
        if start + kw_words.len() > words.len() {
            break;
        }
        for (i, k) in kw_words.iter().enumerate() {
            let w = words[start + i];
            if k.len().min(w.len()) < 3 || !(k.starts_with(w) || w.starts_with(k)) {
                continue 'outer;
            }
        }
        return true;
    }
    false
}

fn try_templates(t: &str, words: &[&str], veldt: &mut Veldt) -> Option<AgentResponse> {
    // Longest matching keyword wins — "counts primes" should route to
    // count_primes (kw "count prime"), not is_prime (kw "prime").
    let mut best: Option<(Template, usize)> = None;
    for tmpl in catalog() {
        let score = tmpl.keywords.iter()
            .filter(|k| keyword_match(k, t, words))
            .map(|k| k.len())
            .max();
        if let Some(s) = score {
            if best.as_ref().map(|(_, bs)| s > *bs).unwrap_or(true) {
                best = Some((tmpl, s));
            }
        }
    }
    best.map(|(tmpl, _)| {
        let name = extract_name(words).unwrap_or_else(|| tmpl.canonical.to_string());
        generate_from_template(&tmpl, name, veldt)
    })
}

// Resolve a template's dependency chain into an ordered stage list:
// any `needs` lineage absent from the garden gets planted first, then
// the requested organism itself. Recursive — deps can have deps.
fn resolve_stages(tmpl: &Template, name: String, veldt: &Veldt, stages: &mut Vec<StageSpec>, visiting: &mut Vec<String>) -> Result<(), String> {
    for dep in tmpl.needs {
        let dep_present = veldt.functions.get(*dep)
            .map(|v| v.iter().any(|e| matches!(e.health, Health::Healthy | Health::Sick(_))))
            .unwrap_or(false)
            || stages.iter().any(|s| s.label.contains(dep));
        if dep_present { continue; }
        if visiting.contains(&dep.to_string()) {
            return Err(format!("dependency cycle at {}", dep));
        }
        let dep_tmpl = catalog().into_iter().find(|t| t.canonical == *dep)
            .ok_or_else(|| format!("needs {} but I don't know how to write it", dep))?;
        visiting.push(dep.to_string());
        resolve_stages(&dep_tmpl, dep.to_string(), veldt, stages, visiting)?;
        visiting.pop();
    }
    stages.push(StageSpec {
        label: format!("{} ({})", name, tmpl.desc),
        code: tmpl.make.replace("@N@", &name),
    });
    Ok(())
}

fn generate_from_template(tmpl: &Template, name: String, veldt: &mut Veldt) -> AgentResponse {
    let mut stages = Vec::new();
    let mut visiting = Vec::new();
    if let Err(e) = resolve_stages(tmpl, name, veldt, &mut stages, &mut visiting) {
        return AgentResponse {
            steps: vec![step("err", e.clone())],
            reply: format!("Can't write that: {}.", e),
            code: None, ran: false, output: None, success: None, events: Vec::new(),
        };
    }
    grow_and_report_stages(stages, None, veldt)
}

// The agent loop's observe phase: run each generated program through the
// real pipeline in order, narrating what the garden did with each. The
// veldt is saved between stages so later stages (pmap/spawn threads, or
// calls into freshly-planted deps) see the new organisms.
fn grow_and_report_stages(stages: Vec<StageSpec>, note: Option<String>, veldt: &mut Veldt) -> AgentResponse {
    let mut steps = Vec::new();
    let mut events = Vec::new();
    let mut all_code = String::new();
    let mut combined_output = String::new();
    let mut births = 0;
    let multi = stages.len() > 1;

    if multi {
        steps.push(step("plan", format!("{} stages — dependencies first.", stages.len())));
    }
    for (i, stage) in stages.iter().enumerate() {
        if multi {
            steps.push(step("plan", format!("Stage {}/{}: {}", i + 1, stages.len(), stage.label)));
        } else {
            steps.push(step("plan", format!("Writing {}. Health-checked at birth, then planted.", stage.label)));
        }
        steps.push(step("act", "Generated Veldt source (see code below)."));
        steps.push(step("act", "Running it through the interpreter."));
        if !all_code.is_empty() { all_code.push_str("\n"); }
        all_code.push_str(&stage.code);

        let result = execute_source(&stage.code, veldt);
        for ev in &result.events {
            let kind = if ev.contains("sick") || ev.contains("OBITUARY") { "obs" } else { "ok" };
            steps.push(step(kind, ev.clone()));
            if ev.contains("[BIRTH]") { births += 1; }
            events.push(ev.clone());
        }
        if !result.output.trim().is_empty() {
            combined_output.push_str(&result.output);
        }
        if !result.success {
            steps.push(step("err", format!("Stage {} failed — see output.", i + 1)));
            return AgentResponse {
                steps,
                reply: format!("Died at stage {}/{} ({}). The code is below if you want to patch it by hand.", i + 1, stages.len(), stage.label),
                code: Some(all_code),
                ran: true,
                output: Some(combined_output),
                success: Some(false),
                events,
            };
        }
        // persist between stages: spawned threads (spawn/pmap) and calls
        // into freshly-grown deps resolve against the on-disk garden.
        let _ = veldt.save();
    }

    if births > 0 {
        steps.push(step("ok", "It lives. The new variant competes with its lineage from here — run \"evolve\" to breed rivals."));
    }
    let reply = match (births, note) {
        (0, Some(n)) => n,
        (0, None) => "Ran it.".to_string(),
        (1, note) => format!("Planted — it's alive in the garden now.{} The code is below; hit Insert to keep it in your editor.",
            note.map(|n| format!(" {}", n)).unwrap_or_default()),
        (n, note) => format!("Planted {} organisms — all alive.{} The code is below; hit Insert to keep it in your editor.", n,
            note.map(|x| format!(" {}", x)).unwrap_or_default()),
    };
    AgentResponse {
        steps,
        reply,
        code: Some(all_code),
        ran: true,
        output: if combined_output.is_empty() { None } else { Some(combined_output) },
        success: Some(true),
        events,
    }
}

// Pull a user-supplied function name out of phrases like
// "a function called foo", "named foo", "fn foo"
fn extract_name(words: &[&str]) -> Option<String> {
    for (i, w) in words.iter().enumerate() {
        if (*w == "called" || *w == "named" || *w == "fn") && i + 1 < words.len() {
            let cand = words[i + 1];
            if cand.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false) {
                return Some(cand.to_string());
            }
        }
    }
    None
}

// === Benchmarks ===
// Runs every catalog species through the real pipeline on a sandboxed
// garden (temp dir — never touches the user's) and times each stage,
// then times canned intents. `veldt bench` and "benchmark" both hit this.

#[derive(serde::Serialize, serde::Deserialize)]
pub struct BenchRow {
    pub name: String,
    pub ms: u128,
    pub stages: usize,
    pub healthy: bool,
    pub trial: f64,
}

pub fn bench_report(veldt: &Veldt) -> (String, Vec<BenchRow>) {
    // serialize: HOME is process-global and pmap reads it
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let home = std::env::temp_dir().join(format!("veldt-bench-{}", std::process::id()));
    let garden = home.join(".veldt").join("garden");
    let _ = std::fs::remove_dir_all(&garden);
    let prev_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", &home);
    let mut bv = Veldt::with_path(garden);

    let mut rows = Vec::new();
    let mut lines = vec![format!(
        "Gardener benchmark — {} species (sandboxed garden)", catalog().len())];

    for tmpl in catalog() {
        let start = std::time::Instant::now();
        let mut stages = Vec::new();
        let mut visiting = Vec::new();
        let resolved = resolve_stages(&tmpl, tmpl.canonical.to_string(), &bv, &mut stages, &mut visiting).is_ok();
        let mut ok = resolved;
        if resolved {
            for s in &stages {
                let r = execute_source(&s.code, &mut bv);
                let _ = bv.save();
                if !r.success { ok = false; break; }
            }
        }
        let ms = start.elapsed().as_millis();
        let (healthy, trial) = bv.functions.get(tmpl.canonical)
            .and_then(|v| v.last())
            .map(|e| (matches!(e.health, Health::Healthy), e.trial_score))
            .unwrap_or((false, 0.0));
        rows.push(BenchRow { name: tmpl.canonical.to_string(), ms, stages: stages.len(), healthy, trial });
        let tag = if !ok { "FAILED" } else if tmpl.make.contains("grow fn") {
            if healthy { "healthy" } else { "sick" }
        } else { "ran" };
        lines.push(format!("  {:<16} {:>6}ms  {:<7} trial {:.2}  ({} stage{})",
            tmpl.canonical, ms, tag, trial, stages.len(), if stages.len() == 1 { "" } else { "s" }));
    }
    // species summary — healthy/sick/failed counts + total time
    {
        let total_ms: u128 = rows.iter().map(|r| r.ms).sum();
        let healthy_n = rows.iter().filter(|r| r.healthy).count();
        let zero_trial = rows.iter().filter(|r| r.trial <= 0.0).count();
        let times: Vec<f64> = rows.iter().map(|r| r.ms as f64).collect();
        lines.push(format!("species: {}/{} healthy · {} zero-trial · {}ms total  {}",
            healthy_n, rows.len(), zero_trial, total_ms, crate::spark(&times)));
    }

    // Intent latency — canned tasks against the now-populated sandbox
    lines.push(String::new());
    lines.push("Intent latency:".to_string());
    let mut latencies = Vec::new();
    for task in [
        "what's alive?", "show fib", "test fib", "adopt fib#1", "obituaries",
        "suggest", "write a function that counts primes", "improve fib",
        "surprise me", "help",
    ] {
        let start = std::time::Instant::now();
        let _ = respond(task, "", &mut bv);
        let ms = start.elapsed().as_millis();
        latencies.push(ms as f64);
        lines.push(format!("  {:<45} {:>6}ms", format!("\"{}\"", task), ms));
    }
    lines.push(format!("  intent latency dist: {}", crate::spark(&latencies)));

    // garden state at bench end + user garden summary
    let alive: usize = bv.functions.values().map(|v| v.len()).sum();
    lines.push(String::new());
    lines.push(format!("sandbox end: {} variants across {} lineages | your garden: {} lineages",
        alive, bv.functions.len(), veldt.functions.len()));

    match prev_home {
        Some(h) => std::env::set_var("HOME", h),
        None => std::env::remove_var("HOME"),
    }
    (lines.join("\n"), rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Route the on-disk garden (and therefore spawn/pmap's Veldt::new())
    // into a per-test temp dir so tests never touch the real garden.
    // Returns a lock guard: HOME is process-global, so agent tests are
    // serialized — otherwise parallel threads could swap each other's
    // garden mid-pmap.
    fn fresh_veldt() -> (Veldt, std::sync::MutexGuard<'static, ()>) {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let id = N.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let home = std::env::temp_dir()
            .join(format!("veldt-agent-test-{}-{}-{}", std::process::id(), id, nanos));
        let garden = home.join(".veldt").join("garden");
        std::env::set_var("HOME", &home);
        (Veldt::with_path(garden), guard)
    }

    #[test]
    fn every_template_runs_and_births_healthy() {
        for tmpl in catalog() {
            let (mut veldt, _guard) = fresh_veldt();
            let mut stages = Vec::new();
            let mut visiting = Vec::new();
            resolve_stages(&tmpl, tmpl.canonical.to_string(), &veldt, &mut stages, &mut visiting)
                .unwrap_or_else(|e| panic!("{}: resolve failed: {}", tmpl.canonical, e));
            assert_eq!(stages.len(), 1 + tmpl.needs.len(), "{}: wrong stage count", tmpl.canonical);
            let mut last_events = Vec::new();
            for (i, s) in stages.iter().enumerate() {
                let res = execute_source(&s.code, &mut veldt);
                assert!(res.success, "{} stage {} failed: {}", tmpl.canonical, i, res.output);
                last_events = res.events;
                let _ = veldt.save();
            }
            if tmpl.make.contains("grow fn") {
                assert!(last_events.iter().any(|e| e.contains("[BIRTH]") && e.contains("healthy")),
                    "{}: no healthy birth: {:?}", tmpl.canonical, last_events);
            }
        }
    }

    #[test]
    fn deps_resolve_and_plant_first() {
        let (mut veldt, _guard) = fresh_veldt();
        let resp = respond("write a function that counts primes", "", &mut veldt);
        assert_eq!(resp.success, Some(true), "failed: {:?}", resp.output);
        let births = resp.events.iter().filter(|e| e.contains("[BIRTH]")).count();
        assert_eq!(births, 2, "expected is_prime + count_primes: {:?}", resp.events);
        assert!(veldt.functions.contains_key("is_prime"));
        assert!(veldt.functions.contains_key("count_primes"));
    }

    #[test]
    fn parallel_demo_runs_pmap() {
        let (mut veldt, _guard) = fresh_veldt();
        let resp = respond("write something parallel", "", &mut veldt);
        assert_eq!(resp.success, Some(true), "failed: {:?}", resp.output);
        let out = resp.output.unwrap_or_default();
        assert!(out.contains("results:"), "pmap output missing: {}", out);
    }

    #[test]
    fn improve_grows_competing_strain() {
        let (mut veldt, _guard) = fresh_veldt();
        let resp = respond("write a fibonacci function", "", &mut veldt);
        assert_eq!(resp.success, Some(true));
        let resp = respond("improve fib", "", &mut veldt);
        assert_eq!(resp.success, Some(true), "improve failed: {:?}", resp.output);
        assert!(veldt.functions["fib"].len() >= 2, "expected 2+ fib variants");
    }

    #[test]
    fn show_returns_fittest_source() {
        let (mut veldt, _guard) = fresh_veldt();
        respond("write a fibonacci function", "", &mut veldt);
        let resp = respond("show fib", "", &mut veldt);
        let code = resp.code.unwrap_or_default();
        assert!(code.contains("fn fib("), "bad show output: {}", code);
        assert!(code.contains("return"), "missing body: {}", code);
    }

    #[test]
    fn adopt_boosts_variant() {
        let (mut veldt, _guard) = fresh_veldt();
        respond("write a fibonacci function", "", &mut veldt);
        let resp = respond("adopt fib#1", "", &mut veldt);
        assert!(resp.reply.contains("Adopted"));
        assert!(veldt.functions["fib"][0].fitness > 900_000.0);
    }

    #[test]
    fn test_intent_retrials() {
        let (mut veldt, _guard) = fresh_veldt();
        respond("write a fibonacci function", "", &mut veldt);
        let resp = respond("test fib", "", &mut veldt);
        assert!(resp.events.iter().any(|e| e.contains("[TRIAL] fib#1 — healthy")));
    }

    #[test]
    fn bench_smoke() {
        let (mut veldt, _guard) = fresh_veldt();
        let (report, rows) = bench_report(&veldt);
        assert!(report.contains("Gardener benchmark"));
        assert!(report.contains("fib"));
        assert_eq!(rows.len(), catalog().len());
        assert!(rows.iter().all(|r| r.ms < 60_000));
        let resp = respond("run benchmarks", "", &mut veldt);
        assert!(resp.reply.contains("Gardener benchmark"));
    }

    #[test]
    fn stub_and_surprise_and_status() {
        let (mut veldt, _guard) = fresh_veldt();
        let resp = respond("write a function called quux", "", &mut veldt);
        assert_eq!(resp.success, Some(true));
        assert!(veldt.functions.contains_key("quux"));

        let resp = respond("surprise me", "", &mut veldt);
        assert_eq!(resp.success, Some(true));
        let births = resp.events.iter().filter(|e| e.contains("[BIRTH]")).count();
        assert!(births >= 1);

        let resp = respond("what's alive?", "", &mut veldt);
        assert!(resp.reply.contains("Garden census"));
    }

    #[test]
    fn fix_and_test_all() {
        let (mut veldt, _guard) = fresh_veldt();
        respond("write a fibonacci function", "", &mut veldt);
        // fix on a healthy lineage
        let resp = respond("fix fib", "", &mut veldt);
        assert!(resp.reply.contains("isn't sick") || resp.reply.contains("healthy"), "{}", resp.reply);
        // fix on a missing lineage
        let resp = respond("fix nonexistent_thing", "", &mut veldt);
        assert!(resp.reply.contains("Fix what"), "{}", resp.reply);
        // sick the variant, then fix heals or reports resistance
        veldt.functions.get_mut("fib").unwrap()[0].health = Health::Sick("broke".into());
        let resp = respond("fix fib", "", &mut veldt);
        assert!(resp.steps.iter().any(|s| s.text.contains("heal")), "no heal attempt: {:?}", resp.steps);
        // test all — every lineage
        let resp = respond("test all", "", &mut veldt);
        assert!(resp.events.iter().any(|e| e.contains("[TRIAL]")));
    }
}
