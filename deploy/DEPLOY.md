# Deploying to asleepace.com (private)

What you get: `https://sprites.asleepace.com`, token-only, serving
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

## 3. Install the service
```bash
sudo ./deploy/install.sh
```
This creates the `sprite` system user, `/etc/sprite/{sprite.env,tokens}` and `/var/lib/sprite/library` (seeded with the 100 sample sprites), installs and starts `sprite.service`, and prints your first token **once**. Re-running it upgrades the binary and keeps your tokens, settings and library.

Edit `/etc/sprite/sprite.env` if your hostname isn't `sprites.asleepace.com`, then `sudo systemctl restart sprite`.

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
curl -H "Authorization: Bearer $TOKEN" https://sprites.asleepace.com/v1/sprites | head
curl -o cat.gif -H "Authorization: Bearer $TOKEN" https://sprites.asleepace.com/v1/sprites/cat.gif
```

## Use it

**Claude Code**
```bash
claude mcp add --transport http sprite-factory https://sprites.asleepace.com/mcp \
  --header "Authorization: Bearer $TOKEN"
```
In the Claude apps, add it as a custom connector with the same URL if your plan allows custom
connectors with headers; otherwise use Claude Code or the stdio binary locally.

**Game prototypes (Phaser)**
```ts
const BASE = "https://sprites.asleepace.com/v1";
const KEY = import.meta.env.VITE_SPRITE_KEY; // ?key= because image loaders can't set headers

this.load.atlas("sprites", `${BASE}/atlas.png?scale=2&key=${KEY}`, `${BASE}/atlas.json?scale=2&key=${KEY}`);
// later
this.add.sprite(100, 100, "sprites", "cat_0");
```
Animations: `atlas.json` lists each sprite's frames and `frameMs` under `animations`.

**Plain `<img>`**: `https://sprites.asleepace.com/v1/sprites/apple.png?scale=4&key=…`

For a prototype served from a dev origin that reads pixels (canvas), add that origin to
`SPRITE_CORS` in `/etc/sprite/sprite.env`.

## REST reference
| Route | Notes |
|---|---|
| `GET /healthz` | no auth |
| `GET /v1/sprites?category=` | names, categories, frame counts, asset URLs |
| `GET /v1/sprites/{name}.png` | `?scale=1..16&frame=N` |
| `GET /v1/sprites/{name}.gif` | animated, `?scale=` (default 4) |
| `GET /v1/sprites/{name}.sheet.png` | all frames in a row |
| `GET /v1/sprites/{name}.json` | the spec |
| `GET /v1/atlas.png`, `/v1/atlas.json` | `?category=`, `?names=a,b`, `?scale=1..8` |
| `POST /v1/render?format=png\|gif\|sheet\|json` | render a spec without saving (256 KB limit) |

Everything returns an `ETag`; send `If-None-Match` to get `304` when nothing changed.
Writes only happen through MCP (`create_sprite`, `edit_sprite`, `delete_sprite`).

## Tokens
`/etc/sprite/tokens` holds one `label:token` per line. Give each person or prototype its own
(`openssl rand -hex 32`), and delete a line to revoke it, then `systemctl restart sprite`.
Treat `?key=` tokens as low-trust (they end up in browser history and bundles); give
prototypes their own token so you can rotate it without touching anyone else's.

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
The library is plain JSON, so it's also easy to keep in git.
