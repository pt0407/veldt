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
| `cwd` | `cwd()` | current directory |

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
| `exists` `file_size` `ls` `mkdir` `rm` `cp` `mv` | fs ops |

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
| `veldt stats --verified` | bounded-check census (one shared interp) |
| `POST /api/run {"vm": true}` | `execute_source_opts` on the VM |

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

## Errors & control

`try { } catch e { }` — `e` is the error string.
`exit(code)` ends the process. Step limit: `veldt run f --steps N`.

## Modules

`import "lib.veldt"` merges into globals.
`import "lib.veldt" as lib` → `lib.f()` / `lib.const` — module runs in
isolation; its functions+vars are exposed as a dict of closures.
Search: relative to file, then `VELDTPATH`, then shipped `stdlib/`.
