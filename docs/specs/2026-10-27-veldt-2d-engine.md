# Veldt 2D Game Engine — roadmap

The ascii-dungeon layer (canvas_* + ui_* + turn loops) proves the language
can express games. This spec turns it into a real 2D game-dev engine:
**playable real-time games, driven by a human or an evolved policy.**

## Architecture

The interpreter runs server-side; the browser is a live viewport +
input device. The loop is **decoupled**, not per-frame HTTP:

```
veldt program (server thread)                browser (30fps)
  while game_running() {                     ── GET /api/game/frame ──► paints sprites
    k = keys()        ◄── input queue ◄────── POST /api/game/input {keys}
    update state
    game_frame(scene) ──► frame buffer ───► latest scene JSON
    sleep_ms(16)
  }
```

The veldt program free-runs server-side writing frames into a shared
buffer; the browser samples the latest frame and streams input back.
Server speed and client render are independent — real-time on localhost
/LAN, graceful on slower links.

## Phase A — engine builtins (language)

- `game_new(w, h)` → game-state dict (frame buffer, entities, camera)
- `entity(name, x, y, sprite)` → entity dict `{name,x,y,sprite,vx,vy,tag}`
- `ent_move(e, dx, dy)` / `ent_vel(e, vx, vy)` → moved/velocified entity
- `collide(a, b)` → AABB hit (sprite sizes); `collides_with(e, tag)`
- `keys()` → list of currently-pressed keys from the input queue
- `game_frame(state, entities, hud)` → push the frame to the shared buffer
- `game_running()` → bool — session alive (input opens/closes it)
- `sleep_ms(n)` → frame pacing (delta-time basis)
- `camera_xy(x, y)` → viewport offset (for scrolling worlds)

## Phase B — server runtime (game sessions)

- `POST /api/game/start {source, name}` → spawn a game thread running the
  veldt program; returns a session id; garden is shared so grown fns resolve
- `GET /api/game/frame?id=` → latest scene JSON (entities, sprites, hud)
- `POST /api/game/input {id, keys}` → append to the session's input queue
- `POST /api/game/stop {id}` → kill the session
- `GameSession { frame, input_queue, running, veldt }` behind the server mutex

## Phase C — the play viewport (browser)

- `/play/<name>` — real-time page: canvas + key capture + 30fps frame poll
- Sprite renderer — named entities draw as pixel sprites (hero/enemy/coin/
  wall/collectable table), not ascii glyphs; scrolling camera support
- HUD overlay, input legend, game-over / score screen

## Phase D — the veldt game library

- `agent "make a platformer"` / `"make a topdown"` — scaffolds a real-time
  game (player + enemies + collect + collide + win) with a grown
  `game_step`-style policy the Gardener can breed
- Examples: a playable platformer or topdown (WASD/arrows, real collision,
  win/lose) — a human plays it, or an evolved policy plays it better

## Claimed by

Devin — all four phases. See AGENTS.md.
