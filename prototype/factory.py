#!/usr/bin/env python3
"""
Sprite Factory — a tiny 2.5D → pixel-art renderer.

The style lives HERE (palette, light, outline, canvas). Authors (humans or LLMs)
only describe WHAT to draw as a list of primitives; the renderer guarantees
every sprite comes out with the same palette, light direction, outline and size.

Pipeline per frame:
  shapes (vector, 32x32 space) → rasterize at 8x → inflate each shape into a
  height field (distance transform) → normals → fixed top-left light →
  quantize to a 5-step material ramp → weighted-mode downsample to 32x32 →
  internal edge lines → pixel overrides → selective outer outline → cleanup.

Because shading is computed from geometry every frame, animations (rotate,
squash, flap) re-light correctly instead of smearing pre-drawn pixels.
"""
from __future__ import annotations

import colorsys
import json
import math
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont
from scipy import ndimage

# ─────────────────────────── style spec (owned by "designers") ───────────────────────────
SIZE = 32          # output canvas (px)
SS = 8             # supersampling factor
FRAMES = 8         # frames per animation
LIGHT = np.array([-0.55, -0.75, 0.95])
LIGHT = LIGHT / np.linalg.norm(LIGHT)

# Base colours (ramp index 2). Ramps are generated with hue-shifting:
# shadows drift toward blue/purple, highlights toward warm yellow.
BASES = {
    "red":      "#d8343f",
    "orange":   "#ec7a2c",
    "yellow":   "#f5cf3a",
    "gold":     "#d9a521",
    "lime":     "#8ccf3d",
    "green":    "#3fa548",
    "forest":   "#2c7a46",
    "teal":     "#2aa6a0",
    "sky":      "#5cb8ec",
    "blue":     "#3a6fd6",
    "navy":     "#2e3d8f",
    "purple":   "#8a4ec4",
    "pink":     "#ef7fb0",
    "brown":    "#8a5232",
    "wood":     "#b97c45",
    "tan":      "#d9b07a",
    "skin":     "#f0b98c",
    "cream":    "#f2e6c9",
    "white":    "#eef1f5",
    "gray":     "#9aa0a8",
    "steel":    "#6f8299",
    "charcoal": "#4a4a57",
    "black":    "#2a2833",
    "glass":    "#a8e0f0",
}


def _hex(h):
    h = h.lstrip("#")
    return tuple(int(h[i:i + 2], 16) / 255 for i in (0, 2, 4))


def _ramp(base_hex):
    r, g, b = _hex(base_hex)
    h, l, s = colorsys.rgb_to_hls(r, g, b)
    out = []
    # (lightness delta, hue shift toward target, saturation scale)
    steps = [(-0.34, -1.0, 1.10),   # 0 outline / deep shadow
             (-0.15, -0.5, 1.05),   # 1 shadow
             (0.00, 0.0, 1.00),     # 2 base
             (+0.12, +0.5, 0.95),   # 3 light
             (+0.24, +1.0, 0.80)]   # 4 highlight
    for dl, dh, ds in steps:
        hh = h
        if dh < 0:   # shift toward blue-purple (0.70)
            hh = h + (0.70 - h) * 0.12 * -dh if abs(0.70 - h) < 0.5 else h - 0.04 * -dh
        elif dh > 0:  # shift toward yellow (0.14)
            hh = h + (0.14 - h) * 0.10 * dh if abs(0.14 - h) < 0.5 else h + 0.03 * dh
        ll = min(0.96, max(0.06, l + dl))
        ss_ = min(1.0, s * ds)
        if l > 0.85 and dl < 0:   # whites need bigger steps to read as shading
            ll = min(0.96, max(0.06, l + dl * 1.25))
        rr, gg, bb = colorsys.hls_to_rgb(hh % 1.0, ll, ss_)
        out.append((round(rr * 255), round(gg * 255), round(bb * 255)))
    return out


PALETTE = {name: _ramp(hx) for name, hx in BASES.items()}


