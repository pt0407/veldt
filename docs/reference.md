# Veldt builtin reference

All builtins are called as functions: `len(xs)`, `fetch(url)`.
Methods live on values: `xs.push(3)`, `s.split(",")`, `d.put(k, v)`.

## I/O & process

| builtin | signature | notes |
|---|---|---|
| `print` | `print(v)` | stdout, returns null |
| `eprint` | `eprint(v)` | stderr |
| `input` | `input()` / `input(prompt)` | line from stdin |
| `read_stdin` | `read_stdin()` | single line, no newline |
| `args` | `args()` | script argv (`veldt run f -- a b`) |
| `env` | `env("NAME")` | env var or null |
| `exit` | `exit(code)` | clean process exit |
| `exec` | `exec(cmd)` | run shell command, returns output |
| `sleep` | `sleep(ms)` | block |
| `cwd` `home` | `cwd()` / `home()` | current / home directory |

## Types & conversion

| `int` `float` `str` | `int(x)` etc | coercion |
| `len` | `len(list\|str\|dict)` | element count |
| `dict` `set` | `dict()` `set()` | empty container |
| `range` | `range(n)` / `range(a,b)` | int list |
| `max` `min` `abs` | variadic | `max(1,2,3)` or `max(list)` |

## Strings

Methods: `upper lower trim split contains replace starts_with
ends_with len chars find` — plus `format("%0 is %1", a, b)` positional
(`%N` indexed, `%%` literal; `{}` can't be used — `{expr}` interpolation
fires at lex time).

| `b64_encode` `b64_decode` | base64 |
| `url_encode` `url_decode` | percent-encoding |
| `sha256` | hex digest |
| `re_match` `re_find` `re_replace` `re_split` `re_captures` | regex |

## Lists

Methods: `push pop get set len map filter reduce any all sort
reverse join take skip slice find contains index_of unique flat
sum min max zip enumerate group_by`. `push`/`put` return new values.

Mutating (O(1), in-place): `xs.append(v)` `xs.pop_in()` — no copy.

## Dicts & sets

`d.get(k)` `d.put(k,v)` (returns new dict) `d.has(k)` `d.keys()`
`d.values()` `d.delete(k)` `d.merge(d2)` `d["k"]` read + `d["k"]=v`
assign; `d.insert(k,v)` in-place O(1); `s.insert(v)` set insert;
`s.len()`/`s[i]` on strings; sets: `add has remove union intersect
difference to_list`.

## Math

`sqrt pow exp log log10 floor ceil round fabs`
`sin cos tan` `pi` `rand` `rand(a,b)` `floor(int)` etc.

## Files & encoding

| `read_file` `write_file` | whole file |
| `read_lines` `write_lines` | line lists |
| `exists` `file_size` `ls` `mkdir` `rm` `cp` `mv` `read_lines` `write_lines` | fs ops |

## JSON

`json_parse(str)` → value; `json_stringify(v)` → str.

## HTTP

| `fetch` | `fetch(url)` / `fetch(url, dict)` | GET; dict can carry headers/method/body |
| `post` | `post(url, body)` / `post(url, body, dict)` | POST |
| `serve` | `serve(port, fn(req) {...})` | blocking HTTP server; req is dict with `method`, `path`, `body`, `headers` |
| `match_path` | `match_path("/u/:id", "/u/5")` | route params → dict or null |

## Database

| `db_open` | `db_open(path)` → handle | SQLite |
| `db_exec` | `db_exec(h, sql)` | DDL/DML |
| `db_query` | `db_query(h, sql)` → list of dicts | SELECT |
| `db_close` | `db_close(h)` | |

## Date & time

| `time_ms` | epoch milliseconds |
| `date` | `date()` now-ISO; `date("%Y-%m-%d")` formatted |
| `parse_date` | `parse_date("YYYY-MM-DD HH:MM:SS")` → epoch secs |
| `date_add` | `date_add(ts, n[, unit])` — unit: `days`(default)/`hours`/`mins`/`secs`/`weeks` |
| `date_fmt` | `date_fmt(ts[, fmt])` — format an epoch timestamp like `date(fmt)` |

## Concurrency (shared-nothing)

Args/results marshal via JSON; each worker is a fresh interpreter that
loads the garden. **Only garden-persisted functions can be spawned.**

