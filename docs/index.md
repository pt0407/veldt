# Veldt

A scripting language where code is alive — functions are born into a
garden, compete on trials, mutate, speciate, and die. The fittest
implementation wins; you call the family, the garden picks the strain.

## Install

```bash
cargo install --path .
veldt repl
```

## In 30 seconds

```veldt
grow fn score(n: int) test score(2) == 4 {
    return n * 2
}
print(call_best("score", 21))   # 42 — the garden chose the fittest
```

```bash
veldt run demo.veldt     # run a program
veldt evolve 10          # 10 mutation cycles — new variants compete
veldt garden             # census: who's alive, who's dying, fitness
veldt test score         # re-trial a whole family
veldt agent "make a fizzbuzz"   # the Gardener writes it for you
```

## The loop

1. **Grow** — `grow fn` plants a variant with tests + probes.
2. **Trial** — birth checks run tests; `veldt test` re-scores a family.
3. **Evolve** — `veldt evolve N` applies mechanical mutations
   (constant folding, dead-branch elimination, loop-invariant
   hoisting, early-exit fusion, operator swaps, crossover) or an
   `--llm` rewrite when `VELDT_LLM_KEY` is set.
4. **Compete** — fitness + niche scores decide; `call_best` dispatches
   to the champion. Losers age out — obituaries record why.

## Ship a garden

```bash
veldt registry           # host a pool on :7878
veldt push <url>         # send your garden
veldt pull <url> --merge # adopt immigrant variants
veldt tournament <url>   # trial merged families, push winners back
```

## Reference

- [Builtin reference](reference.md) — every function, with examples
- `veldt docs <builtin>` — same, in the terminal
- `veldt help <topic>` — quick guides (garden, evolve, agents, …)
- `veldt perf` — microbenchmarks; `bench/` — workload scripts
- `veldt lsp` — language server (hover, goto-def, diagnostics)
- `veldt watch <file>` — re-run on save

## Specs

- [v1 roadmap](specs/2026-10-25-veldt-v1-roadmap.md) — done: modules,
  stdlib, REPL, errors, IDE
- [v1.1+ roadmap](specs/2026-10-26-veldt-v1.1-roadmap.md) — perf,
  algorithmic evolution, distribution, the registry network