# ─────────────────────────── geometry helpers ───────────────────────────
def _affine(rot=0.0, sx=1.0, sy=1.0, dx=0.0, dy=0.0, pivot=(16, 16)):
    """3x3 matrix: translate(-pivot) → scale → rotate → translate(pivot + d)."""
    px, py = pivot
    c, s = math.cos(math.radians(rot)), math.sin(math.radians(rot))
    T1 = np.array([[1, 0, -px], [0, 1, -py], [0, 0, 1]], float)
    S = np.array([[sx, 0, 0], [0, sy, 0], [0, 0, 1]], float)
    R = np.array([[c, -s, 0], [s, c, 0], [0, 0, 1]], float)
    T2 = np.array([[1, 0, px + dx], [0, 1, py + dy], [0, 0, 1]], float)
    return T2 @ R @ S @ T1


def _apply(M, pts):
    pts = np.asarray(pts, float)
    h = np.hstack([pts, np.ones((len(pts), 1))])
    return (h @ M.T)[:, :2]


def _chaikin(pts, it):
    pts = np.asarray(pts, float)
    for _ in range(it):
        q = 0.75 * pts + 0.25 * np.roll(pts, -1, axis=0)
        r = 0.25 * pts + 0.75 * np.roll(pts, -1, axis=0)
        pts = np.empty((len(pts) * 2, 2))
        pts[0::2], pts[1::2] = q, r
    return pts


def _ellipse_pts(cx, cy, rx, ry, a0=0, a1=360, n=72):
    ts = np.radians(np.linspace(a0, a1, n, endpoint=(a1 - a0) % 360 != 0))
    return np.stack([cx + rx * np.cos(ts), cy + ry * np.sin(ts)], 1)


def _rrect_pts(x0, y0, x1, y1, r):
    r = max(0.0, min(r, (x1 - x0) / 2, (y1 - y0) / 2))
    if r < 1e-3:
        return np.array([[x0, y0], [x1, y0], [x1, y1], [x0, y1]], float)
    pts = []
    for cx, cy, a in [(x1 - r, y0 + r, -90), (x1 - r, y1 - r, 0), (x0 + r, y1 - r, 90), (x0 + r, y0 + r, 180)]:
        ts = np.radians(np.linspace(a, a + 90, 10))
        pts += list(zip(cx + r * np.cos(ts), cy + r * np.sin(ts)))
    return np.array(pts)


def prim_outline(p):
    """Return (kind, points, width, local_center) in sprite space before transforms."""
    t = p["t"]
    if t == "ellipse":
        cx, cy = p["c"]
        rx, ry = p["r"] if isinstance(p["r"], (list, tuple)) else (p["r"], p["r"])
        pts = _ellipse_pts(cx, cy, rx, ry)
        return "poly", pts, 0, (cx, cy)
    if t == "arc":   # filled pie/segment: c, r, a0, a1 (degrees, 0=right, 90=down)
        cx, cy = p["c"]
        rx, ry = p["r"] if isinstance(p["r"], (list, tuple)) else (p["r"], p["r"])
        pts = _ellipse_pts(cx, cy, rx, ry, p.get("a0", 180), p.get("a1", 360), 48)
        if p.get("pie", True):
            pts = np.vstack([[cx, cy], pts])
        return "poly", pts, 0, (cx, cy)
    if t == "rect":
        x0, y0, x1, y1 = p["p"]
        pts = _rrect_pts(x0, y0, x1, y1, p.get("round", 0))
        return "poly", pts, 0, ((x0 + x1) / 2, (y0 + y1) / 2)
    if t == "poly":
        pts = np.asarray(p["pts"], float)
        if p.get("smooth"):
            pts = _chaikin(pts, int(p["smooth"]))
        return "poly", pts, 0, tuple(pts.mean(0))
    if t in ("star", "ngon"):
        cx, cy = p["c"]
        n = p.get("n", 5)
        r1 = p["r"]
        r2 = p.get("r2", r1 * 0.45) if t == "star" else r1
        a0 = math.radians(p.get("a", -90))
        k = n * 2 if t == "star" else n
        pts = []
        for i in range(k):
            rr = r1 if (t == "ngon" or i % 2 == 0) else r2
            a = a0 + i * 2 * math.pi / k
            pts.append((cx + rr * math.cos(a), cy + rr * math.sin(a)))
        pts = np.array(pts)
        if p.get("smooth"):
            pts = _chaikin(pts, int(p["smooth"]))
        return "poly", pts, 0, (cx, cy)
    if t == "line":
        pts = np.asarray(p["pts"], float)
        return "line", pts, p.get("w", 1.0), tuple(pts.mean(0))
    raise ValueError(f"unknown primitive type {t!r}")


