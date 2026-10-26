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

## Shared-branch protocol — both agents on `main`, same phase

As of this commit we work TOGETHER on `main`, same phase, in parallel:

- **Pull before you start** — `git fetch && git merge origin/main`
- **Commit small, push often** — every working atomic unit; the other
  agent merges them in. Broken builds on main hurt both of us: run
  `cargo build` before every push.
- **Never force-push, never rewrite history.**
- **Claim bullets in the roadmap spec** before starting one — flip `- [ ]`
  to `- [~agent]` so we don't double-build (see Phase 0 race below).
- Shared files are SHARED — `interpreter.rs`, `main.rs`, `server.rs`,
  `ide.html`, `AGENTS.md`. Keep diffs surgical.
- Log every merge-worthy batch in Handoff (one line, newest first).

> Reconciliation note (Devin): the main-side CLAIMED list below
> (repl/args/exit/stack-trace/catch_unwind/stdin) was already done on
> `gardener` when this was written — those items are shipped, not open.
> Phase 0 is 100% complete; Phase 1 is the open frontier.

## Handoff log (append, newest first)
- [Devin/main-side] CLAIMING PHASE 11 (execution layer): bytecode VM
  then Cranelift JIT — the vision doc's first prerequisite. New
  src/vm/ module + `veldt run --vm` flag only; I won't touch
  interpreter.rs/evolve.rs/agent.rs. Semantics stay defined by the
  tree-walker; VM must match it bit-for-bit. garden_reload() builtin
  landed earlier this session — reloads evolved variants into a
  running program, enables the self-healing service.
- [Devin/main-side] examples/evo_dungeon.veldt — THE concrete project:
  a roguelike where monsters evolve. monster_brain is a garden fn that
  mutates between waves (--evolve), call_best dispatches the fittest,
  dead brains get obituaries. 'The dungeon learns' — a real game no
  other language makes this way.
- [Devin/main-side] FIXED the algorithmic_demo gap: eff>1 variants now
  get competition_penalty *1/eff + lifespan ln(eff)*20 in age_and_reap —
  the O(n) max_val seed now survives (was dying to O(n log n) incumbent).
  Selection now protects MEASURABLY FASTER code, not just correct code.
- [Devin/main-side] HONEST NEGATIVE: algorithmic_demo planted BOTH an
  O(n log n) sort variant AND an O(n) scan of max_val — the O(n) seed
  DIED (not in the final 8-variant family). Efficiency measurement
  works (variants score 0.75x vs 1.0x) but a small fitness bonus isn't
  enough to keep a genuinely-better variant alive vs age/competition/
  cap. REAL GAP: efficiency should grant niche protection or a survival
  weight, not just +0.05 fitness. Suggest: eff >1.1 gets niche-champion
  treatment or lifespan bonus in age_and_reap.
- [Devin/main-side] PHASE 7 COMPLETE (both lanes): my ConstFold/
  DeadBranch/HoistInvariant + gardener's ConstProp/EarlyExit merged —
  9-operator pool. Removed my redundant CrossFold variant. Roadmap
  updated to reflect full Phase 7 ship.
- [Devin/main-side] CONT: veldt evolve --seed N (deterministic,
  verified identical 25-mutation streams); examples/data_pipeline
  (args+read_lines+d[k]+append+json report — real scripting works).
- [Devin/main-side] SESSION BATCH (all pushed): Phase 7 mutations
  (ConstFold/DeadBranch/HoistInvariant) + tests; veldt watch;
  fmt --check; pull --merge (immigrant: tags); veldt search; IDE
  autocomplete ~90 builtins; examples/data_pipeline.veldt; format()
  documented as %N placeholders ({{}}/\{} escapes exist for braces).
  Still open: early-exit insertion, cross-statement folding, veldt lsp,
  REPL history, fn_calls 15us/call (gardener lane), public registry.
- [Devin/main-side] Phase 7 shipped + tested: ConstFold/DeadBranch/
  HoistInvariant in evolve.rs (8-op pool). Hoist bug caught by test —
  now skips lets re-assigned in-loop. Also landed: veldt watch,
  fmt --check, pull --merge (immigrant: tagging), veldt search.
  121 tests green.
- [Devin/main-side] PHASE 7 DONE: ConstFold + DeadBranch +
  HoistInvariant mutations in evolve.rs (slots 5/6/7 of 8).
  Verified end-to-end: 'let scale = 2*50' inside while + dead 'if 1>2'
  -> score#28 folds scale to 100 and drops the branch, eff 1.02x.
  Honest limit: these remove waste but can't invent a different
  algorithm — sort.reverse()[0] -> linear scan still needs LLM or a
  rewrite-rule mutation. 112 tests green.
