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

## Handoff log (append, newest first)
- [Devin] gardener branch: Gardener agent (IDE chat + `veldt agent` CLI),
  bench_report(), BYOK LLM mutation via VELDT_LLM_*, execute_source
  shared pipeline now owns call-tracing merge.
