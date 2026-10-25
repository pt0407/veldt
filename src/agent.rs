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
use crate::veldt::{Health, Veldt};
use serde::Serialize;

#[derive(Serialize)]
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
    veldt.load_into_interpreter(&mut interp);

    interp.print_buffer = Some(String::new());
    let success = match interp.run_with_lines(&stmts, &parser_lines) {
        Ok(()) => true,
        Err(e) => {
            if let Some(buf) = interp.print_buffer.take() {
                output.push_str(&buf);
            }
            let line = interp.current_line;
            if line > 0 {
                output.push_str(&format!("Runtime error (line {}): {}\n", line, e));
            } else {
                output.push_str(&format!("Runtime error: {}\n", e));
            }
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

    ExecResult { output, events, success }
}

// === Intent routing ===

pub fn respond(task: &str, editor_source: &str, veldt: &mut Veldt) -> AgentResponse {
    let t = task.to_lowercase();
    let words: Vec<&str> = t.split(|c: char| !c.is_alphanumeric() && c != '#' && c != '_')
        .filter(|w| !w.is_empty())
        .collect();

    let has = |pats: &[&str]| pats.iter().any(|p| t.contains(p));

    if has(&["prune", "kill "]) || words.contains(&"kill") {
        return prune_intent(&words, veldt);
    }
    if has(&["clear"]) {
        return plain("That one's on you — the Clear Garden button is up top. I don't do extinctions.");
    }
    if has(&["evolve", "mutate", "generation"]) {
        return evolve_intent(&words, veldt);
    }
    if has(&["sick", "heal", "tend", "fix the garden", "mend"]) {
        return tend_intent(veldt);
    }
    if has(&["garden", "status", "alive", "what's growing", "what is growing", "what have you", "ecosystem"]) {
        return status_intent(veldt);
    }
    if has(&["run", "execute", "try this", "try the"]) && !has(&["write", "make", "create", "grow", "function"]) {
        return run_intent(editor_source, veldt);
    }
    if has(&["write", "make", "create", "grow", "function", "implement", "build", "code"]) {
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
         • \"write a fibonacci function\" — I generate code, run it, plant it\n\
         • \"run this\" — execute what's in the editor\n\
         • \"evolve 5\" — run evolution cycles\n\
         • \"tend the garden\" — heal every sick variant\n\
         • \"prune fib#2\" — kill a specific variant\n\
         • \"what's alive?\" — garden census\n\
         Known species: fib, factorial, prime, gcd, power, sort, reverse, palindrome, sum, mean, max, min, fizzbuzz, collatz, evens, doubles, vowels, digits, leapyear, clamp, words."
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
    AgentResponse {
        steps,
        reply: if result.success {
            "Ran it. Any grows were health-checked and planted.".into()
        } else {
            "It died on the vine — the error is in the output. Fix the code and I'll run it again.".into()
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
    let mut steps = vec![step("plan", format!("Running {} evolution cycle{}.", cycles, if cycles == 1 { "" } else { "s" }))];
    let mut evolver = Evolver::new();
    let events = evolver.evolve(veldt, cycles);
    for ev in &events {
        steps.push(step("obs", ev.format()));
    }
    let reply = if events.is_empty() {
        "Nothing to evolve — the garden needs grown functions first. Ask me to write one.".to_string()
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

// === Code generation ===

struct Template {
    keywords: &'static [&'static str],
    canonical: &'static str,
    desc: &'static str,
    // Veldt source with @N@ as the function-name placeholder
    make: &'static str,
}

fn catalog() -> Vec<Template> {
    vec![
        Template {
            keywords: &["fib", "fibonacci"],
            canonical: "fib",
            desc: "fibonacci number",
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
            make: r#"grow fn @N@(text: str) test @N@("the quick brown fox") == 4 {
    let words = text.split(" ")
    return len(words)
}

print(@N@("code is alive"))
"#,
        },
    ]
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
        return grow_and_report(&name, "a stub function", code, veldt);
    }
    plain(
        "I'm not sure what to write. Name a species I know — fib, factorial, prime, gcd, power, sort, reverse, palindrome, sum, mean, max, min, fizzbuzz, collatz, evens, doubles, vowels, digits, leapyear, clamp, words — or say \"a function called NAME\" for a stub."
    )
}

fn try_templates(t: &str, words: &[&str], veldt: &mut Veldt) -> Option<AgentResponse> {
    for tmpl in catalog() {
        if tmpl.keywords.iter().any(|k| t.contains(k) || words.contains(k)) {
            let name = extract_name(words).unwrap_or_else(|| tmpl.canonical.to_string());
            let code = tmpl.make.replace("@N@", &name);
            return Some(grow_and_report(&name, tmpl.desc, code, veldt));
        }
    }
    None
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

// The agent loop's observe phase: run the generated program through the
// real pipeline and narrate what the garden did with it.
fn grow_and_report(name: &str, desc: &str, code: String, veldt: &mut Veldt) -> AgentResponse {
    let mut steps = vec![
        step("plan", format!("Writing {} — \"{}\". Health-checked at birth, then planted.", name, desc)),
        step("act", "Generated Veldt source (see code below)."),
        step("act", "Running it through the interpreter."),
    ];
    let result = execute_source(&code, veldt);
    for ev in &result.events {
        let kind = if ev.contains("sick") || ev.contains("OBITUARY") { "obs" } else { "ok" };
        steps.push(step(kind, ev.clone()));
    }
    let (reply, success) = if result.success {
        steps.push(step("ok", "It lives. The new variant competes with its lineage from here — run \"evolve\" to breed rivals."));
        (format!("Planted {} — it's alive in the garden now. The code is below; hit Insert to keep it in your editor.", name), true)
    } else {
        steps.push(step("err", "The seed didn't take — see output."));
        (format!("I wrote {}, but it failed on its first run. The code is below if you want to patch it by hand.", name), false)
    };
    AgentResponse {
        steps,
        reply,
        code: Some(code),
        ran: true,
        output: Some(result.output),
        success: Some(success),
        events: result.events,
    }
}
