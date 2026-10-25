# Veldt perf (Phase 6 complete — persistent structures + borrow-reads)

Measured with `./bench/run.sh` on a debug build.

| workload         | size            | before  | now      | speedup |
|------------------|-----------------|---------|----------|---------|
| list_ops         | 20k push        | 40s     | **371ms**| ~108x (im::Vector) |
| list_ops_fast    | 20k append      | —       | 384ms    | ~100x   |
| dict_ops         | 4k put+get      | 7.2s    | 2.1s     | 3.4x    |
| dict_ops_fast    | 20k insert+get  | 67s     | **218ms**| ~307x   |
| numeric_loop     | 100k iters      | 930ms   | **320ms**| 2.9x    |
| fn_calls         | 30k calls       | ~450ms  | ~450ms   | flat    |
| string_build     | 5k concats      | 69ms    | 23ms     | 3x      |

## What Phase 6 changed

- `Value::List -> im::Vector` + `Value::Dict -> im::HashMap` (gardener):
  O(1) clone via structural sharing — kills both quadratics.
- `Value::Str -> Arc<str>` interned strings + alloc-weighed step
  accounting (gardener).
- `xs.append(v)`, `d.insert(k,v)`, `s.insert(v)`, `xs.pop_in()` — true
  O(1) in-place ops on Var receivers (main side).
- Borrow-read fast paths: `d.get/has/keys/values/len`, `xs.get/len`,
  `s.len`, `xs[i]`, `d["k"]`, `s[i]` — read through `vars.borrow()`
  instead of cloning the container (main side).
- `d["k"]` dict index read + `d["k"] = v` index-assign (both lanes).

## Next wall

fn_calls (~15µs/call): env HashMap alloc + param binding per call.
Options: small-map frames, interned param names, frame pool.
