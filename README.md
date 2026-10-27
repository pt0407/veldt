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
veldt run hello.veldt --vm              # bytecode VM + Cranelift JIT
```

`--vm` runs the same source on a stack-based bytecode VM (covers the
full scripting subset — closures, imports, match, `?.`, slices,
comprehensions, try/catch, `call_best`). Hot functions compile to
native via Cranelift after 64 calls — three signature modes cover
the ENTIRE instruction set: pure-int (`i64` sigs, fib 88x),
pure-float (`f64` sigs), and a boxed-Value ABI for everything else
(strings, lists, dicts, structs, methods incl. write-back, try/catch,
closures — `garden --jit` reports 37/37 = 100%). Direct native calls
between value-mode fns share one arena — no clone dance. Also on
`veldt test/fuzz/evolve/repl/tournament/watch --vm`,
`veldt run --vm --trace` (per-step instr dump), `stats --verified`
(bounded-check census), and `POST /api/run {"vm": true}`.
Honest perf: numeric code 3-88x; string/list/dict ~1.7x (helpers do
the work — the win is removing dispatch, not making ops faster).

## The language

Feels like a small Python: `let` bindings, `while`/`for`/`if`/`match`,
closures that capture by reference, `?./??` safe access, `|>` pipes,
list comprehensions, destructuring, structs, sets, dicts, try/catch,
string interpolation `"total: {n}"`, regex, JSON, HTTP client+server,
SQLite, file I/O, threads via `spawn`/`join`/`pmap`, namespaced imports
`import "lib.veldt" as lib`.

See `examples/` — `evo_dungeon.veldt` (a roguelike where the monsters
literally evolve between waves — run with `--evolve`), `web_server.veldt`,
`csv_analysis.veldt`, `data_pipeline.veldt`, `algorithmic_demo.veldt`,
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

Two equivalence gates guard promotion: `veldt adopt name#N --verify`
(bounded — replays trials/probes) and `--exact` (Z3 SMT — proves the
variant equivalent to the fittest, or extracts a real counterexample
and refuses to promote). `veldt verify <name> --exact` runs the check
standalone; `veldt stats --exact` censuses the whole garden. Honest
scope: ints/bools/floats + fixed-length lists, bounded loop unrolling
— not full theorem proving.

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
tests/       140+ tests — cargo test
```

## Status

Pre-1.0. The language is real and the garden works end-to-end.
Three execution engines ship in one binary: the tree-walker
(default), the bytecode VM, and a Cranelift JIT covering 100% of
the instruction set (37/37 garden fns native — `--vm`/`--jit`
flags throughout). Evolution can already do the headline trick:
from `xs.sort().reverse()[0]` it autonomously derives an O(n)
linear scan, scores it 20x more efficient, selects it — and the
Z3 gate proves the two algorithms equivalent. Honest roadmap:
`docs/specs/2026-10-25-veldt-v1-roadmap.md`.

MIT licensed.