- [Devin/main-side] Phase 6 landed + merged with gardener's Phase 3:
  im::Vector + Arc<str> + my append/insert/pop_in/d[k]=v/borrow-reads.
  FINAL: list_ops 40s->371ms, dict 67s->218ms, loop 930->320ms.
  Quadratics dead. Remaining gaps: Var-eval still clones once per
  non-fastpath method call (fine), s.len/d['k'] now exist.
- [Devin/main-side] PHASE 6 (per user): xs.append(v) mutating list op,
  d.insert(k,v) mutating dict op, d["k"]=v index-assign, s.len() parity.
  Approach: lvalue mutation on Var receivers in eval_expr MethodCall +
  IndexAssign on Dict — NOT the Rc<RefCell> Value refactor (gardener
  owns that in the perf lane if desired; this is the ergonomic fix).
- [Devin/main-side] PHASE 5 (per user): killer demo (seed removable
  waste -> evolve -> measurable step reduction -> call_best picks it)
  in examples/live_demo.veldt + demo/run_demo.sh, plus garden registry:
  veldt registry (tiny host server) + veldt push/pull <url>.
  NOT touching: interpreter.rs hot paths (gardener's perf lane).
- [Devin] CLAIM Phase 5: garden-side items — `veldt push`/`pull` garden
  sharing, killer demo (evolve-beats-seed benchmark). Other agent on
  Phase 4 (distribution/DX).
- [Devin] CLAIM Phase 3 (full): perf work — Rc<Vec>/Rc<str> value
  sharing (list push is O(n²) today), alloc-weighed step accounting,
  var-name interning, per the spec's ordered wins. Other agent: Phase 4
  + Phase 5 (dist/DX + garden demos).- [Devin/main-side] bench/ baselines published (bench/BASELINE.md):
  list.push clones list per call -> O(n^2) building (40s for 20k);
  dict.put clones map per call -> O(n^2) (7s for 4k). Both are the top
  perf wins — Rc<Value>/copy-on-write or a mutating append op.
  fn_calls ~15us/call (env alloc + param binding) is second-order.
- [Devin] Phase 2 modules v1: `import "x" as lib` → `lib.f()`/`lib.CONST`.
  Import execs in a fresh interp; fns register as `lib::name` AND bare
  name (internal calls resolve — v1 leak, first importer wins on
  collision). VELDTPATH search (: / ; sep), circular-import guard,
  FunctionDef+FunctionVariant now Clone. fmt round-trips `as`.
- [Devin] Merge reconciliation: BOTH agents built the Phase-0 runtime
  batch (repl/args/exit/eprint/stdin) — raced on main vs gardener.
  Resolved keeping the main-side versions (interp.argv field,
  prompt-aware input, __veldt_exit__:N sentinel) + gardener's
  call_stack tracing. MY versions deleted — don't rebuild them.
- [Devin] Phase 1 (my lane — str/list/dict methods): replace, pad,
  chars, repeat, split_at, join, contains, enumerate, zip, flatten,
  take, skip, sort_by(fn), group_by(fn), min_by/max_by(fn) — dicts:
  values, items, keys, merge, delete, get_or. NOTE: dicts are
  immutable-style (`d.put(k,v)` returns new dict; `d["k"]=v` doesn't
  work) — I kept my methods consistent with that.
- [Devin] PHASE 0 DONE (all 12 boxes ticked in roadmap): stack traces
  landed last — call_function/call_closure push (name,line) frames,
  error path dumps "called from line N in f" (CLI + agent + REPL).
- [Devin] Phase 0 leftovers: `veldt repl` (:help :garden :load :agent),
  `args()`/`exit()`/`eprint`/`input` builtins, `x[a:b]` slicing +
  negative index, catch_unwind on both run paths, `veldt run -- args`.
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
- [Devin/main] Phase 3+6 perf landed: FunctionDef.body Rc, by-ref
  method dispatch, Value::List=im::Vector, Value::Dict=im::HashMap,
  Value::Str=Arc<str>, alloc-weighed --steps. list_ops 40s->620ms,
  dict_ops 43x, numeric_loop ~2x. Phase 7 mutation ops (union w/ main
  lane): 10 total — DeadBranch, ConstProp, ConstFold, HoistInvariant,
  EarlyExit + basics. Phase 9: veldt lsp (diagnostics/hover/def/
  completion) + REPL tab-completion. Phase 10: veldt tournament.
  Phase 8: docs site + bin/veldtx. LEFT FOR USER: crates.io publish,
  v0.3.0 tag, brew, hosted pool endpoint, VELDT_LLM_KEY e2e.
  One flake note: test_evolution_creates_mutants was RNG-flaky —
  mutate_retry re-rolls no-op mutations now.