# ─────────────────────────── animation ───────────────────────────
PRESETS = {
    "none":   [],
    "idle":   [{"g": "*", "prop": "sy", "amp": 0.04, "pivot": "bottom"},
               {"g": "*", "prop": "sx", "amp": -0.03, "pivot": "bottom"}],
    "bob":    [{"g": "*", "prop": "dy", "amp": 1.2}],
    "float":  [{"g": "*", "prop": "dy", "amp": 1.5}, {"g": "*", "prop": "rot", "amp": 4, "phase": 0.25}],
    "hop":    [{"g": "*", "prop": "dy", "amp": -3.0, "wave": "abs"},
               {"g": "*", "prop": "sy", "amp": 0.10, "wave": "abs", "pivot": "bottom"},
               {"g": "*", "prop": "sx", "amp": -0.07, "wave": "abs", "pivot": "bottom"}],
    "wobble": [{"g": "*", "prop": "rot", "amp": 7, "pivot": "bottom"}],
    "sway":   [{"g": "*", "prop": "rot", "amp": 5, "pivot": "bottom"}],
    "pulse":  [{"g": "*", "prop": "sx", "amp": 0.07, "pivot": "center"},
               {"g": "*", "prop": "sy", "amp": 0.07, "pivot": "center"}],
    "spin":   [{"g": "*", "prop": "rot", "amp": 360, "wave": "saw", "pivot": "center"}],
    "drive":  [{"g": "*", "prop": "dy", "amp": -0.6, "wave": "abs2"}],
}


def _wave(kind, t):
    if kind == "sin":
        return math.sin(2 * math.pi * t)
    if kind == "abs":
        return abs(math.sin(math.pi * t))
    if kind == "abs2":
        return abs(math.sin(2 * math.pi * t))
    if kind == "saw":
        return t
    if kind == "square":
        return 1.0 if t < 0.5 else 0.0
    raise ValueError(kind)


def _bbox_of(prims):
    allp = []
    for p in prims:
        if p["t"] == "px":
            allp += [list(q) for q in p["pts"]]
            continue
        kind, pts, w, _ = prim_outline(p)
        allp += pts.tolist()
    a = np.array(allp)
    return a[:, 0].min(), a[:, 1].min(), a[:, 0].max(), a[:, 1].max()


def _resolve_pivot(pv, prims):
    if isinstance(pv, (list, tuple)):
        return tuple(pv)
    x0, y0, x1, y1 = _bbox_of(prims)
    if pv in (None, "center"):
        return ((x0 + x1) / 2, (y0 + y1) / 2)
    if pv == "bottom":
        return ((x0 + x1) / 2, y1)
    if pv == "top":
        return ((x0 + x1) / 2, y0)
    raise ValueError(pv)


def _prim_center(p):
    if p["t"] == "px":
        return tuple(np.asarray(p["pts"], float).mean(0))
    _, pts, _, _ = prim_outline(p)
    return ((pts[:, 0].min() + pts[:, 0].max()) / 2, (pts[:, 1].min() + pts[:, 1].max()) / 2)


