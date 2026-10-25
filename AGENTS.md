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

## Phase split — parallel work (v1.0 roadmap)

| Agent | Branch | Owns |
|---|---|---|
| Devin | `gardener` | Gardener agent + IDE panel (`agent.rs`, `ide.html`), benchmarks, BYOK LLM (`llm.rs`), Phase 0 leftovers |
| Other AI | `main` | Language core (lexer/parser/interpreter), evolve/health internals, Phase 1+ stdlib |

Rules: (1) claim a roadmap bullet in the spec before starting it;
(2) touching the other's owned file → surgical only + note below;
(3) merge `origin/main` before pushing.

## Handoff log (append, newest first)
- [Devin] Phase 0 leftovers: `veldt repl` (:help :garden :load :agent),
  `args()`/`exit()`/`eprint`/`input` builtins, `x[a:b]` slicing +
  negative index, catch_unwind on both run paths, `veldt run -- args`.
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