| `spawn` | `spawn("name", [args])` → handle | real OS thread |
| `join` | `join(handle)` → result | errors propagate |
| `pmap` | `pmap("name", list)` → list | thread per item |

## CLI extras

`veldt watch <file>` rerun on save; `veldt fmt <file> --check` CI gate;
`veldt registry [port]` shared garden pool; `veldt push/pull <url>`;
`veldt pull <url> --merge` adds immigrants (provenance-tagged);
`veldt search <url> <name>` query families;
`veldt evolve N --seed S` deterministic evolution;
`veldt repl` `:load <file>` runs a file in the session.

## Execution engine (`--vm`)

Every evaluate path can route through the bytecode VM + Cranelift
tier-2 JIT instead of the tree-walking interpreter. Semantics are
bit-identical (locked by `tests/vm_parity.rs`); the VM is just faster.

| Flag | What runs on the fast engine |
|---|---|
| `veldt run <f> --vm` | whole program (bytecode + JIT) |
| `veldt run <f> --vm --trace` | per-step `[vm:pc] instr` dump |
| `veldt test <fam> --vm` | birth trials via `jit_call` |
| `veldt fuzz <fam> --vm` | boundary corpus through the JIT |
| `veldt evolve N --vm` | mutation birth-checks |
| `veldt repl --vm` | persistent VM session |
| `veldt tournament <url> --vm` | remote-trial scoring |
| `veldt watch <f> --vm` | hot-reload through the fast engine |
| `veldt stats --verified` | bounded-check census (one shared interp) |
| `POST /api/run {"vm": true}` | `execute_source_opts` on the VM |
| `POST /api/agent {"vm": true}` | Gardener code-runs via `prefer_vm` |

`veldt.prefer_vm` (runtime-only, not serialized) — set on the struct
before `execute_source` and every internal code-run routes through the
fast engine. `emit_line` routes VM `PrintOut` through `print_buffer`
so capture works under `--vm` too.

JIT coverage: `garden --jit` reports per-fn signature mode —
`native` (int sigs), `native (float)` (f64 sigs),
`native (boxed)` (boxed-Value ABI — covers the entire instruction
set incl. strings/lists/dicts/structs/methods/try/catch/closures).
Hot fns compile at ≥64 calls; `call_best` + `name#N` dispatch
native too. `VELDT_JIT_STATS=1` prints calls / compiled / cold
per family. Honest perf: numeric code 3-88x (scalar sigs); string/
list/dict ~1.7x (helpers do the work — the win is removing
dispatch overhead, not making ops faster).

## The garden

| `grow` | `grow fn name(...) test ... { }` | plant a seed |
| `call_best` | `call_best("name", args...)` | dispatch to fittest trusted variant |
| `veldt evolve` | CLI | one evolution cycle |
| `veldt test name` | re-trial family |
| `veldt adopt name#N` | canonicalize |
| `veldt lineage name` | family tree |
| `veldt export` `veldt import [url]` | share gardens |
| `veldt stats` | ecosystem dashboard |

## Diagnostics & lint

`veldt check <file> --strict` runs ~91 static checks over the AST —
dead code, arity mismatches, shadowing, unbound reads, impossible
match arms, empty containers that never grow, and more. Warnings are
one line each; `--json` emits machine-readable objects for tooling.

| Command | What it does |
|---|---|
| `veldt check <file> [--strict] [--json]` | parse + strict lint of a source file |
| `veldt check - --strict` | same, reading the program from stdin |
| `veldt check <file> --watch` | re-lint on file save |
| `veldt check --garden [family]` | strict-lint every variant body (synthetic `grow fn` wrap) |
| `veldt check --garden --bugs` | defect-class warnings only — the real-bug subset |
| `veldt check --garden --fix` | mechanically repair dead code in variant bodies, then re-lint |
| `veldt check --garden --fix --verify` | also re-trial every repaired variant, write health back |
| `veldt check --suppress <s>` | drop warnings containing `<s>` (repeatable; works with --json) |
| `veldt fix <file>` | same repairs applied to a source file, in-place + reformatted |
| `veldt fix <file> --check` | dry-run gate — exits 1 if repairable dead code exists |
| `veldt stats --lint` | garden-wide warning census: defect-class split + top warning kinds |
| `veldt evolve --tend` | self-maintaining cycles — heal sick + clean dead code each cycle |
| `veldt adopt <name#N> --lint` | refuse to promote a variant carrying defect-class warnings |
| `veldt tend [family]` | heal sick variants on demand (mechanical repair, then healer-merge) |

