#!/usr/bin/env python3
"""
JSON-lines bridge between the MCP server (TypeScript) and the Python renderer.

One request per line on stdin: {"id": 1, "cmd": "render", ...}
One response per line on stdout: {"id": 1, "ok": true, ...} or {"id": 1, "ok": false, "error": "..."}

Kept as a long-lived process so numpy/scipy import once, not per tool call.
"""
import base64
import io
import json
import math
import sys
import traceback

import numpy as np
from PIL import Image, ImageDraw, ImageFont

import factory as F

PRIM_TYPES = {"ellipse", "rect", "poly", "arc", "star", "ngon", "line", "px"}
try:
    FONT = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", 14)
except Exception:
    FONT = ImageFont.load_default()


def png(arr_or_img):
    img = arr_or_img if isinstance(arr_or_img, Image.Image) else Image.fromarray(arr_or_img, "RGBA")
    buf = io.BytesIO()
    img.save(buf, "PNG", optimize=True)
    return base64.b64encode(buf.getvalue()).decode()


def on_backdrop(rgba, scale, bg=(246, 245, 240)):
    big = Image.fromarray(F.upscale(rgba, scale), "RGBA")
    out = Image.new("RGB", big.size, bg)
    out.paste(big, (0, 0), big)
    return out


def validate(spec):
    """Structural errors (block render) + nothing else."""
    errs = []
    if not isinstance(spec.get("prims"), list) or not spec["prims"]:
        errs.append("spec.prims must be a non-empty array")
        return errs
    for i, p in enumerate(spec["prims"]):
        t = p.get("t")
        if t not in PRIM_TYPES:
            errs.append(f"prims[{i}]: unknown type {t!r} (use one of {sorted(PRIM_TYPES)})")
        m = p.get("m")
        if m is not None and m not in F.PALETTE:
            errs.append(f"prims[{i}]: unknown material {m!r}")
        if "clip" in p and not (isinstance(p["clip"], int) and 0 <= p["clip"] < i):
            errs.append(f"prims[{i}]: clip must be the index of an EARLIER prim")
    if spec.get("anim", "none") not in F.PRESETS:
        errs.append(f"unknown anim {spec.get('anim')!r} (use one of {sorted(F.PRESETS)})")
    for k, tr in enumerate(spec.get("tracks", [])):
        if tr.get("prop") not in ("dx", "dy", "rot", "sx", "sy"):
            errs.append(f"tracks[{k}]: prop must be dx|dy|rot|sx|sy")
        g = tr.get("g", "*")
        if g != "*" and not any(p.get("g") == g for p in spec["prims"]):
            errs.append(f"tracks[{k}]: no prim has group {g!r}")
    return errs


def lint(spec, frames):
    """Measurable style checks the author should react to."""
    notes = []
    f0 = frames[0]
    a = f0[:, :, 3] > 0
    if not a.any():
        return {"warnings": ["sprite rendered empty"], "stats": {}}
    ys, xs = np.nonzero(a)
    w, h = xs.max() - xs.min() + 1, ys.max() - ys.min() + 1
    span = max(w, h)
    if span < 26:
        notes.append(f"subject spans {span}px on its longest axis; aim for 26–30px so it fills the canvas")
    for k, f in enumerate(frames):
        b = f[:, :, 3] > 0
        if b[0].any() or b[-1].any() or b[:, 0].any() or b[:, -1].any():
            notes.append(f"frame {k} touches the canvas edge, so the outline is clipped; keep shapes inside 1.5–30.5")
            break
    if ys.max() < 29 and spec.get("cat") in ("animals", "home", "places", "vehicles"):
        notes.append(f"lowest pixel is y={ys.max()}; upright things should rest near y=30")
    colors = {tuple(c) for c in f0[a][:, :3].tolist()}
    if len(colors) < 6:
        notes.append(f"only {len(colors)} colours; sprite may look flat — vary materials or shading")
    return {"warnings": notes,
            "stats": {"bbox": [int(xs.min()), int(ys.min()), int(xs.max()), int(ys.max())],
                      "span_px": int(span), "colors": len(colors), "frames": len(frames),
                      "prims": len(spec["prims"])}}


