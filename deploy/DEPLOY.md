# Deploying to asleepace.com

What you get: `https://sprites.asleepace.com`, public to read and create, write token to edit or delete, serving
- **`/mcp`** — the Sprite Factory MCP server (Streamable HTTP) for Claude and other agents
- **`/v1/...`** — read-only REST for game prototypes (PNG, GIF, sheets, atlases)

Both read and write the same library folder, so a sprite a model creates is fetchable a second later.

```
browser / Claude ──TLS──▶ nginx :443 ──▶ sprite serve 127.0.0.1:8787 ──▶ /var/lib/sprite/library
                          (rate limit,      (systemd, user "sprite",
                           no query logs)     only the library is writable)
```

## 1. DNS
Add an `A` record: `sprites.asleepace.com → <droplet IP>`.

## 2. Build on the server
Building there avoids glibc mismatches.
```bash
curl https://sh.rustup.rs -sSf | sh -s -- -y   # if Rust isn't installed
git clone git@github.com:asleepace/sprite-factory.git /opt/src/sprite-factory
cd /opt/src/sprite-factory
cargo build --release
```
The build needs about 1 GB of RAM. On a small droplet, add swap first or build on a matching Ubuntu machine and copy `target/release/sprite` over.

To upgrade later: `cd /opt/src/sprite-factory && git pull && cargo build --release && sudo ./deploy/install.sh`.
Before upgrading, `target/release/sprite check /var/lib/sprite/library` confirms every stored sprite
still passes the new binary's spec limits (it lists any that don't, and exits non-zero).

## 3. Install the service
```bash
sudo ./deploy/install.sh
```
This creates the `sprite` system user, `/etc/sprite/{sprite.env,tokens}` and `/var/lib/sprite/library` (seeded with the 100 sample sprites), installs and starts `sprite.service`, and prints your first token **once**. Re-running it upgrades the binary and keeps your tokens, settings and library.

Edit `/etc/sprite/sprite.env` if your hostname isn't `sprites.asleepace.com`, then `sudo systemctl restart sprite`.

