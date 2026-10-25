# Veldt

A scripting language where code lives in an evolving garden.

Functions in Veldt aren't written once and frozen — they're **grown**.
Every `grow fn` seeds a variant into a persistent garden that keeps
evolving across runs: mutants are born and trialled, specialists claim
niches, losers die and get obituaries, and your programs call whoever
the garden currently favors.

```veldt
grow fn double(x: int) test double(2) == 4 {
    return x * 2
}

print(call_best("double", 21))   # 42 — dispatched to the fittest variant
```

## Install

```bash
cargo install --path .          # from this repo
# or once published: cargo install veldt
veldt --version
```

## 60-second quickstart

```bash
veldt repl                      # interactive shell
veldt run hello.veldt           # run a script
veldt new myapp                 # scaffold a project
```

`hello.veldt`:

```veldt
let name = args().get(0) ?? "world"
print("hello " + name)
```

```bash
veldt run hello.veldt -- veldt-user     # hello veldt-user
```

## The language

Feels like a small Python: `let` bindings, `while`/`for`/`if`/`match`,
closures that capture by reference, `?./??` safe access, `|>` pipes,
list comprehensions, destructuring, structs, sets, dicts, try/catch,
string interpolation `"total: {n}"`, regex, JSON, HTTP client+server,
SQLite, file I/O, threads via `spawn`/`join`/`pmap`, namespaced imports
`import "lib.veldt" as lib`.

See `examples/` — `web_server.veldt`, `csv_analysis.veldt`,
`dungeon.veldt` all run today.

## The garden (the point)

```bash
veldt evolve            # one evolution cycle across all lineages
veldt lineage sort      # the family tree: parents, origins, niches
veldt test sort         # re-trial every variant
veldt adopt sort#3      # promote a variant to canonical
veldt studio            # IDE at http://127.0.0.1:7777 — lineage trees,
                        # diffs, obituaries, the Gardener agent
veldt stats             # ecosystem dashboard
```

Every variant is judged on four evidence channels: its inline `test`s,
a co-evolving probe pool (generated inputs that accumulate forever),
real call traces (what users actually got), and scaled efficiency
(step-cost on big inputs vs the incumbent). Selection is lexicase over
per-niche scores — specialists survive.

Variants climb a trust ladder: `seedling` → `established` → `canonical`.
`call_best` prefers trusted variants, so a fresh mutant can't hijack
your call sites.

## The Gardener

`veldt agent` — a rule-based coding agent built into the toolchain:

```bash
veldt agent "write a fibonacci function"   # generates, runs, plants
veldt agent "improve fib"                  # grows a faster strain
veldt agent "what's alive?"                # garden census
```

`veldt evolve --llm` enables LLM-assisted mutation
(`VELDT_LLM_KEY`, `VELDT_LLM_BASE`, `VELDT_LLM_MODEL` — any
OpenAI-compatible endpoint; falls back to mechanical mutation).

## Layout

```
src/         lexer, parser, AST interpreter, garden, evolver, IDE server
stdlib/      shipped modules (str, math)
examples/    runnable programs
bench/       perf workloads + baselines
docs/specs/  design docs + the v1.0 roadmap
tests/       112 tests — cargo test
```

## Status

Pre-1.0. The language is real and the garden works end-to-end; the
interpreter is a tree-walk (fast enough for scripts, not for compute),
and error messages are still growing up. Honest roadmap:
`docs/specs/2026-10-25-veldt-v1-roadmap.md`.

MIT licensed.