def frame_transforms(spec, t):
    """Return a list of 3x3 matrices, one per primitive, for time t in [0,1).

    Track fields: g (group or '*'), prop (dx|dy|rot|sx|sy), amp, wave (sin|abs|abs2|saw|square),
    phase (0-1), pivot ('center'|'bottom'|'top'|[x,y]|'each' = every member around its own centre).
    """
    tracks = list(PRESETS.get(spec.get("anim", "none"), []))
    tracks += spec.get("tracks", [])
    prims = spec["prims"]
    G = np.eye(3)
    local = [np.eye(3) for _ in prims]
    for tr in tracks:
        g = tr.get("g", "*")
        idxs = list(range(len(prims))) if g == "*" else [i for i, p in enumerate(prims) if p.get("g") == g]
        if not idxs:
            continue
        v = tr["amp"] * _wave(tr.get("wave", "sin"), (t + tr.get("phase", 0.0)) % 1.0)
        prop = tr["prop"]
        pv = tr.get("pivot", "center")

        def mk(pivot):
            kw = {"pivot": pivot}
            kw[prop] = (1.0 + v) if prop in ("sx", "sy") else v
            return _affine(**kw)

        if g == "*":
            G = mk(_resolve_pivot(pv, prims)) @ G
        elif pv == "each":
            for i in idxs:
                local[i] = mk(_prim_center(prims[i])) @ local[i]
        else:
            M = mk(_resolve_pivot(pv, [prims[i] for i in idxs]))
            for i in idxs:
                local[i] = M @ local[i]
    return [G @ L for L in local]


# ─────────────────────────── rendering ───────────────────────────
def _shade_from_normals(nx, ny, nz, gloss):
    d = nx * LIGHT[0] + ny * LIGHT[1] + nz * LIGHT[2]
    idx = np.full(d.shape, 2, np.int8)
    idx[d < 0.62] = 1
    idx[d > 0.86] = 3
    hi = 0.975 if not gloss else 0.94
    idx[d > hi] = 4
    return idx


