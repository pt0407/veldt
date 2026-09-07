# Veldt — a language where code is alive

## Overview

Veldt is a programming language where programs grow new code, and that code lives in a shared, persistent ecosystem called the veldt. Grown code has a lifespan determined by how useful it is and whether it works — and when superior code exists for the same job, inferior code's lifespan shrinks until it dies out. The codebase is not a file you edit — it is a living veldt where code competes, evolves, and dies by natural selection.

Implemented in Rust. Interpreter design (lexer → parser → interpreter → ecosystem).

## Core metaphor

- Programs are **seeds** — they run and produce output.
- Code is **alive** — it enters the veldt, lives, can get sick, can heal, ages, and dies.
- The veldt is **shared** — all programs grow into and read from one global ecosystem.
- The codebase **evolves** — the language gets richer as the veldt grows.
- Code **competes** — when superior code exists for the same job, inferior code dies out. Natural selection, not just aging.

## Output channels

Programs have two output channels:

1. **`print`** — outputs data to the user. Ephemeral. Normal program output.
2. **`grow`** — outputs code to the veldt. Permanent (until the code dies). This is how programs extend the language.

Example:
```
fn greet(name) {
    print("hello " + name)
}

greet("world")

grow fn shout(name) {
    print("HELLO " + name.upper())
}
```

After running, `shout` enters the veldt. Any future program can call `shout`. If `shout` has a bug, it gets sick, the runtime attempts healing, and if it cannot be saved, it dies with an obituary.

## The veldt (ecosystem)

The veldt is a shared, persistent store of living code. It is loaded on startup and written to after every run.

### Storage

The veldt persists to disk (a directory containing living code entries, each with metadata). Location: `~/.veldt/garden/` by default.

### What lives in the veldt

Three kinds of entries live in the veldt:

1. **Functions** — the living organisms. They have lifespans, compete, heal, age, and die.
2. **Variables** — immortal data. They persist permanently. No lifespan, no death. Rocks in the ecosystem.
3. **Structs (type definitions)** — immortal like variables. They define user types and persist permanently.

### Function variants (lineages)

A function name is a **lineage**, not a single function. When you grow a function with a name that already exists in the veldt, it becomes a new **variant** of that lineage — like Peter 1, Peter 2, Peter 3. All variants coexist and compete.

- `sort#1`, `sort#2`, `sort#3` are all variants of the `sort` lineage.
- When a program calls `sort(list)`, the **fittest variant** in the lineage runs.
- You can explicitly call a specific variant: `sort#2(list)`.
- Inferior variants lose lifespan and eventually die, pruning the lineage.

### Function entry metadata

Each function variant stores:
- **ID**: variant number within its lineage (e.g., `sort#2`)
- **Name**: lineage name (e.g., `sort`)
- **Source**: the code itself
- **Born**: timestamp when it was grown
- **Age**: how many runs it has survived
- **Health**: `healthy`, `sick`, `dying`
- **Lifespan**: current remaining lifespan (dynamic — see below)
- **Usage count**: how many times it's been called by running programs
- **Last used**: run number when last called
- **Fitness**: current fitness score (health × usage × recency + birth trial score)
- **Trial score**: fitness score from birth trial run
- **Healing attempts**: history of repair attempts

### Variable entry metadata

Each grown variable stores:
- **Name**: variable name
- **Value**: the value (serialized)
- **Born**: timestamp when it was grown

### Struct entry metadata

Each grown struct stores:
- **Name**: struct name
- **Fields**: field names and types
- **Born**: timestamp when it was grown

## Life cycle of code

### Lifespan is dynamic

Lifespan is not a fixed number. It is computed from two factors:

1. **Does it work?** (health)
   - Healthy (parses, type-checks, runs) → strong lifespan baseline
   - Sick (parse OK but type/run fails) → weak lifespan baseline, fast aging
   - Stillborn (parse fails) → dies immediately

2. **Is it useful?** (usage)
   - Called frequently by running programs → lifespan grows
   - Never called → lifespan shrinks toward zero
   - Recently called → lifespan boosted; long dormant → lifespan decays

Each run, lifespan is recalculated:
```
lifespan = base(health) + usage_bonus(recent_calls) - dormancy_penalty(time_since_last_use) - competition_penalty(inferior_to)
```

Healthy, frequently-used code lives long. Healthy, never-used code slowly starves. Sick code dies fast unless it gets healed and used. Inferior variants in a lineage lose lifespan to superior ones.

### Competition (natural selection)

Variants within a lineage compete. When two or more variants share a lineage (same function name), the runtime compares them and ranks them by fitness:

