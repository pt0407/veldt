# Veldt perf (after Phase 6 borrow-reads + mutating ops)

Measured with `./bench/run.sh` on a debug build.

| workload         | size            | before  | now      | speedup |
|------------------|-----------------|---------|----------|---------|
| list_ops         | 20k push        | 40s     | 31s      | 1.3x    |
| list_ops_fast    | 20k append      | —       | **384ms**| ~100x   |
| dict_ops         | 4k put+get      | 7.2s    | 2.1s     | 3.4x    |
| dict_ops_fast    | 20k insert+get  | 67s     | **532ms**| ~126x   |
| numeric_loop     | 100k iters      | 930ms   | 554ms    | 1.7x    |
| fn_calls         | 30k calls       | ~450ms  | ~450ms   | flat    |
| string_build     | 5k concats      | 69ms    | 23ms     | 3x      |

## What Phase 6 changed

- `xs.append(v)`, `d.insert(k,v)`, `s.insert(v)`, `xs.pop_in()` — true
  O(1) in-place ops on Var receivers (no container clone)
- Borrow-read fast paths: `d.get/has/keys/values/len`, `xs.get/len`,
  `s.len`, `xs[i]`, `d["k"]`, `s[i]` read through `vars.borrow()`
  instead of cloning the whole container per call
- NEW: `d["k"]` dict index read + `d["k"] = v` index-assign (were
  missing entirely — only `.get()/.put()` existed)

## Still quadratic (needs Value::List(Rc)/Value::Dict(Rc) — the
## copy-on-write refactor)

- `xs = xs.push(v)` — whole-list clone per call (value semantics)
- `d = d.put(k,v)` — whole-map clone per call
- any method call on a container VAR clones it once (Var eval does
  `.cloned()`) — filter/map/reduce etc pay one clone per pipeline call;
  fast paths now cover the hot read ops (get/has/len/index)
