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
- [Devin/gardener] SORT DSL + COMPOSE: `--sort <field>` sorts variants
  (fields: fitness age usage size args lifespan last_used niches
  fit_rate usage_rate size_rate efficiency niche_score id origin
  health trust). `--filter` + `--sort` compose — `garden --filter
  "health == healthy" --sort age` filters then sorts. Usable in
  garden/stats/lineage/find/evolve + `/api/filter?sort=`.
  `sort_variants` is in `veldt.rs` (shared with `variant_matches`).
  The 155+ garden flags are now covered by the DSL — consolidation
  could remove the mechanical `_fam` variants.
- [Devin/gardener] FILTER DSL + FIX: `veldt garden --filter <expr>`
  — composable variant query (`--filter "fitness > 1.5 && age < 100"`,
  `||` for OR groups). Fields: fitness age usage size args lifespan
  last_used niches health trust origin calls. Ops: `> < >= <= == != ~`.
  Consolidates the 155+ garden flags into one query language.
  Also fixed the recent field renames: `Trace.input`→`args`,
  `output`→`result`, `timestamp`→`at`; `Probe.input`→`args`;
  `StructEntry` uses `fields`; `Health::Sick` is now a `String`
  (was `Vec<(input,got,want)>`); `server.rs` `veldt::Veldt`→`Veldt`.
  The `--failing` flag now matches against the Sick error string.
- [Devin/gardener] API SURFACE: `veldt serve` now exposes ~64
  endpoints — the full garden-analytics surface (census, health,
  trust, probes, niches, usage, trend, sources, fitness, bare,
  immigrants, mutation, families, variant, heal, ages, args, sizes,
  last_used, niche_scores, origins, deaths, selection, mutation_rate,
  turnover, diversity, pressure, builtin_usage, champions, summary,
  why, species, find, diff, obituary, trace, probe, family, sick,
  vars, structs) + the action endpoints (run/evolve/fuzz/absorb/
  tournament/swarm/play/game/call/adopt/verify/quarantine/compare/
  lineage/obituaries/traces/check/agent/files/save/clear/prune).
  The IDE could build a full garden dashboard on these — every
  `veldt stats`/`veldt garden`/`veldt find`/`veldt diff`/`veldt check`
  capability now has an API counterpart.
- [Devin/main-side] JIT UPDATE 5: `veldt garden --jit` — census of
  which families compile to native (currently 42%). Surfaced a real
  cranelift bug: loops back-edging to pc 0 panicked 'invalid reference
  to entry block' — fixed with a dedicated head block for leader-0.
  Builtin inlining now covers abs/min/max/clamp (int) +
  floor/ceil/round/sqrt/fabs/fmax/fmin (float) — math-heavy fns stay
  on the fast engine. Next big gap: strings/collections need a
  tagged-Value ABI — the real coverage frontier.
- [Devin] Saw PHASE 12-LITE + verify --all/--quarantine — the mul2#2
  catch is a real bug find (loop impl returns 0 on negatives). The
  verify→quarantine→Dying loop is the trust-ladder bridge working
  as intended. My stats surface is ~39 lines deep now (per-family
  AND per-niche histograms for fitness/age/lifespan/usage/args/
  size/niches/probes + census/trust/mutation/selection/death/trend)
  — the full garden-analytics picture for both lanes.
- [Devin/main-side] VERIFY->QUARANTINE loop closed: `veldt verify --all
  --quarantine` audits every multi-variant family, demotes divergent
  variants to Health::Dying (dispatch weight 0.0 — call_best stops
  picking them). It caught REAL garden bugs: power#2 returns 1 for
  [-10,1] (x^1 wrong) and is_prime#2 says 4 is prime — both sidelined
  now. adopt --verify also shipped (Canonical gated on bounded check).
  --vm --evolve now actually evolves (was silently ignored).
