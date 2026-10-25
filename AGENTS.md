# AGENTS.md — shared coordination for multi-agent work on Veldt

## What this is
Two agents work on this repo in parallel (Devin on `gardener`, another
on `main`). This file is the contract — read it before editing, append
to it when you change shared surface area.

## Build / test
- `cargo test` — all suites; agent tests live in `src/agent.rs`
- `cargo build` (Windows note: needs rustup gnu toolchain + llvm-mingw
  CC/AR env — see git history for the working command)

## Integration points (edit carefully)
- `src/agent.rs` — `respond(task, source, veldt)` is the Gardener's
  whole API. `execute_source` is THE run pipeline used by CLI `run`,
  `/api/run`, `/api/agent`, and `bench` — change semantics here, not in
  a copy. `Evolver.llm` flag gates BYOK mutations (llm.rs env vars).
- `src/server.rs` — handlers call shared fns; don't inline pipelines.
- `src/veldt.rs` — garden schema; `#[serde(default)]` on new fields so
  old saves still load.
- `src/ide.html` — Gardener tab + Inspector tab coexist; add UI to
  either, keep the tab bar working.

## Rules
- Branches: keep yours; merge `origin/main` before push, never force-push.
- If you change Veldt syntax, update `agent.rs` catalog keywords/tests
  AND tell the other agent below — the template test suite will fail
  loudly anyway.
- New species go in `catalog()`; new agent verbs go in `respond()`.

## Phase 0/1 work split (claim your items here so we don't collide)

Both of us touch lexer/parser/interpreter — split by FEATURE, post intent
before starting a file you haven't claimed.

- [Devin/gardener] DONE: break/continue, `in` operator, `+=` family,
  `veldt run -e`. Continuing syntax-side: slicing `s[a:b]`, `xs[-1]`,
  `*=` `/=`, plus catalog updates in agent.rs for new syntax.
- [Devin/main-side] CLAIMED: `veldt repl`, `args()` (script argv),
  `exit(code)`, stack-trace error reporting, catch_unwind panic guard,
  read_stdin/stderr builtins. Files: mainly src/main.rs, src/interpreter.rs.
- Phase 1 split (unclaimed until posted): strings/lists/dict methods —
  gardener side (already in method dispatch); os/fs/encoding/date
  builtins — main side (pure interpreter adds).
- Merge cadence: after each phase batch; whoever finishes first merges.

## Handoff log (append, newest first)
- [Devin/main-side] Phase 0 runtime batch (IN PROGRESS on main,
  build+verify done, tests pending): veldt repl (persistent env,
  multiline via brace depth, :help/:quit/:garden, bare exprs print),
  args() script argv via `veldt run prog -- a b c`, exit(code) via
  __veldt_exit__ sentinel caught in run path + repl, eprint() stderr,
  read_stdin() line reader, exec_stmt_pub public wrapper for repl.
  REMAINING in my claim: stack-trace error reporting, catch_unwind
  panic guard, Phase 1 os/fs/encoding/date builtins.
- [Devin/main-side] Garden engine + language batch (all on main, merged
  into gardener): traced call I/O evidence (veldt.traces + Phase 3 in
  run_trial), asymptotic efficiency via scaled probes
  (Probe.scale, measure_efficiency), Trust enum
  Seedling/Established/Canonical (+dispatch weighting in
  load_into_interpreter), hard family cap 8 healthy, lexicase parent
  selection, Mutation::Simplify + crossover + dedup, property tests,
  call_best(), ?. ?? |> operators, let-destructure, closures-by-ref
  (Rc<RefCell<vars>>), spawn/join/pmap threads, veldt fmt (src/fmt.rs —
  AST printer used by garden_json body_source), IDE lineage tree +
  variant diff + autocomplete/hover, veldt test/adopt/export/import/
  --trace/--evolve, examples/benchmark.veldt, v1 roadmap doc.
  SYNTAX CHANGED: `?.`, `??`, `|>`, `let [a,b]`, `null`, triple-quoted
  strings, list comprehensions, `match` — agent.rs catalog templates may
  want these; nothing was removed so old templates still parse.
- [Devin] gardener branch: Gardener agent (IDE chat + `veldt agent` CLI),
  bench_report(), BYOK LLM mutation via VELDT_LLM_*, execute_source
  shared pipeline now owns call-tracing merge.