**Fitness** = (health × usage × recency) + trial_score

- A healthy, frequently-called, recently-used function with a good trial score has high fitness.
- A sick or never-called function has low fitness.
- New variants get a **trial run at birth** (see Birth below) — the runtime tests them on sample inputs and measures correctness + speed. This trial score gives new code a starting fitness so it can compete immediately, even with zero usage.

When a superior variant exists:
- The inferior variant's lifespan **shrinks** each run (competition penalty).
- The superior variant's lifespan **grows** (it absorbs the "demand" the inferior one was failing to meet).
- If the inferior variant is also sick, it dies even faster — competition + sickness compounds.

This means: if you grow a better `sort`, the old `sort` variant dies out. If you grow a worse `sort`, *it* dies out and the better one survives. The veldt self-optimizes toward the fittest variant for every lineage. No manual cleanup — competition handles it.

### Birth (and trial run)

When a program executes a `grow fn` statement, the grown function enters the veldt as a new variant. The runtime immediately assesses its health and gives it a trial:

1. **Parse check**: does the code parse?
   - Fails → **stillborn** (dies immediately, obituary printed).
2. **Type check**: does it type-check against the current veldt?
   - Fails → **sick**, enters with a weak lifespan.
3. **Trial run**: the runtime calls the function with sample inputs (generated from the parameter types) and measures:
   - Does it run without errors? (correctness)
   - How fast does it complete? (performance)
   - This produces a **trial score** that becomes the variant's starting fitness.

If all pass → **healthy**, enters with a strong lifespan baseline + trial score as initial fitness.
If parse OK but type/trial fails → **sick**, enters with a weak lifespan and low trial score.

If the new variant joins an existing lineage (same function name), competition begins immediately — the trial score lets it compete against incumbents from the start.

### Birth of variables and structs

`grow let` and `grow struct` entries are immortal — no health check, no trial run, no lifespan. They enter the veldt and persist permanently.

### Life

During each program run, the veldt is loaded. Living code is available to all programs. When a running program calls veldt code, that code's usage count increments and its lifespan grows (usage bonus). Used, healthy code thrives.

### Aging

After each run:
- All veldt entries age by 1.
- Lifespan is recalculated based on current health, usage, and dormancy.
- Competition penalties applied — inferior code in a competitive pair loses lifespan to the superior one.
- Sick code ages faster (sickness multiplier on aging).

### Healing (immune system)

When a function variant is sick, the runtime attempts to heal it before it dies. Healing is **garden-inspired**: the runtime searches the veldt for healthy code with similar signatures (same name pattern, same number of parameters, similar structure) and tries to adapt the sick code.

Healing is **contextual** — it depends on whether the healer serves the same purpose as the sick code:

**Same-purpose healer** (healthy function in the same lineage, or doing the same job):
- Healing = **absorption**. The sick code becomes a copy of the healthy code. It's not just fixed — it's converted. The superior code wins by both starving and absorbing the inferior. Like an immune system that converts invaders into allies.
- The absorbed variant keeps its ID but its source becomes the healthy code's source.

**Different-purpose healer** (healthy function with similar structure but different job):
- Healing = **patching**. The healer can only lend parts — type annotations, structural patterns — without being absorbed. The sick code stays itself, just patched up.
- Strategies, attempted in order:
  1. **Type grafting**: borrow type annotations from the similar healthy function.
  2. **Structure grafting**: replace broken body fragments with the healthy function's structure, adjusting parameter names.

If healing succeeds → code becomes healthy, lifespan restored.
If healing fails → code remains sick, continues aging toward death.

### Death

When code's age exceeds its lifespan, it dies. The runtime prints an **obituary**:

```
[OBITUARY] shout#1(name) — died at age 7 (sick)
  Born: 2026-09-07 14:32:01
  Health: sick (type error: unknown method 'upper' on str)
  Healing attempts: 2 (failed — no similar healthy function found)
  Last used: never
  Cause of death: lifespan exceeded (sickness)
```

Competition-related deaths look like:
```
[OBITUARY] sort#1(list) — died at age 23 (healthy)
  Born: 2026-09-05 09:14:00
  Health: healthy
  Last used: 2 runs ago
  Cause of death: outcompeted by sort#2(list) — higher fitness (more usage, more recent)
```

Dead code is removed from the veldt. The ecosystem self-cleans.

## Language specification

### Types

- `int` — integers
- `str` — strings
- `bool` — booleans
- `list` — lists (homogeneous)
- `fn` — functions
- User-defined structs (see below)