Upgrading an older install? Add a scope to each line of `/etc/sprite/tokens` (see [Tokens](#tokens)):
`write` for you, `read` for prototypes. The installer never overwrites `sprite.env`, so add
`SPRITE_PUBLIC_URL=https://sprites.asleepace.com` yourself if you want `export_gif` to include download links.

## 4. nginx + TLS
```bash
sudo cp deploy/nginx-sprites.conf /etc/nginx/sites-available/sprites.asleepace.com
sudo ln -s ../sites-available/sprites.asleepace.com /etc/nginx/sites-enabled/
sudo nginx -t && sudo systemctl reload nginx
sudo certbot --nginx -d sprites.asleepace.com
```

## 5. Check it
```bash
TOKEN=...   # from install.sh
curl https://sprites.asleepace.com/healthz                                         # ok
curl https://sprites.asleepace.com/v1/sprites | head                               # public
curl -o cat.gif https://sprites.asleepace.com/v1/sprites/cat.gif
```

## Use it

**Claude Code**
```bash
claude mcp add --transport http sprite-factory https://sprites.asleepace.com/mcp \
  --header "Authorization: Bearer $TOKEN"
```
Without the header, MCP still works for reading, rendering, exporting and creating new sprites;
the keyed tools (below) answer with an error asking for a write token. In the Claude apps, add it as
a custom connector with the same URL; add the header if your plan allows it, to unlock editing.

**Game prototypes (Phaser)**
```ts
const BASE = "https://sprites.asleepace.com/v1";
this.load.atlas("sprites", `${BASE}/atlas.png?scale=2`, `${BASE}/atlas.json?scale=2`);
// later
this.add.sprite(100, 100, "sprites", "cat_0");
```
Animations: `atlas.json` lists each sprite's frames and `frameMs` under `animations`.

**Plain `<img>`**: `https://sprites.asleepace.com/v1/sprites/apple.png?scale=4`

For a prototype served from a dev origin that reads pixels (canvas), add that origin to
`SPRITE_CORS` in `/etc/sprite/sprite.env`.

## REST reference
| Route | Notes |
|---|---|
| `GET /healthz` | |
| `GET /v1/sprites?category=` | names, categories, canvas sizes, frame counts, asset URLs |
| `GET /v1/sprites/{name}.png` | `?scale=1..16&frame=N` (scale up to 8 for a 64px sprite, 4 for 128px) |
| `GET /v1/sprites/{name}.gif` | animated, timed from the spec's `ms`/`hold`; `?scale=` (default 4 for a 32px sprite, 2 at 64, 1 at 128: 128px wide; at most 512px), `?bg=transparent\|rrggbb`, `?loop=forward\|pingpong`, `?repeat=N` |
| `GET /v1/sprites/{name}.sheet.png` | all frames in a row |
| `GET /v1/sprites/{name}.json` | the spec |
| `GET /v1/atlas.png`, `/v1/atlas.json` | `?category=`, `?names=a,b`, `?scale=1..8`, `?max=` (largest side in px, default 4096, up to 8192). `413` if the selection doesn't fit; filter it or lower the scale. Identical frames share a cell, so look frames up by key. Sprites of different canvas sizes get cells of their own size: read each frame's `w`/`h` (`meta.spriteSize` is only set when every sprite shares one) |
| `POST /v1/render?format=png\|gif\|sheet\|json` | render a spec without saving (256 KB limit). Send it with `Content-Type: application/json` (anything else is a `415`), so other sites' pages can't call it without passing `SPRITE_CORS` |

Everything returns an `ETag`; send `If-None-Match` to get `304` when nothing changed.
No REST route needs a token. Writes only happen through MCP.

## Access
| | no token | write token (bearer header) |
|---|---|---|
| REST: list, PNG, GIF, sheet, spec, atlas, `POST /v1/render` | ✓ | ✓ |
| MCP: `style_guide`, `list_sprites`, `get_sprite`, `render_sprite`, `sprite_history`, `export_gif` | ✓ | ✓ |
| MCP: `create_sprite` with a name that has never been used (60 per hour across all tokenless callers; `SPRITE_ANON_CREATES_PER_HOUR`) | ✓ | ✓ |
| MCP: `create_sprite` over an existing or deleted name, `edit_sprite`, `delete_sprite`, `restore_sprite` | | ✓ |
| MCP: `export_atlas` (writes files; the REST atlas is public), `blind_review` (spends `ANTHROPIC_API_KEY` credits) | | ✓ |

`export_gif` returns the GIF inline and as a URL for everyone, but only saves a copy in `_export/`
for a write token. A token that doesn't match, or an `Authorization` header that isn't
`Bearer <token>`, is a `401`, not a silent fall back to anonymous. History records who made each
change: the token label, or `anon` (`anon` and `local` can't be used as token labels).

## Tokens
`/etc/sprite/tokens` holds one `label:scope:token` per line. Give each person or prototype its own
(`openssl rand -hex 32`), and delete a line to revoke it, then `systemctl restart sprite`.

| scope | adds |
|---|---|
| `read` | nothing beyond public access; labels the caller in history (header or `?key=`) |
| `write` | the keyed MCP tools above — header only |

```
colin:write:3f9c…
racecar-proto:read:a71e…
```
A `?key=` token ends up in browser history and bundles, so it never unlocks the keyed tools, even
a write token; it's treated like a read token.
Entries written as `label:token` (from before scopes) still work as `write`; the server prints a
warning at startup until you add a scope.

## Blind review on the server
Set `ANTHROPIC_API_KEY` in `/etc/sprite/sprite.env` (with a spend limit on the key) and every
client gets working `blind_review`, regardless of sampling support. Leave it unset and you can
uncomment the `IPAddressDeny` lines in the unit to cut the service off from the internet entirely.

## Operating it
```bash
journalctl -u sprite -f                 # logs
sudo systemctl restart sprite           # after editing env or tokens
sudo tar czf sprites-$(date +%F).tgz -C /var/lib/sprite library   # back up the library
```
The library is plain JSON, so it's also easy to keep in git. Every save is also kept under
`_history/NAME/` (one file per revision, plus `log.jsonl` with who and when), and deleted sprites
move to `_trash/`, so `sprite_history` / `restore_sprite` can undo any change, including a delete.