def render_frame(spec, t=0.0):
    """Render one frame → (H,W,4) uint8 RGBA at SIZE."""
    prims = spec["prims"]
    pm = frame_transforms(spec, t)
    N = SIZE * SS
    label = np.zeros((N, N), np.int32)       # 0 = empty; else prim_index+1
    shade = np.zeros((N, N), np.int8)
    weights = []
    meta = []

    raster_prims = [(i, p) for i, p in enumerate(prims) if p["t"] != "px"]
    for i, p in raster_prims:
        kind, pts, w, ctr = prim_outline(p)
        M = pm[i]
        if p.get("rot"):
            M = M @ _affine(rot=p["rot"], pivot=ctr)
        if p.get("flipx"):
            M = M @ _affine(sx=-1, pivot=(16, 16))
        pts = _apply(M, pts) * SS
        img = Image.new("L", (N, N), 0)
        dr = ImageDraw.Draw(img)
        if kind == "poly":
            dr.polygon([tuple(q) for q in pts], fill=255)
        else:
            ww = max(1, int(round(w * SS)))
            dr.line([tuple(q) for q in pts], fill=255, width=ww, joint="curve")
            if p.get("cap", True):
                rr = ww / 2
                for q in (pts[0], pts[-1]):
                    dr.ellipse([q[0] - rr, q[1] - rr, q[0] + rr, q[1] + rr], fill=255)
        mask = np.asarray(img) > 127
        if not mask.any():
            continue
        if p.get("cut"):
            # erase what's below, and also hide later prims? no — cut only affects earlier
            if p.get("clip"):
                pass
            label[mask] = 0
            continue
        if p.get("clip") is not None:
            # only draw where the target earlier primitive exists
            tgt = p["clip"]
            mask &= (label == tgt + 1)
            if not mask.any():
                continue
        # shading
        if p.get("flat") or p["t"] == "line" and not p.get("lit"):
            sh = np.full(mask.shape, p.get("shade", 2), np.int8)
        else:
            dist = ndimage.distance_transform_edt(mask)
            ys, xs = np.nonzero(mask)
            bw = (xs.max() - xs.min() + 1) / SS
            bh = (ys.max() - ys.min() + 1) / SS
            puff = p.get("puff", min(bw, bh) / 2)
            R = max(1.0, puff * SS)
            x = np.clip(dist / R, 0, 1)
            h = R * np.sqrt(np.clip(1 - (1 - x) ** 2, 0, 1)) * p.get("height", 1.0)
            h = ndimage.gaussian_filter(h, 1.2)
            gy, gx = np.gradient(h)
            nz = np.ones_like(h)
            nn = np.sqrt(gx * gx + gy * gy + nz)
            sh = _shade_from_normals(-gx / nn, -gy / nn, nz / nn, p.get("gloss", False))
            sh = np.clip(sh + p.get("tone", 0), 0, 4).astype(np.int8)
        label[mask] = i + 1
        shade[mask] = sh[mask]
    # ── weighted-mode downsample
    combo = np.where(label > 0, label * 8 + shade.astype(np.int32), 0)
    blocks = combo.reshape(SIZE, SS, SIZE, SS).transpose(0, 2, 1, 3).reshape(SIZE * SIZE, SS * SS)
    wvec = np.ones(len(prims) + 2)
    for i, p in enumerate(prims):
        wvec[i + 1] = p.get("weight", 1.0)
    out_lab = np.zeros(SIZE * SIZE, np.int32)
    out_sh = np.zeros(SIZE * SIZE, np.int8)
    cov_need = SS * SS * 0.42
    for b in range(SIZE * SIZE):
        row = blocks[b]
        nz_ = row[row > 0]
        if len(nz_) == 0:
            continue
        labs = nz_ // 8
        # thin features: if a high-weight prim is present at all, let it compete with full coverage
        heavy = wvec[labs] > 1.5
        if len(nz_) < cov_need and not (heavy.sum() >= SS * SS * 0.18):
            continue
        vals, counts = np.unique(nz_, return_counts=True)
        score = counts * wvec[vals // 8]
        # also prefer the top-most prim when close
        best = vals[np.argmax(score + (vals // 8) * 1e-3)]
        out_lab[b] = best // 8
        out_sh[b] = best % 8
    lab = out_lab.reshape(SIZE, SIZE)
    sh = out_sh.reshape(SIZE, SIZE)

    # ── cleanup: remove lone shade pixels inside a single prim
    for _ in range(1):
        new = sh.copy()
        for y in range(1, SIZE - 1):
            for x in range(1, SIZE - 1):
                if lab[y, x] == 0:
                    continue
                nb = [(y - 1, x), (y + 1, x), (y, x - 1), (y, x + 1)]
                if all(lab[q] == lab[y, x] for q in nb):
                    ns = [sh[q] for q in nb]
                    if sh[y, x] not in ns:
                        vals, cnt = np.unique(ns, return_counts=True)
                        new[y, x] = vals[np.argmax(cnt)]
        sh = new

    # ── internal edge lines: lower prim pixels touching an `edge` prim get a dark line
    edge_px = []
    for y in range(SIZE):
        for x in range(SIZE):
            li = lab[y, x]
            if li == 0:
                continue
            for dy, dx in ((-1, 0), (1, 0), (0, -1), (0, 1)):
                yy, xx = y + dy, x + dx
                if 0 <= yy < SIZE and 0 <= xx < SIZE:
                    lj = lab[yy, xx]
                    if lj > li and prims[lj - 1].get("edge"):
                        edge_px.append((y, x))
                        break
    for y, x in edge_px:
        sh[y, x] = 0

    # ── colourize
    rgba = np.zeros((SIZE, SIZE, 4), np.uint8)
    for y in range(SIZE):
        for x in range(SIZE):
            li = lab[y, x]
            if li == 0:
                continue
            m = prims[li - 1].get("m", "gray")
            rgba[y, x, :3] = PALETTE[m][int(sh[y, x])]
            rgba[y, x, 3] = 255

    # ── pixel overrides (eyes, sparkles, tiny details) — transformed with their group
    for i, p in enumerate(prims):
        if p["t"] != "px":
            continue
        pts = _apply(pm[i], p["pts"])
        col = PALETTE[p.get("m", "black")][p.get("shade", 1)]
        for qx, qy in pts:
            xi, yi = int(math.floor(qx)), int(math.floor(qy))
            if 0 <= xi < SIZE and 0 <= yi < SIZE:
                rgba[yi, xi, :3] = col
                rgba[yi, xi, 3] = 255
                if lab[yi, xi] == 0:
                    lab[yi, xi] = len(prims) + 1  # treat as filled for outline

    # ── selective outer outline (4-neighbour), colour = darkest step of neighbour material
    opaque = rgba[:, :, 3] > 0
    outline = np.zeros_like(opaque)
    oc = np.zeros((SIZE, SIZE, 3), np.uint8)
    for y in range(SIZE):
        for x in range(SIZE):
            if opaque[y, x]:
                continue
            for dy, dx in ((1, 0), (0, 1), (-1, 0), (0, -1)):
                yy, xx = y + dy, x + dx
                if 0 <= yy < SIZE and 0 <= xx < SIZE and opaque[yy, xx]:
                    li = lab[yy, xx]
                    m = prims[li - 1].get("m", "gray") if 0 < li <= len(prims) else "black"
                    if prims[li - 1].get("t") == "px" if 0 < li <= len(prims) else False:
                        m = "black"
                    c = np.array(PALETTE[m][0], float)
                    # pull outline toward a shared ink colour so silhouettes read as a set
                    ink = np.array([34, 26, 46], float)
                    oc[y, x] = (0.55 * c + 0.45 * ink).astype(np.uint8)
                    outline[y, x] = True
                    break
    rgba[outline, :3] = oc[outline]
    rgba[outline, 3] = 255
    return rgba


def render_anim(spec, frames=None):
    frames = frames or spec.get("frames", FRAMES)
    if spec.get("anim", "none") == "none" and not spec.get("tracks"):
        return [render_frame(spec, 0.0)]
    return [render_frame(spec, i / frames) for i in range(frames)]


# ─────────────────────────── outputs ───────────────────────────
def upscale(rgba, k):
    return np.kron(rgba, np.ones((k, k, 1), np.uint8))


def save_outputs(spec, outdir: Path, scale=6):
    outdir.mkdir(parents=True, exist_ok=True)
    name = spec["name"]
    frames = render_anim(spec)
    Image.fromarray(frames[0], "RGBA").save(outdir / f"{name}.png")
    strip = np.concatenate(frames, axis=1)
    Image.fromarray(strip, "RGBA").save(outdir / f"{name}_sheet.png")
    if len(frames) > 1:
        imgs = [Image.fromarray(upscale(f, scale), "RGBA") for f in frames]
        imgs[0].save(outdir / f"{name}.gif", save_all=True, append_images=imgs[1:],
                     duration=spec.get("ms", 110), loop=0, disposal=2, transparency=0)
    return frames


def contact_sheet(specs, path: Path, scale=4, cols=10, frame_idx=0, bg=(236, 232, 224)):
    """Grid of sprites with labels — what an LLM 'looks at' to critique its work."""
    cell = SIZE * scale + 8
    lab_h = 14
    rows = math.ceil(len(specs) / cols)
    W, H = cols * cell, rows * (cell + lab_h)
    sheet = Image.new("RGB", (W, H), bg)
    dr = ImageDraw.Draw(sheet)
    try:
        font = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 11)
    except Exception:
        font = ImageFont.load_default()
    for k, spec in enumerate(specs):
        fr = render_anim(spec)
        f = fr[min(frame_idx, len(fr) - 1)]
        img = Image.fromarray(upscale(f, scale), "RGBA")
        cx, cy = (k % cols) * cell, (k // cols) * (cell + lab_h)
        # checker to reveal transparency/outline
        dr.rectangle([cx + 4, cy + 4, cx + 4 + SIZE * scale - 1, cy + 4 + SIZE * scale - 1], fill=(250, 248, 242))
        sheet.paste(img, (cx + 4, cy + 4), img)
        dr.text((cx + 4, cy + cell - 2), spec["name"], fill=(40, 40, 50), font=font)
    sheet.save(path)


def strip_sheet(specs, path: Path, scale=4, bg=(236, 232, 224)):
    """One row per sprite showing every animation frame — for reviewing motion."""
    rows = [(sp["name"], render_anim(sp)) for sp in specs]
    nmax = max(len(f) for _, f in rows)
    cw = SIZE * scale + 4
    W, H = 90 + nmax * cw, len(rows) * cw
    sheet = Image.new("RGB", (W, H), bg)
    dr = ImageDraw.Draw(sheet)
    for r, (name, frames) in enumerate(rows):
        dr.text((4, r * cw + cw // 2 - 6), name, fill=(40, 40, 50))
        for k, f in enumerate(frames):
            img = Image.fromarray(upscale(f, scale), "RGBA")
            x, y = 90 + k * cw, r * cw + 2
            dr.rectangle([x, y, x + SIZE * scale - 1, y + SIZE * scale - 1], fill=(250, 248, 242))
            sheet.paste(img, (x, y), img)
    sheet.save(path)


def palette_sheet(path: Path, k=16):
    names = list(PALETTE)
    img = Image.new("RGB", (5 * k + 90, len(names) * k), (255, 255, 255))
    dr = ImageDraw.Draw(img)
    for r, n in enumerate(names):
        for i, c in enumerate(PALETTE[n]):
            dr.rectangle([i * k, r * k, i * k + k - 1, r * k + k - 1], fill=c)
        dr.text((5 * k + 4, r * k + 2), n, fill=(0, 0, 0))
    img.save(path)


def load_specs(paths):
    specs = []
    for p in paths:
        data = json.loads(Path(p).read_text())
        specs += data if isinstance(data, list) else [data]
    return specs


def main(argv):
    import argparse
    ap = argparse.ArgumentParser(description="Sprite Factory")
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("render", help="render specs to PNG/GIF")
    r.add_argument("files", nargs="+")
    r.add_argument("--out", default="out")
    r.add_argument("--only", nargs="*")
    s = sub.add_parser("sheet", help="contact sheet for review")
    s.add_argument("files", nargs="+")
    s.add_argument("--out", default="sheet.png")
    s.add_argument("--scale", type=int, default=4)
    s.add_argument("--cols", type=int, default=10)
    s.add_argument("--frame", type=int, default=0)
    s.add_argument("--only", nargs="*")
    st = sub.add_parser("strips", help="animation frames per sprite, for review")
    st.add_argument("files", nargs="+")
    st.add_argument("--out", default="strips.png")
    st.add_argument("--scale", type=int, default=4)
    st.add_argument("--only", nargs="*")
    sub.add_parser("palette").add_argument("--out", default="palette.png")
    a = ap.parse_args(argv)
    if a.cmd == "palette":
        palette_sheet(Path(a.out)); return
    specs = load_specs(a.files)
    if getattr(a, "only", None):
        specs = [s for s in specs if s["name"] in a.only]
    if a.cmd == "render":
        for sp in specs:
            save_outputs(sp, Path(a.out))
        print(f"rendered {len(specs)} → {a.out}")
    elif a.cmd == "sheet":
        contact_sheet(specs, Path(a.out), a.scale, a.cols, a.frame)
        print(f"sheet {len(specs)} → {a.out}")
    elif a.cmd == "strips":
        strip_sheet(specs, Path(a.out), a.scale)
        print(f"strips {len(specs)} → {a.out}")


if __name__ == "__main__":
    main(sys.argv[1:])