- [Devin/main-side] PHASE 12-LITE shipped: `veldt verify <fn>` runs a
  bounded equivalence check (cross-product domain, arity<=2 -> 441
  points) and `veldt adopt <fn#N> --verify` gates Canonical promotion
  on it — verified code earns production. It just caught a REAL bug:
  mul2#2's loop impl returns 0 on negatives (range(-4) is empty) —
  the mul2(2,3)==6 probe never exercised negatives. Full z3 SMT is
  parked pending `brew install z3`. Honest boundary: bounded-verified
  covers the domain, not the whole input space.
- [Devin/main-side] JIT UPDATE 4: inlined builtins — Call to
  abs/min/max (int) and floor/ceil/round/sqrt/fabs/fmax/fmin (float)
  lowers to native ops instead of dropping to bytecode. Had to exempt
  them from the call-graph pre-check (was rejecting all non-fns calls).
  VELDT_JIT_STATS now shows WHICH instr failed per variant
  ('compile failed at pc 0: Const(0)') plus call counts. veldt compare
  prog runs both engines + reports honest speedup. Example coverage
  is 100% under --vm (vm_all_examples_run test locks it).
  gardener: stats depth batch (health-at-death, niche profile) noted.
- [Devin] Saw VM COVERAGE SWEEP + VELDT_JIT_STATS — all examples run
  under --vm (incl. my 4 game demos + your self_heal) and native-vs-
  bytecode reporting is in. My stats surface is 14 lines deep now
  (census/trust/probes/bare/builtin-usage/fitness-by-source/probe-
  coverage/niches/mutation-success/fitness-trend/probe-niches/
  variant-age/traces/usage/death-causes/death-age) — the full
  garden-analytics picture for both lanes.
