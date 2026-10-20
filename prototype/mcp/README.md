# sprite-factory-mcp

MCP server that lets any model make 32×32 pixel-art sprites in one locked house style.
The model describes shapes and materials; the Python renderer owns palette, lighting,
outline and animation, so every sprite comes out of the same set no matter who wrote it.

## Tools

| Tool | What it does |
|---|---|
| `style_guide` | Shape DSL, materials, animation reference. Call once before authoring. |
| `create_sprite` | Shapes → rendered sprite + animation frames + style warnings. Saved to the library. |
| `edit_sprite` | Surgical `prim_ops` (set / merge / insert / delete) or full replace, then re-render. |
| `render_sprite` | Re-render without changes. |
| `get_sprite` / `list_sprites` / `delete_sprite` | Library management. `list_sprites {sheet:true}` returns a contact sheet. |
| `blind_review` | Sends an unlabeled, shuffled sheet to a fresh model via MCP sampling and grades the guesses (uses each sprite's `aka` synonyms). Falls back to returning the sheet + answer key when the client has no sampling. |
| `export_atlas` | One PNG atlas + Phaser/Pixi-style JSON (frames, per-sprite animation, frame ms). |

Every create/edit returns images, so the model sees its work and revises — that loop is the point.
Bad specs are rejected with specific errors and never saved.

## Setup

```bash
pip install numpy pillow scipy        # renderer deps
cd mcp && npm install && npm run build
npm run import -- ..                   # optional: load the 100 sample sprites into ../library
npm test                               # end-to-end test over stdio
```

### Claude Code
```bash
claude mcp add sprite-factory -- node /path/to/spritefactory/mcp/dist/server.js
```

### Claude Desktop / other clients
```json
{
  "mcpServers": {
    "sprite-factory": {
      "command": "node",
      "args": ["/path/to/spritefactory/mcp/dist/server.js"],
      "env": { "SPRITE_LIBRARY": "/path/to/library" }
    }
  }
}
```

Env: `SPRITE_LIBRARY` (sprite JSON folder, default `../library`), `SPRITE_FACTORY_DIR`
(renderer folder, default `..`), `SPRITE_PYTHON` (default `python3`).

## Design notes
- The renderer runs as one long-lived Python process (JSON lines over stdio), so numpy/scipy load once.
- Style changes (palette, light, outline, canvas size) are edits to `factory.py` — authors can't drift from them.
- `blind_review` uses `includeContext: "none"` so the reviewing model never sees the names.
  Not every MCP client implements sampling; check yours. Without it, `blind_review` returns the unlabeled sheet and key for a separate reviewer.
