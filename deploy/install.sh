#!/usr/bin/env bash
# Installs or upgrades Sprite Factory as a systemd service. Run as root from the repo root
# after `cargo build --release`. Safe to re-run: keeps existing tokens, env and library.
set -euo pipefail

BIN=${1:-target/release/sprite}
[[ -x "$BIN" ]] || { echo "build first: cargo build --release (or pass the binary path)"; exit 1; }
[[ $EUID -eq 0 ]] || { echo "run as root (sudo)"; exit 1; }

id sprite &>/dev/null || useradd --system --home-dir /var/lib/sprite --shell /usr/sbin/nologin sprite
install -d -o sprite -g sprite -m 0750 /var/lib/sprite /var/lib/sprite/library
install -d -o root -g sprite -m 0750 /etc/sprite
install -o root -g root -m 0755 "$BIN" /usr/local/bin/sprite

if [[ ! -f /etc/sprite/sprite.env ]]; then
  install -o root -g sprite -m 0640 deploy/sprite.env.example /etc/sprite/sprite.env
  echo "→ wrote /etc/sprite/sprite.env — check SPRITE_HOSTS"
fi

if [[ ! -s /etc/sprite/tokens ]]; then
  TOKEN=$(openssl rand -hex 32)
  printf '# label:token — one per person or prototype; remove a line to revoke\ncolin:%s\n' "$TOKEN" > /etc/sprite/tokens
  chown root:sprite /etc/sprite/tokens && chmod 0640 /etc/sprite/tokens
  echo "→ created token for 'colin' (shown once):"
  echo "   $TOKEN"
fi

if [[ -z "$(ls -A /var/lib/sprite/library 2>/dev/null)" && -d testdata/nouns ]]; then
  echo "→ seeding the library with the 100 sample sprites"
  python3 - <<'PY'
import json, glob, os
for f in glob.glob("testdata/nouns/*.json"):
    for s in json.load(open(f)):
        p = f"/var/lib/sprite/library/{s['name']}.json"
        open(p, "w").write(json.dumps(s, indent=1) + "\n")
PY
  chown -R sprite:sprite /var/lib/sprite/library
fi

install -o root -g root -m 0644 deploy/sprite.service /etc/systemd/system/sprite.service
systemctl daemon-reload
systemctl enable --now sprite
systemctl restart sprite
sleep 1
systemctl --no-pager --lines=3 status sprite
curl -fsS http://127.0.0.1:8787/healthz && echo "  ← healthz"