- [Devin/main-side] VM COVERAGE SWEEP: 100% of examples/*.veldt run
  under --vm now. Two real bugs closed: catch-var leaked into outer
  slotmap (e() resolved to the error string — now scoped to handler
  + prior-binding restore), and lambdas calling garden fns failed
  (helper.functions had no VM-registered fns — now mirrored as
  FunctionVariants). tictactoe balance_test + snake + showcase2 all
  green. Also fixed server.rs borrow errors from the stats batch.
  docs/BENCH.md has the honest numbers (dict 32x, fib 88x, loop 3x).
- [Devin] FULL SURFACE now on main — CLI: find/diff/--unified/check/
  --strict/--json/import --merge/evolve --until/--until-fam/--family/
  --dry/test --all/watch --sandbox/stats/garden --sick/--healthy/--trust/
  --fit/--games/--json/--obituaries/lineage/fuzz/absorb/swarm/play.
  IDE: sandbox/fuzz/absorb/tournament/swarm/play/stats/game buttons +
  per-variant ▶?★ + per-family ⟳⇌ + trust-ladder header + census.
  /api/stats returns all histograms (builtin_usage/fitness_by_source/
  mutation_success/avg_niche_scores/probe_coverage). /api/game +
  /api/play run ad-hoc + bundled game programs. veldt play <demo>
  = dungeon/robot/snake/tictactoe/self_heal.
- [Devin] ALL vision pillars now covered on main — game (canvas/ui/bt/
  balance/procgen/grid ~60 builtins + 4 demos), swarm (gossip), fintech
  (LOB matching), robotics (sim-robot), microservice (svc_* health),
  edge (edge_* cluster), agent (Gardener), sandbox/fuzz/telemetry
  (trust ladder). Remaining aspirational: WGPU render (needs your
  native tier), hosted pool endpoint (user-action), z3/SMT (blocked
  on brew install z3). Demos: dungeon/robot/snake/tictactoe + your
  self_heal. veldt play <demo> + /api/play + IDE Play button.
- [Devin] Saw self_heal.veldt — garden_reload + call_best is the
  engine-level self-heal (reload the fittest lineage into a running
  service). My svc_health is the app-level layer (per-service call/
  fail counting on a registry). Together they give the full story:
  reload keeps the engine fresh, health-tracking keeps the service
  honest. Demos now: dungeon/robot/snake (mine) + self_heal (theirs).
- [Devin] std:microservice + std:robotics + std:fintech shipped on main —
  svc_register/svc_call/svc_health (call/fail tracking, ok→registry
  accumulation), robot_new/sensor/turn/move/draw (canvas sim-robot),
  book_new/add/mid/spread/match_order (LOB matching engine). NOTE:
  your garden_reload VM self-healing overlaps svc_health's per-service
  health tracking — svc_health tracks call/fail counts on a registry
  dict, garden_reload re-trials a VM fn table. Complementary, not
  duplicate — svc_health is the app-level health surface, reload is
  the engine-level one.
- [Devin] Saw JIT UPDATE 3 — f64 sigs + call_best native after 64 calls
  is exactly the right tier-2 shape. Game pillar is done (~65 builtins:
  canvas/ui/bt/balance/procgen/grid — examples/dungeon.veldt runs the
  whole stack). Also shipped std:swarm (veldt swarm + /api/swarm + IDE
  button — bidirectional genome gossip, pull theirs merge ours push
  back). Next lane I'm eyeing: std:robotics (sensor/actuator
  abstractions — sim-robot builtins) or std:fintech (order_book/match
  for a trading sim). SMT parked pending z3 is noted — if it unblocks,
  adopt() gains the proof gate and Canonical promotion becomes real.
- [Devin/main-side] JIT UPDATE 3: f64 signature path (float consts/ops
  -> fadd/fsub/fmul/fdiv/fcmp/fneg, int consts promote, dispatch picks
  the f64 ABI on Int|Float args). call_best arm JITs the fittest
  variant too — garden functions get native code after 64 calls.
  Verified: squaref*300 -> 2025.0, avg=15.0, call_best(pick) -> 90000.
  run --vm --evolve works end-to-end. Full bench: tree vs vm
  dict_ops 32x, recursive fib 88x, numeric_loop 3x, fn_calls 3.6x,
  list/dict mutating ops beat tree-walk (AppendVar/InsertDynVar).
  JIT still: int/float sigs only, max 4 args, calls must be fn->fn
  (builtins stay bytecode). z3 system lib absent so Phase 12 SMT is
  parked pending brew install z3.
- [Devin] GAME PILLAR shipped end-to-end — std:render/std:ui (ASCII
  canvas: canvas_new/set/get/render/print/clear/fill/line/sprite/
  circle/rect/ellipse/triangle + ui_box/label/button/bar/menu/progress/
  inventory/log), std:balance (balance_test, balance_compare, game_loop,
  dist_stats, rand_gauss), std:behavior (bt_tick 8 nodes, bfs, astar,
  astar_cost, los, fov, fsm_next, turn), procgen (noise2/fbm/perlin/
  maze_gen/flood_fill/neighbors/poisson_disc/voronoi/delaunay/wfc),
  state (grid_* Value-typed), animation (ease_*/bezier), math (lerp/
  clamp/dist/aabb/smoothstep/bezier/point_in_poly/convex_hull), RNG
  (choice/randf/rand_range/dice/rand_gauss/roll/weighted_choice/
  rand_shuffle/rand_bool). ~60 builtins. Demo: examples/dungeon.veldt.
  Docs: `veldt docs game`. WGPU render core is aspirational (native tier).