def cmd_render(req):
    spec = req["spec"]
    errs = validate(spec)
    if errs:
        return {"ok": False, "error": "invalid spec", "details": errs}
    scale = int(req.get("scale", 8))
    frames = F.render_anim(spec)
    out = {"ok": True, "lint": lint(spec, frames),
           "still": png(on_backdrop(frames[0], scale))}
    if len(frames) > 1:
        strip = np.concatenate(frames, axis=1)
        out["strip"] = png(on_backdrop(strip, max(2, scale // 2)))
    if req.get("raw"):
        out["frames_raw"] = [png(f) for f in frames]
    return out


def labelled_grid(items, scale, cols, labels=True, numbered=False):
    cell = F.SIZE * scale + 16
    lab = 20
    rows = max(1, math.ceil(len(items) / cols))
    sheet = Image.new("RGB", (min(cols, len(items)) * cell, rows * (cell + lab)), (246, 245, 240))
    dr = ImageDraw.Draw(sheet)
    for k, (label, spec) in enumerate(items):
        f = F.render_frame(spec, 0.0)
        im = Image.fromarray(F.upscale(f, scale), "RGBA")
        x, y = (k % cols) * cell, (k // cols) * (cell + lab)
        sheet.paste(im, (x + 8, y + 8), im)
        if labels:
            dr.text((x + 8, y + cell), f"#{k + 1}" if numbered else label, fill=(30, 30, 40), font=FONT)
    return sheet


def cmd_sheet(req):
    items = [(s["name"], s) for s in req["specs"]]
    sheet = labelled_grid(items, int(req.get("scale", 3)), int(req.get("cols", 8)),
                          numbered=bool(req.get("numbered")))
    return {"ok": True, "image": png(sheet)}


def cmd_atlas(req):
    """Pack every frame of every sprite into one PNG + a frame map (Phaser/Pixi-style JSON hash)."""
    specs = req["specs"]
    scale = int(req.get("scale", 1))
    rows = []
    for s in specs:
        rows.append((s, F.render_anim(s)))
    width = max(len(fr) for _, fr in rows) * F.SIZE
    atlas = np.zeros((len(rows) * F.SIZE, width, 4), np.uint8)
    frames_map, anims = {}, {}
    for r, (s, fr) in enumerate(rows):
        keys = []
        for k, f in enumerate(fr):
            atlas[r * F.SIZE:(r + 1) * F.SIZE, k * F.SIZE:(k + 1) * F.SIZE] = f
            key = f"{s['name']}_{k}"
            frames_map[key] = {"frame": {"x": k * F.SIZE * scale, "y": r * F.SIZE * scale,
                                         "w": F.SIZE * scale, "h": F.SIZE * scale}}
            keys.append(key)
        anims[s["name"]] = {"frames": keys, "frameMs": s.get("ms", 110), "category": s.get("cat")}
    img = Image.fromarray(F.upscale(atlas, scale) if scale > 1 else atlas, "RGBA")
    img.save(req["png_path"])
    meta = {"meta": {"image": req["png_name"], "size": {"w": img.width, "h": img.height},
                     "scale": scale, "spriteSize": F.SIZE * scale},
            "frames": frames_map, "animations": anims}
    with open(req["json_path"], "w") as fh:
        json.dump(meta, fh, indent=1)
    preview = on_backdrop(atlas, 2)
    return {"ok": True, "width": img.width, "height": img.height, "frames": len(frames_map),
            "preview": png(preview)}


def cmd_palette(req):
    return {"ok": True, "materials": {k: ["#%02x%02x%02x" % c for c in v] for k, v in F.PALETTE.items()},
            "presets": sorted(F.PRESETS)}


CMDS = {"render": cmd_render, "sheet": cmd_sheet, "atlas": cmd_atlas, "palette": cmd_palette,
        "ping": lambda r: {"ok": True}}


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        rid = None
        try:
            req = json.loads(line)
            rid = req.get("id")
            res = CMDS[req["cmd"]](req)
        except Exception as e:
            res = {"ok": False, "error": f"{type(e).__name__}: {e}",
                   "trace": traceback.format_exc(limit=3)}
        res["id"] = rid
        sys.stdout.write(json.dumps(res) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
