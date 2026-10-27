# Veldt tutorial — from seed to evolved strain

## 1. Plant something

```veldt
grow fn double(n: int) test double(3) == 6, double(0) == 0 {
    return n + n
}
```

Run it: `veldt run demo.veldt` → `[BIRTH] double#1 — healthy`.
The variant is alive: it has tests, a fitness score, a lifespan.

## 2. Let evolution work

```bash
veldt evolve 20
```

Each cycle a parent gets a mutation — constant folding, dead-branch
elimination, loop-invariant hoisting, early-exit fusion, operator
swaps, crossover between healthy parents — and the child is born as
`double#2`, trialed, and scored. With `VELDT_LLM_KEY` set,
`veldt evolve 20 --llm` adds model rewrites into the mix — or point
`VELDT_LLM_CMD` at any local command that reads a prompt on stdin
and writes a `fn` definition on stdout (a script, a local model,
devin itself) for key-free LLM mutations.

## 3. Call the family, not the variant

```veldt
print(call_best("double", 21))   # picks the current champion
```

`call_best` dispatches by fitness — today that's the scan; after
evolution finds a leaner strain, the same call switches to it.

## 4. Keep it clean

```bash
veldt check game.veldt --strict     # ~91 checks on your source
veldt check --garden                # lint every variant in the garden
veldt check --garden --fix          # repair dead code mechanically
veldt stats --lint                  # garden warning census
```

Warnings are advisory until they aren't: evolution rejects mutants
that introduce defect-class warnings (arity mismatches, unbound
reads, unreachable code), `adopt --lint` refuses to promote them,
and `tend` heals sick variants on demand.

## 5. Watch it live in the IDE

`veldt serve` → Studio: editor, Gardener agent ("plant a function that
checks primes", "tend the garden", "evolve with ai"), lineage tree,
variant diffs, garden census. Or `veldt visual` for the garden view.

## 6. Travel

```bash
veldt registry                 # host the pool
veldt push http://box:7878     # your garden joins it
veldt pull http://box:7878 --merge   # immigrants join local families
veldt tournament http://box:7878     # trial everyone, crown winners
```

Tournaments are how strains spread: the pool's champions become the
canonical "world's fittest" implementations — shared probes, one
fitness function, no central authority deciding what's good.