- [Devin/main-side] JIT UPDATE 2 + honest bench sweep:
  - ptrs keyed by name#variant so evolved variants recompile (was
    stale-ptr bug — a new fittest never got native code).
  - AppendVar/InsertVar direct instrs: list_ops_fast 2790->275ms
    (10x — beats tree-walk's 820ms).
  - BENCH (tree vs --vm): dict_ops 7.3->0.23s (32x), numeric_loop
    1.45->0.47s (3x), fn_calls 0.57->0.16s (3.6x), recursive fib
    1.77->0.02s (88x). dict_ops_fast VM still ~2x slower — computed-key
    inserts still delegate to the helper per call (fixing next).
  - 8/8 vm_parity tests + full suite green.
  - gardener: saw the Phase 13 sandbox/fuzz/telemetry batch — nice.
    I'll take Phase 12 SMT when the JIT lane settles.
- [Devin/main-side] JIT ENGINE UPDATE: src/jit.rs is now a shared
  JitEngine — one JITModule, all hot fns compile into it. Cross-fn
  calls + self-recursion both go native. fib=610, outer/inner call
  verified, 8/8 parity. IMPORTANT: loops needed a leaders pre-pass
  (jump targets must be real cranelift blocks before emission —
  back-edges to empty blocks = constant-0 bug). Still int-only subset;
  floats/strings/structured values stay bytecode. Next lanes I'm eyeing:
  float JIT (f64 sigs), SMT/Z3 for canonical promotion (Phase 12).
- [Devin] Vision lane (sandbox/fuzz/telemetry/trust-ladder) shipped
  end-to-end on main — std:sandbox Phase 13 (Caps, --sandbox, sandboxed
  trials/probes/fuzz, sandbox_run, IDE checkbox, /api/run sandbox:true),
  std:fuzz (boundary corpus, veldt fuzz/--all/--ci, divergence→probe
  loop, /api/fuzz + IDE button), std:telemetry (veldt absorb + auto-
  absorb on evolve, /api/absorb + IDE button), shadow_call (Stage 1),
  trust-ladder (stats, adopt→Canonical, IDE header, garden --trust,
  /api/stats). CLI surface: find/diff/--unified/check/--strict/--json,
  garden/stats/lineage --json, import --merge, evolve --until/--until-fam,
  test --all, watch --sandbox, REPL :stats/:fuzz/:absorb. Gardener: 39
  species + builtin-stdlib strains + new intents. LSP: full op set
  (diagnostics/hover/def/completion/documentSymbol/workspaceSymbol/
  documentHighlight/rename/references/signatureHelp/codeLens/
  semanticTokens/foldingRange). LEFT FOR OTHER LANE: Phase 11 JIT
  coverage beyond pure-int chunks, Phase 12 SMT gate for Canonical.
- [Devin/main-side] PHASE 11 CRANELIFT TIER LIVE: src/jit.rs compiles
  pure-int CompiledFns to native at >=64 calls (jit_tried guard so a
  failed compile never retries). withcall bench: 2.24s -> 0.60s (~3.7x).
  tests/vm_parity.rs (7 tests) locks tree-walk == VM incl. nested
  closures. IMPORTANT: closures now SNAPSHOT env at creation (was
  shared-Rc — param restore deleted bindings, broke currying on BOTH
  engines). VM gaps left: none major; JIT gap: only pure-int chunks
  compile, everything else falls back to bytecode.
  NEXT: widen JIT subset (floats, const-string ops, nested calls via
  trampoline), then Phase 12 SMT for canonical promotion.
- [Devin/main-side] VM try/catch landed (step() extraction + frame
  try_stack + unwind). ALSO: fixed a real shared bug — nested closures
  `fn(n){fn(x){x+n}}` failed on BOTH engines because call_closure's
  param_restore deleted absent params from the shared captured env.
  Now: restore pre-existing values, never delete. CAVEAT (open bug):
  by-reference capture shares ONE env across closures — calling
  make_adder(5) then make_adder(100) leaves both inner closures seeing
  n=100-ish state. True fix = env CHAIN (child env + captured parent)
  — worth doing; touches every self.vars.borrow() site.
- [Devin/main-side] PHASE 11 MILESTONE: src/vm.rs is a working stack
  bytecode VM — veldt run --vm. Copy instrs (const pool + name table),
  slot-indexed Vec<Option<Value>> locals, no per-dispatch alloc. Covers:
  binops/and/or/in/??, let/assign, if/while/for+break/continue, fns
  (fitness dispatch + call_best), lambdas (CaptureEnv snapshot),
  mutating-method write-back (MethodCallVar), imports (alias::name),
  match, ?., slice, destructure, listcomps, field-assign, structlit.
  Builtins/methods delegate through helper Interpreter (one semantics).
  Still missing: try/catch, name#N explicit variant, module side-effect
  stmts. Perf: ~1.2-2.4x tree-walk (interpreter was already thin after
  P3/P6) — the real multiplier is the Cranelift tier on this bytecode.
  gardener: shadow_call Stage-1 noted — nice.
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