The repair passes are conservative rewrites that preserve semantics:
statements after the first `return` in every block are truncated,
`let x = <pure>` never used again is dropped (calls/method calls keep
a dead binding since they may side-effect), and a trailing `continue`
inside a loop body is removed.

Evolution uses the same machinery: every mutation candidate is
normalized (repairs) then lint-gated — a child that introduces
defect-class warnings its parent didn't carry is rejected at birth
with a `[LINT]` event. `veldt check --garden --fix` applies the same
repairs to the whole garden.

`veldt lsp` publishes lint warnings as editor diagnostics (severity 2)
anchored to the enclosing fn — the same checks `check --strict` runs,
live in the editor.

## Game, procgen & simulation library

Canvas (dict-backed 2D raster — `canvas_*` functions take/return dicts):

| `canvas_new(w, h)` | new canvas dict |
| `canvas_set(c, x, y, ch)` / `canvas_get(c, x, y)` | pixel write/read |
| `canvas_line(c, x1, y1, x2, y2, ch)` | Bresenham line |
| `canvas_rect(c, x, y, w, h, ch[, fill])` | rectangle |
| `canvas_circle(c, cx, cy, r, ch)` / `canvas_ellipse(c, cx, cy, rx, ry, ch)` | outlines |
| `canvas_triangle(c, x1,y1, x2,y2, x3,y3, ch)` | three edges |
| `canvas_fill(c, x, y, w, h, ch)` | filled rect |
| `canvas_clear(c[, ch])` | wipe |
| `canvas_render(c)` / `canvas_print(c)` | to string / stdout |
| `sprite(c, x, y, str)` | draw text |

Grids & pathfinding:

| `grid_new(w, h)` / `grid_get(g, x, y)` / `grid_set(g, x, y, v)` | grid dict |
| `grid_neighbors(g, x, y[, diag])` / `neighbors(c, x, y[, diag])` | adjacency |
| `astar(c, sx, sy, tx, ty, wall_ch[, diag])` | shortest path list |
| `astar_cost(c, sx, sy, tx, ty, cost_dict)` | weighted A* |
| `bfs(c, sx, sy, tx, ty, wall_ch)` | breadth-first path |
| `flood_fill(c, x, y, ch)` | region fill |
| `fov(c, x, y, radius, wall_ch)` | field of view set |
| `los(...)` | line of sight |

UI widgets (draw onto a canvas):

| `ui_label(c, x, y, text)` `ui_box(c, x, y, w, h[, title])` | text/frame |
| `ui_bar(c, x, y, w, cur, max[, ch])` `ui_progress(c, x, y, w, cur, max)` | meters |
| `ui_menu(c, x, y, items, sel)` `ui_button(c, x, y, label, hot)` | menus |
| `ui_log(c, x, y, w, h, msgs)` `ui_inventory(c, x, y, items, cols)` | panels |

Randomness & noise (seeded, deterministic per interp):

| `randf()` `rand_bool()` `rand_sign()` `rand_range(lo, hi)` | scalars |
| `rand_gauss(mean, stddev)` `rand_permute(n)` `rand_shuffle(list)` `choice(list)` `weighted_choice(list)` `dice(n, sides)` `roll` | distributions |
| `noise2(x, y)` `perlin(x, y)` `fbm(x, y[, octaves])` `poisson_disc(w, h, r)` | noise fields |
| `voronoi(c, [[x,y,ch],...])` `delaunay([[x,y],...])` `wfc(sample, w, h)` `maze_gen(w, h, wall_ch)` | generators |

Math & curves:

| `lerp(a, b, t)` `clamp(v, lo, hi)` `dist(x1, y1, x2, y2)` `aabb(x1,y1,w1,h1, x2,y2,w2,h2)` `smoothstep(e0, e1, x)` | scalar helpers |
| `bezier(t, x1, y1, x2, y2)` `line_points(x1, y1, x2, y2)` `point_in_poly(x, y, [[px,py],...])` `convex_hull([[x,y],...])` `bounce(t)` `ease_in(t)` `ease_out(t)` `ease_in_out(t)` `collide(a, b)` | curves/easing |

