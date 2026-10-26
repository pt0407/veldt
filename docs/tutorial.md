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
`veldt evolve 20 --llm` adds model rewrites into the mix.

## 3. Call the family, not the variant

```veldt
print(call_best("double", 21))   # picks the current champion
```

`call_best` dispatches by fitness — today that's the scan; after
evolution finds a leaner strain, the same call switches to it.

## 4. Watch it live in the IDE

`veldt serve` → Studio: editor, Gardener agent ("plant a function that
checks primes", "tend the garden", "evolve with ai"), lineage tree,
variant diffs, garden census. Or `veldt visual` for the garden view.

## 5. Travel

```bash
veldt registry                 # host the pool
veldt push http://box:7878     # your garden joins it
veldt pull http://box:7878 --merge   # immigrants join local families
veldt tournament http://box:7878     # trial everyone, crown winners
```

Tournaments are how strains spread: the pool's champions become the
canonical "world's fittest" implementations — shared probes, one
fitness function, no central authority deciding what's good.