### Structs

```
struct Point {
    x: int,
    y: int,
}

let p = Point { x: 3, y: 4 }
print(p.x)          # field access: 3

grow struct Vec3 {
    x: int,
    y: int,
    z: int,
}
```

Structs can be grown into the veldt. Grown structs are immortal — they persist permanently and don't compete or die.

### Syntax

```
# Comments start with #

# Variable binding
let x = 5
let name = "world"

# Functions
fn greet(name: str) {
    print("hello " + name)
}

# Calling functions
greet("world")

# Conditionals
if x > 3 {
    print("big")
} else {
    print("small")
}

# Loops
for i in range(10) {
    print(i)
}

# While loops
while x > 0 {
    x = x - 1
}

# Structs
struct Point {
    x: int,
    y: int,
}
let p = Point { x: 3, y: 4 }
print(p.x)

# Growing code (the core feature)
grow fn shout(name: str) {
    print("HELLO " + name)
}

grow let default_greeting = "hello"

grow struct Vec3 {
    x: int,
    y: int,
    z: int,
}

# Calling a specific variant (if sort has multiple variants in the veldt)
sort#2(my_list)

# Return
fn add(a: int, b: int) {
    return a + b
}
```

### Operators

- Arithmetic: `+`, `-`, `*`, `/`, `%`
- Comparison: `==`, `!=`, `<`, `>`, `<=`, `>=`
- Boolean: `and`, `or`, `not`
- String concatenation: `+`

### Builtins

- `print(x)` — output to user
- `range(n)` — produce a list [0, 1, ..., n-1]
- `len(x)` — length of string or list
- `str.upper()`, `str.lower()` — string methods
- `list.push(x)` — append to list

## Implementation architecture

### Modules

1. **`lexer.rs`** — tokenize source code into tokens
2. **`parser.rs`** — parse tokens into an AST
3. **`ast.rs`** — AST type definitions
4. **`interpreter.rs`** — walk the AST, execute programs
5. **`veldt.rs`** — the ecosystem: load, save, age, heal, kill code
6. **`health.rs`** — health checking and healing logic
7. **`main.rs`** — CLI entry point, orchestrate lex → parse → load veldt → run → grow → age → save

### Execution flow

```
source file
    → lexer (tokens)
    → parser (AST)
    → load veldt from disk (functions, variables, structs)
    → interpreter runs AST
        → `print` outputs to stdout
        → `grow` collects grown code
        → calls to veldt functions dispatch to fittest variant, increment usage
        → calls to specific variants (name#N) dispatch to that variant
    → after run:
        → new grown functions enter veldt as new variants
            → parse check, type check, trial run → health + trial score
        → new grown variables/structs enter veldt (immortal, no check)
        → sick variants get healing attempts (absorb or patch)
        → all function variants age, lifespan recalculated
        → competition penalties applied within lineages
        → dead variants pruned (obituaries printed)
        → veldt saved to disk
```

### CLI

```
veldt run program.veldt       # run a program
veldt garden                  # list all living code in the veldt
veldt garden --sick           # list only sick variants
veldt garden --obituaries     # show recent deaths
veldt lineage <name>          # show all variants of a function lineage
veldt prune <name#N>          # manually kill a specific variant
veldt clear                   # clear the entire veldt
```

## What we are NOT building today

- The visual IDE (next project — the veldt as a visual ecosystem)
- LLM-based healing (v2 — mechanical/garden-inspired healing first)
- A compiler or bytecode VM (interpreter only)
- Modules or imports (the veldt IS the module system)
- Concurrency

## Success criteria

1. You can write and run a Veldt program with variables, functions, structs, conditionals, and loops.
2. Programs can `grow` functions that persist to the veldt as variants and are available in future runs.
3. Grown functions are health-checked and trial-run at birth (correctness + speed = trial score).
4. Functions exist as lineages with variants — calling by name dispatches to the fittest, calling by `name#N` dispatches to a specific variant.
5. Lifespan is dynamic — based on usefulness (usage), health (does it work), and competition.
6. Variants compete within lineages — superior variants starve inferior ones, which die out.
7. Sick variants get garden-inspired healing (absorbed by same-purpose healers, patched by different-purpose healers).
8. Function variants age and die with obituaries when their lifespan is exceeded (including competition as a cause of death).
9. Grown variables and structs are immortal — they persist permanently.
10. `veldt garden` shows the living ecosystem; `veldt lineage <name>` shows variants.
11. The whole thing feels alive — not like editing files, like tending a living, competing ecosystem.
