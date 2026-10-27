# Veldt v1.0 Roadmap — General-Purpose Scripting Language

Goal: a new user can `cargo install veldt`, write a ~50-line script for a real
task (file processing, API client, cron automation, small web service), run it,
and never hit a wall. The garden stays the differentiator but must remain
optional — the language has to stand on its own first.

Current state (honest): real programs run (CSV pipeline, HTTP server, dungeon
crawler, SQLite), but missing primitives that every script needs in its first
hour. Tree-walking interpreter; ~60 builtins; no REPL; no argv access; no
break/continue; coarse errors.

## Phase 0 — Scripting essentials (blocking, ~1 week)

Every one of these is hit within the first hour of writing real scripts.

- [x] `veldt repl` — interactive loop: multiline detection (open braces),
      persistent env across lines, `:help`, `:garden`, `:load file`
- [x] `args()` builtin — script argv. `veldt run prog.veldt -- a b c` →
      `args()` returns `["a","b","c"]`
- [x] `veldt run -e "code"` — inline eval, and `veldt -e "code"`
- [x] `break` / `continue` — new Stmt variants + FlowControl::Break/Continue
      threaded through loops and match arms
- [x] `x in list` / `k in dict` / `substr in str` — BinOp::In membership
      (Token::In already exists; add to parse_comparison precedence)
- [x] String/list slicing: `s[a:b]`, `xs[a:b]`, `xs[-1]` negative index.
      Extend Expr::Index eval — parse `expr[lo..hi]` optional range
- [x] `+=`, `-=`, `*=`, `/=` — lexer tokens, desugar to Assign
- [x] `exit(code)` builtin — set process exit status; `veldt run` propagates
- [x] `read_stdin()` / `input()` (added) + eprint? verify; `print(..., flush=false)`
      or `eprint` for stderr output
- [x] Stack traces on runtime errors — interpreter keeps a (name, line) call
      stack; error prints `error at line N in f called from line M in main`
- [x] Shebang support (`#` comments cover `#!`) — `#!` line already lexes as comment; verify end-to-end
- [x] Catch interpreter panics: `catch_unwind` in run path → clean error

## Phase 1 — Stdlib depth (~1-2 weeks)

- [x] Strings (devin): `s[a:b]` slice, `replace` (non-regex), `pad`,
      `chars`, `repeat`, `join` on lists, `split_at` — `format` positional
      still open (~devin)
- [x] Lists (devin): `sort_by(fn)`, `enumerate`, `zip`, `flatten`,
      `group_by(fn)`, `min_by`/`max_by(fn)`, `take(n)`, `skip(n)`, `contains`
- [x] Dicts (devin): `values()`, `items()`, `keys()`, `merge`, `delete`,
      `get_or(k, default)` — immutable-style, returns new dict
- [~devin] `format("{0} {1}", a, b)` positional builtin
- [x] OS/files: `ls(dir)`, `mkdir`, `rm`, `cp`, `mv`, `exists(path)`,
      `cwd()`, `home()`, `read_lines`, `write_lines`, `file_size`
- [x] Encoding: `b64_encode`, `b64_decode`, `url_encode`, `url_decode`,
      `sha256` — all vendored, no new crates
- [x] `re_captures(str, pat)` — capture groups -> list (devin)
- [x] `parse_date(str) -> ts`, `date_add(ts, n[, unit])`, `date_fmt(ts[, fmt])` — real datetime math
- [~devin] `serve` ergonomics: `match_path(pattern, path)` -> params dict
      or null (path params like `/users/:id` work; full `route()` table
      still open)
- [x] HTTP client: `fetch(url[, {headers: dict}])` + `post` — 30s
      timeout, dict headers, returns `{status, body}`

## Phase 2 — Modules & namespacing (~1 week)

- [x] `import "lib.veldt" as lib` → `lib.f()`, `lib.CONST` — module marker
      dict + `alias::name` function registry (devin)
- [x] Search path: VELDTPATH dirs (: / ; sep) — done; importing-file-relative
      + shipped `stdlib/` still open
- [x] `import "lib.veldt"` (no alias) keeps current merge-into-globals
      behavior for backwards compat
- [x] Circular import guard — in-progress path stack, error cleanly
- [x] Ship a `stdlib/` dir with the binary — `include_str!` fallback in the
      import path (devin): `stdlib/str.veldt`, `stdlib/math.veldt`;
      `import "str" as s` / `import "math" as m` resolve embedded.
      `stdlib/http.veldt` still open.

