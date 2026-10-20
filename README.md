# Sprite Factory

One binary, `sprite`, that turns shape specs into consistent 32×32 pixel-art sprites, and
serves the whole pipeline to models over MCP. The house style (palette, light, outline,
canvas) lives in the renderer, so every author — human or model — produces the same set.

```
crates/
  sprite-core   renderer: spec → lit, palette-locked, outlined frames. serde only, no I/O, WASM-safe
  sprite        CLI + MCP server (`sprite mcp`)
  sprite-wasm   browser bindings (wasm-bindgen)
testdata/       the 100 sample sprites + reference renders from the original Python renderer
AUTHORING.md    the shape DSL / style guide (also served by the `style_guide` tool)
deploy/         systemd unit, nginx config, install script, DEPLOY.md
prototype/      the original Python renderer and TypeScript MCP server (see below)
```

## Build

```bash
cargo build --release          # → target/release/sprite (one self-contained binary, ~5.5 MB, needs only libc)
cargo test --release --workspace
```

## CLI

```bash
sprite render  testdata/nouns/*.json --out out/          # PNG + strip + GIF per sprite
sprite hd      testdata/nouns/*.json --size 512 --out hd/ # the same specs redrawn in high definition (64–1024px)
sprite sheet   testdata/nouns/food.json --out food.png   # labelled contact sheet (--numbered for blind review)
sprite strips  testdata/nouns/food.json --out anim.png   # every animation frame
sprite atlas   library/ --out atlas --scale 2            # atlas.png + atlas.json (Phaser/Pixi JSON hash)
sprite palette --out palette.png                         # or no --out for JSON
sprite golden  testdata/golden testdata/nouns/*.json     # parity report vs reference renders
sprite bench   testdata/nouns/*.json
```

Inputs can be batch files (JSON arrays), single-spec files, or folders of either.

**High definition.** Specs are vector shapes in 32px units, so `sprite hd` redraws them on a larger
canvas instead of scaling pixels up: smooth curves, the same palette, light and cel-shading bands,
proportionally thinner outlines. Shortcuts that only work at 32px (single-pixel teeth, stacked eye
pixels) show up at HD, so prefer real shapes for features that matter.

## MCP server

Local (stdio):
```bash
claude mcp add sprite-factory -- /path/to/sprite mcp --library /path/to/library
```

Hosted (private, token-only): `sprite serve` puts MCP at `/mcp` (Streamable HTTP) and a
read-only REST API at `/v1` for game prototypes, both on the same library. See
[`deploy/DEPLOY.md`](deploy/DEPLOY.md) for the systemd + nginx setup.
```bash
SPRITE_TOKENS="me:$(openssl rand -hex 32)" sprite serve --library library --bind 127.0.0.1:8787
claude mcp add --transport http sprite-factory https://sprites.example.com/mcp --header "Authorization: Bearer $TOKEN"
```
```json
{ "mcpServers": { "sprite-factory": { "command": "/path/to/sprite", "args": ["mcp", "--library", "/path/to/library"] } } }
```

Tools: `style_guide`, `list_sprites`, `get_sprite`, `create_sprite`, `edit_sprite`, `render_sprite`,
`delete_sprite`, `blind_review`, `export_atlas`. Every create/edit returns the rendered sprite,
its animation frames and measured style warnings, so the model sees its work and revises.

`blind_review` shows a fresh model an unlabeled, shuffled sheet and grades its guesses against
each sprite's name and `aka` synonyms. It uses, in order:
1. **`ANTHROPIC_API_KEY`** — calls the Messages API directly (`SPRITE_REVIEW_MODEL`, default
   `claude-sonnet-5-5`; `ANTHROPIC_BASE_URL` to override the host). Works with any client.
2. **MCP sampling** — as an MRTR input request on protocol 2026-07-28+, or a classic
   server→client request on older protocols. Sampling is deprecated upstream (SEP-2577), so
   prefer option 1 for anything long-lived.
3. **Fallback** — returns the unlabeled sheet and answer key for a separate reviewer.

## WASM

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-pack
wasm-pack build crates/sprite-wasm --target web --release
```
```js
import init, { render, palette } from "./pkg/sprite_wasm.js";
await init();
const out = render(JSON.stringify(spec)); // out.frames: Uint8Array of out.frameCount × 32×32 RGBA
```

## Parity with the Python renderer

Palette is bit-identical. Renders differ from the Python originals by ~2% of pixels on average
(worst ~4.5%), all on shading-band boundaries; they are not visibly different. That is the
renderer's own noise floor: the Python version changes about as many pixels if a shape moves by
0.02px, because Pillow's polygon rasterizer and this one disagree by a sub-pixel at edges.
`cargo test` enforces mean < 3% and worst < 6% against `testdata/golden`.

## Performance

~5 ms per frame on one core (about 18× the Python renderer), and batch commands use every core.
10,000 sprites × 8 frames is roughly 7 minutes on one core, or about a minute on 8.

## Prototype

`prototype/` holds the first version: a Python renderer (`factory.py`), a gallery builder, and a
TypeScript MCP server that drives it through a JSON-lines bridge. The Rust code replaced it; it is
kept because its renders are the reference for the parity tests, and the TypeScript end-to-end
suite runs against either server:

```bash
cd prototype && pip install numpy pillow scipy
python3 factory.py sheet nouns/food.json --out food.png
cd mcp && npm install && npm run build && npm test                               # TS server
SPRITE_SERVER="../../target/release/sprite mcp" npx tsx test/e2e.ts             # same suite, Rust server
```

The parity tests compare against `testdata/golden`, which the Python renderer produced. After an
intentional style change, regenerate those reference strips or relax the thresholds in
`crates/sprite-core/tests/golden.rs`.