Behavior trees, entities & game loop:

| `bt_tick(tree, ctx)` — nodes `["seq", ...]` `["sel", ...]` `["act", fn]` `["inv", a]`; `succeed`/`fail` | behavior trees |
| `entity(name, x, y, sprite)` `ent_move(e, dx, dy)` `ent_vel(e, vx, vy)` | entities |
| `game_new(w, h)` `game_loop(max_ticks, init, fn)` `game_frame(state, entities, hud)` `game_running()` `camera_xy(state, x, y)` | loop |
| `fsm_next(transitions, state, event)` `turn` | state machines |
| `balance_test(fn, trials)` `balance_compare(f1, f2, trials)` `dist_stats(fn, trials)` `match_order(book, side, price, qty)` `book_new()` `book_add(book, side, price, qty)` `book_mid(book)` `book_spread(book)` | balancing/order book |
| `svc_register(reg, name, fn)` `svc_call(reg, name, args)` `svc_health(reg)` `edge_deploy(cluster, node, fn)` `edge_call(cluster, node, args)` `edge_nodes(cluster)` `edge_health(cluster)` | service/edge sim |
| `robot_new(x, y, dir[, range])` `robot_move(robot, canvas, wall_ch)` `robot_turn(robot, dir)` `robot_sensor(robot, canvas, wall_ch)` `robot_draw(robot, canvas)` | robots |
| `tile_at(map, x, y)` `tile_solid(map, x, y)` `collide_tile(e, map)` `on_ground(e, map)` `physics_tick(e, map, grav)` | tile physics |
| `render(template, dict)` `seq` `garden_reload()` | misc |

## Errors & control

`try { } catch e { }` — `e` is the error string.
`exit(code)` ends the process. Step limit: `veldt run f --steps N`.

## Modules

`import "lib.veldt"` merges into globals.
`import "lib.veldt" as lib` → `lib.f()` / `lib.const` — module runs in
isolation; its functions+vars are exposed as a dict of closures.
Search: relative to file, then `VELDTPATH`, then shipped `stdlib/`.


## Equivalence verification & honesty boundary (`--verify` vs `--exact`)

Veldt provides two verification tiers for checking whether variants in a family compute the same function as the fittest variant before promoting them to `Canonical` trust:

| Command | Tier | What it checks |
|---|---|---|
| `veldt verify <name> [--domain N] [--vm]` | **Bounded check** | Evaluates every healthy variant across `[-N..N]` (default `-10..10`) and reports agreement or divergence |
| `veldt adopt <name#N> --verify` | **Bounded gate** | Promotes `<name#N>` to `Canonical` only if it agrees with the fittest variant on the bounded domain |
| `veldt verify <name> --exact` | **SMT proof (`--features smt`)** | Encodes variant bodies into Z3 formulas (`Int`, `Real`, `Bool`, 4-element symbolic `List`) and solves `f(x) != v(x)`. `Unsat` = provably equivalent; `Sat` = real counterexample (auto-planted as a permanent `smt:cex` probe) |
| `veldt adopt <name#N> --exact` | **SMT gate (`--features smt`)** | Promotes `<name#N>` to `Canonical` only if Z3 proves equivalence (`Unsat`) against the fittest variant |
| `veldt stats --verified` / `veldt stats --exact` | **Garden census** | Reports how many multi-variant families pass bounded verification vs. Z3 SMT verification |

**Honesty boundary:**
- **Bounded verification (`--verify`)** checks a finite grid (`-10..10`), not the whole input space — a variant that diverges only at `x = 1_000_000` will pass bounded verification.
- **SMT verification (`--exact`)** proves mathematical equivalence across all unbounded integers/reals/booleans for straight-line code, `if`/`match`, `try`/`catch` (division-by-zero guards), `for` loops, bounded `while` unrolling (6 iterations with exact loop-exit guards), and 4-element symbolic lists (including `.len()`, `.reverse()`, and sorting-network `.sort()`). It is bounded symbolic model checking for loops/lists (not inductive loop-invariant theorem proving), and bails cleanly to the bounded sweep on string/dict/unbounded-loop functions.
