#!/usr/bin/env python3
"""Render every spec in nouns/*.json (except _test) into a single self-contained gallery page."""
import base64, io, json, sys
from pathlib import Path
from PIL import Image
import numpy as np
import factory as F

ROOT = Path(__file__).parent
files = sys.argv[1:] or ["nouns/animals.json", "nouns/food.json", "nouns/home.json", "nouns/world.json"]
specs = F.load_specs([ROOT / f for f in files])

def b64png(arr):
    buf = io.BytesIO()
    Image.fromarray(arr, "RGBA").save(buf, "PNG", optimize=True)
    return "data:image/png;base64," + base64.b64encode(buf.getvalue()).decode()

sprites, cats, total = [], [], 0
for sp in specs:
    frames = F.render_anim(sp)
    total += len(frames)
    cat = sp.get("cat", "misc")
    if cat not in cats:
        cats.append(cat)
    sprites.append({
        "name": sp["name"], "cat": cat, "anim": sp.get("anim", "none"),
        "frames": len(frames), "ms": sp.get("ms", 110),
        "prims": len(sp["prims"]),
        "sheet": b64png(np.concatenate(frames, axis=1)),
        "first": b64png(frames[0]),
        "spec": json.dumps(sp, separators=(",", ":")),
    })
    print(".", end="", flush=True)

data = {"size": F.SIZE, "sprites": sprites, "cats": cats, "frames_total": total,
        "palette": F.PALETTE, "palette_count": sum(len(v) for v in F.PALETTE.values())}
html = (ROOT / "gallery_template.html").read_text().replace("/*DATA*/", json.dumps(data))
out = ROOT / "out" / "gallery.html"
out.write_text(html)
print(f"\n{len(sprites)} sprites, {total} frames → {out} ({len(html)//1024} KB)")