## Phase 3 — Performance (~2 weeks, profile-driven)

Target: within ~10-30x of Python on typical script workloads, not 500x.

- [x] `veldt run --steps N` configurable limit — done (devin)
- [x] Per-instruction cost accounting — eval_expr's BinOp arm charges
      step_count += result.len()/16 (Str), len()/4 (List/Dict) — crude
      but fair; big allocs cost more steps (devin)
- [x] Profile: `veldt perf` (arith/fn-recursion/list-ops microbench) +
      `bench/` dir scripts — done
- [x] Likely wins, ordered by expected cost/benefit — done:
      [x] Value::List -> im::Vector persistent vector: O(1) clone +
            O(log n) push_back — 50k-push loop ~4min -> 0.5s (~500x) (devin)
      [x] Value::Str -> Arc<str> interned: O(1) clone, Send+Sync for
            spawn/pmap (devin)
      [x] FunctionDef.body -> Rc<Vec<Stmt>>: per-call AST clone gone (devin)
      [x] eval_method_call(&Value) by-ref dispatch (devin)
      [x] map/filter/reduce builtins exist as methods — done already
      [~] Var-name interning: HashMap<String,_> lookups remain — marginal
            vs the clone wins; revisit if profiling shows it
      [ ] Statement dispatch: match on &Stmt is fine; avoid re-eval of
            subexpressions
- [ ] NOT in scope: bytecode VM, JIT — months, revisit post-v1

## Phase 4 — Distribution & DX (~1 week)

- [x] `veldt --version` (0.3.0 → 1.0.0 when phases 0-2 land) — done
- [x] `veldt new myapp` — scaffold `main.veldt` + README — done
- [ ] `cargo install veldt-lang` to crates.io (name availability check needed)
- [ ] GitHub release binaries — CI: macOS arm64/x86, linux x64, windows
- [x] README rewrite: 60-second quickstart — install, hello world,
      args example, garden teaser, SMT gate + spec link
- [x] `docs/reference.md` — every builtin, signature, example (all 186
      names covered; `veldt docs <builtin>` table exists)
- [x] `veldt help <topic>` — run/repl/agent/evolve/garden/builtins/strings/lists/types/registry/sandbox/filter/lint topics
- [x] `veldt new myapp` — scaffold `main.veldt` + README
- [x] VSCode syntax highlighting — `editors/vscode/veldt.tmLanguage.json` (LSP also shipped: `veldt lsp`)

## Phase 5 — The garden as the reason to stay (post-v1)

This is what nobody else has — keep it load-bearing but opt-in.

- [x] `veldt import <url>` — pull a garden JSON over HTTP (fetch already
      exists in Rust deps) — gardens travel between machines/people (devin)
- [x] `veldt stats` — ecosystem dashboard: 43 lineages/301 variants
      census, trust ladder, probes/deaths counts, fitness trend sparkline
- [x] Killer demo: `evolve_discovers_and_selects_linear_scan_over_sort_reverse`
      seeds ONLY `xs.sort().reverse()[0]` (O(n log n)); one evolve cycle
      births the O(n) linear scan at eff=20x and selects it — then Z3
      `--exact` proves them equivalent. IdiomRewrite is the mutation.
- [ ] Registry sketch: `veldt push`/`veldt pull` against a hosted garden
      pool — shared gene pool across users (the real dream)

## Definition of done for v1.0

1. `cargo install veldt` works; `veldt hello.veldt` runs on a fresh machine
2. A realistic script (parse CSV → filter → aggregate → write JSON report)
   needs zero workarounds — uses args, stdin/stdout, files, break, in, slicing
3. REPL supports multi-line function defs and persists vars
4. Errors show line + call stack, never panic text
5. Garden: `grow` → `evolve` → `call_best` cycle works and is documented;
   one benchmark shows evolved code measurably beating its seed
6. 102+ tests green; examples/ all run clean

## Explicitly NOT in v1.0

- Static type checking (declared types stay documentation — huge project)
- Bytecode VM / JIT
- Async/await (threads + shared-nothing spawn covers most scripting needs)
- LSP server
- Windows-specific polish (works in principle, untested)
- The Gardener LLM-integration hardening (tracked separately on gardener
  branch)
