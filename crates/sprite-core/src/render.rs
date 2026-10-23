//! Spec → RGBA frames. Mirrors factory.py step for step.

use crate::anim::frame_transforms;
use crate::geom::{affine, apply, mul, prim_outline, Mat, Outline};
use crate::palette::Palette;
use crate::raster::{edt, fill_polygon, gaussian, gradient, stroke, Mask};
use crate::spec::{Kind, Prim, Spec};
use crate::{SIZE, SS};

/// One 32×32 RGBA frame, row-major.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame(pub Vec<u8>);

impl Frame {
    pub fn alpha(&self, x: usize, y: usize) -> u8 {
        self.0[(y * SIZE + x) * 4 + 3]
    }
}

/// Rasterizer tuning; the defaults were chosen to match the Python renderer's output.
#[derive(Debug, Clone, Copy)]
pub struct RenderOpts {
    /// how far (in supersampled px) polygon fills spill past the exact edge (PIL draws a 1px outline too)
    pub poly_grow: f64,
}

impl Default for RenderOpts {
    fn default() -> Self {
        RenderOpts { poly_grow: 0.5 }
    }
}

fn light() -> [f64; 3] {
    let l = [-0.55f64, -0.75, 0.95];
    let n = (l[0] * l[0] + l[1] * l[1] + l[2] * l[2]).sqrt();
    [l[0] / n, l[1] / n, l[2] / n]
}

#[inline]
fn shade_from_normal(nx: f64, ny: f64, nz: f64, l: &[f64; 3], gloss: bool) -> i32 {
    let d = nx * l[0] + ny * l[1] + nz * l[2];
    let hi = if gloss { 0.94 } else { 0.975 };
    if d > hi {
        4
    } else if d > 0.86 {
        3
    } else if d < 0.62 {
        1
    } else {
        2
    }
}

pub fn render_frame(spec: &Spec, t: f64, pal: &Palette, opts: &RenderOpts) -> Frame {
    Frame(render_sized(spec, t, pal, opts, SIZE, SS))
}

/// A high-definition frame: the same spec drawn on a `size`×`size` canvas instead of 32×32.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdFrame {
    pub size: usize,
    pub px: Vec<u8>,
}

/// Supported HD sizes: multiples of 32 from 64 to 1024.
pub fn hd_size_ok(size: usize) -> bool {
    (64..=1024).contains(&size) && size.is_multiple_of(32)
}

/// Draw the spec at `size`×`size` (a multiple of 32). Curves come from the vector shapes, so the
/// result is genuinely higher resolution, not an upscale. Shading bands, palette and outline style
/// match the 32px sprite; outlines and edge lines get thinner relative to the body.
pub fn render_frame_hd(spec: &Spec, t: f64, pal: &Palette, opts: &RenderOpts, size: usize) -> HdFrame {
    assert!(hd_size_ok(size), "HD size must be a multiple of 32 between 64 and 1024");
    HdFrame { size, px: render_sized(spec, t, pal, opts, size, 2) }
}

pub fn render_anim_hd(spec: &Spec, pal: &Palette, opts: &RenderOpts, size: usize) -> Vec<HdFrame> {
    if !spec.is_animated() {
        return vec![render_frame_hd(spec, 0.0, pal, opts, size)];
    }
    let f = spec.frames.max(1);
    (0..f).map(|i| render_frame_hd(spec, i as f64 / f as f64, pal, opts, size)).collect()
}

/// Core renderer: output `size`×`size` RGBA, supersampled `ss`×. Spec coordinates are always in
/// 32px sprite units; `k` = output px per sprite px, `cs` = supersampled px per sprite px.
fn render_sized(spec: &Spec, t: f64, pal: &Palette, opts: &RenderOpts, size: usize, ss: usize) -> Vec<u8> {
    let k = size / 32;
    let n = size * ss;
    let prims = &spec.prims;
    let pm = frame_transforms(spec, t);
    let lightv = light();
    let mut label = vec![0i32; n * n]; // 0 empty, else prim index + 1
    let mut shade = vec![0i8; n * n];

    for (i, p) in prims.iter().enumerate() {
        if p.kind == Kind::Px {
            continue;
        }
        let (outline, ctr) = prim_outline(p);
        let mut m = pm[i];
        if p.rot != 0.0 {
            m = mul(&m, &affine(p.rot, 1.0, 1.0, 0.0, 0.0, ctr));
        }
        if p.flipx {
            m = mul(&m, &affine(0.0, -1.0, 1.0, 0.0, 0.0, [16.0, 16.0]));
        }
        let ssf = (k * ss) as f64; // coordinate scale: sprite px → supersampled px
        let mut mask = Mask::new(n);
        match &outline {
            Outline::Poly(pts) => {
                let tp: Vec<_> = pts.iter().map(|q| {
                    let a = apply(&m, *q);
                    [a[0] * ssf, a[1] * ssf]
                }).collect();
                fill_polygon(&mut mask, &tp, opts.poly_grow);
            }
            Outline::Line(pts, w) => {
                let tp: Vec<_> = pts.iter().map(|q| {
                    let a = apply(&m, *q);
                    [a[0] * ssf, a[1] * ssf]
                }).collect();
                let ww = (w * ssf).round().max(1.0);
                stroke(&mut mask, &tp, ww, p.cap);
            }
        }
        if !mask.any() {
            continue;
        }
        if p.cut {
            for (k, &on) in mask.data.iter().enumerate() {
                if on {
                    label[k] = 0;
                }
            }
            continue;
        }
        if let Some(tgt) = p.clip {
            let want = tgt as i32 + 1;
            let mut any = false;
            for (on, l) in mask.data.iter_mut().zip(label.iter()) {
                if *on {
                    if *l != want {
                        *on = false;
                    } else {
                        any = true;
                    }
                }
            }
            if !any {
                continue;
            }
        }
        let flat = p.flat || (p.kind == Kind::Line && !p.lit);
        if flat {
            for ((on, l), sh) in mask.data.iter().zip(label.iter_mut()).zip(shade.iter_mut()) {
                if *on {
                    *l = i as i32 + 1;
                    *sh = p.shade as i8;
                }
            }
            continue;
        }
        // lit: inflate the mask into a height field. Work in a window around the mask with an
        // 8px margin: everything outside is zero height, so EDT/blur/gradient match the full image.
        let Some([bx0, by0, bx1, by1]) = mask.bbox() else { continue };
        let bw = (bx1 - bx0 + 1) as f64 / ssf;
        let bh = (by1 - by0 + 1) as f64 / ssf;
        let sigma = 1.2 * ssf / 8.0;
        let margin = 8usize.max((4.0 * sigma) as usize + 4);
        let cx0 = bx0.saturating_sub(margin);
        let cy0 = by0.saturating_sub(margin);
        let cx1 = (bx1 + margin + 1).min(n);
        let cy1 = (by1 + margin + 1).min(n);
        let (cw, ch) = (cx1 - cx0, cy1 - cy0);
        let mut cm = vec![false; cw * ch];
        for y in 0..ch {
            for x in 0..cw {
                cm[y * cw + x] = mask.get(cx0 + x, cy0 + y);
            }
        }
        let puff = p.puff.unwrap_or(bw.min(bh) / 2.0);
        let r = (puff * ssf).max(1.0);
        let dist = edt(&cm, cw, ch);
        let hf: Vec<f64> = dist
            .iter()
            .map(|&d| {
                let x = (d / r).clamp(0.0, 1.0);
                r * (1.0 - (1.0 - x).powi(2)).clamp(0.0, 1.0).sqrt() * p.height
            })
            .collect();
        let hs = gaussian(&hf, cw, ch, sigma);
        let (gy, gx) = gradient(&hs, cw, ch);
        for y in 0..ch {
            for x in 0..cw {
                let k = y * cw + x;
                if !cm[k] {
                    continue;
                }
                let (gxv, gyv) = (gx[k], gy[k]);
                let nn = (gxv * gxv + gyv * gyv + 1.0).sqrt();
                let s = (shade_from_normal(-gxv / nn, -gyv / nn, 1.0 / nn, &lightv, p.gloss) + p.tone).clamp(0, 4);
                let gk = (cy0 + y) * n + cx0 + x;
                label[gk] = i as i32 + 1;
                shade[gk] = s as i8;
            }
        }
    }

    // ── weighted-mode downsample
    let mut wvec = vec![1.0f64; prims.len() + 2];
    for (i, p) in prims.iter().enumerate() {
        wvec[i + 1] = p.weight;
    }
    let ss2 = (ss * ss) as f64;
    let cov_need = ss2 * 0.42;
    let mut lab = vec![0i32; size * size];
    let mut sh = vec![0i32; size * size];
    let mut vals: Vec<i32> = Vec::with_capacity(ss * ss);
    for by in 0..size {
        for bx in 0..size {
            vals.clear();
            for yy in 0..ss {
                for xx in 0..ss {
                    let q = (by * ss + yy) * n + bx * ss + xx;
                    if label[q] > 0 {
                        vals.push(label[q] * 8 + shade[q] as i32);
                    }
                }
            }
            if vals.is_empty() {
                continue;
            }
            let heavy = vals.iter().filter(|&&v| wvec[(v / 8) as usize] > 1.5).count() as f64;
            if (vals.len() as f64) < cov_need && heavy < ss2 * 0.18 {
                continue;
            }
            vals.sort_unstable();
            let mut best = vals[0];
            let mut best_score = f64::NEG_INFINITY;
            let mut i = 0;
            while i < vals.len() {
                let v = vals[i];
                let mut j = i;
                while j < vals.len() && vals[j] == v {
                    j += 1;
                }
                let score = (j - i) as f64 * wvec[(v / 8) as usize] + (v / 8) as f64 * 1e-3;
                if score > best_score {
                    best_score = score;
                    best = v;
                }
                i = j;
            }
            lab[by * size + bx] = best / 8;
            sh[by * size + bx] = best % 8;
        }
    }
    let at = |x: usize, y: usize| y * size + x;

    // ── cleanup: lone shade pixels inside one prim take the majority neighbour shade
    let mut new = sh.clone();
    for y in 1..size - 1 {
        for x in 1..size - 1 {
            let l = lab[at(x, y)];
            if l == 0 {
                continue;
            }
            let nb = [at(x, y - 1), at(x, y + 1), at(x - 1, y), at(x + 1, y)];
            if nb.iter().all(|&q| lab[q] == l) {
                let ns: Vec<i32> = nb.iter().map(|&q| sh[q]).collect();
                if !ns.contains(&sh[at(x, y)]) {
                    let mut best = (0usize, i32::MAX);
                    for &v in &ns {
                        let c = ns.iter().filter(|&&u| u == v).count();
                        if c > best.0 || (c == best.0 && v < best.1) {
                            best = (c, v);
                        }
                    }
                    new[at(x, y)] = best.1;
                }
            }
        }
    }
    let mut sh = new;

    // ── internal edge lines: lower-prim pixels within `e` px of an `edge` prim go dark
    let e = (k as f64 * 0.5).round().max(1.0) as i32;
    let nbhd = neighbourhood(e);
    let mut edge_px = Vec::new();
    for y in 0..size {
        for x in 0..size {
            let li = lab[at(x, y)];
            if li == 0 {
                continue;
            }
            for &(dy, dx) in &nbhd {
                let (yy, xx) = (y as i32 + dy, x as i32 + dx);
                if (0..size as i32).contains(&yy) && (0..size as i32).contains(&xx) {
                    let lj = lab[at(xx as usize, yy as usize)];
                    if lj > li && prims[(lj - 1) as usize].edge {
                        edge_px.push(at(x, y));
                        break;
                    }
                }
            }
        }
    }
    for q in edge_px {
        sh[q] = 0;
    }

    // ── colourize
    let mut rgba = vec![0u8; size * size * 4];
    for q in 0..size * size {
        let li = lab[q];
        if li == 0 {
            continue;
        }
        let c = pal.ramp(prims[(li - 1) as usize].m)[sh[q] as usize];
        rgba[q * 4..q * 4 + 3].copy_from_slice(&c);
        rgba[q * 4 + 3] = 255;
    }

    // ── pixel overrides (eyes, glints): one pixel at 32px; a round dot of the same footprint in HD.
    // px pixels are drawn as rigid stamps (see `px_offsets`), so a pattern never tears.
    let px_label = prims.len() as i32 + 1;
    let offsets = px_offsets(prims, &pm, k);
    let (ki, kf) = (k as i64, k as f64);
    let pad: i64 = if k > 1 { 1 } else { 0 };
    for (i, p) in prims.iter().enumerate() {
        if p.kind != Kind::Px {
            continue;
        }
        let col = pal.ramp(p.m)[p.shade as usize];
        for (qp, d) in p.pts.iter().zip(&offsets[i]) {
            // output-pixel top-left of this sprite pixel; offsets are whole output pixels
            let x0 = ((qp[0].floor() + d[0]) * kf).round() as i64;
            let y0 = ((qp[1].floor() + d[1]) * kf).round() as i64;
            if x0 + ki <= 0 || y0 + ki <= 0 || x0 >= size as i64 || y0 >= size as i64 {
                continue;
            }
            let (cx, cy) = (x0 as f64 + kf / 2.0, y0 as f64 + kf / 2.0);
            let rad = kf * 0.62;
            let xs = (x0 - pad).max(0) as usize..(x0 + ki + pad).min(size as i64) as usize;
            let ys = (y0 - pad).max(0) as usize..(y0 + ki + pad).min(size as i64) as usize;
            for y in ys {
                for x in xs.clone() {
                    if k > 1 {
                        let (dx, dy) = (x as f64 + 0.5 - cx, y as f64 + 0.5 - cy);
                        if dx * dx + dy * dy > rad * rad {
                            continue;
                        }
                    }
                    let q = at(x, y);
                    rgba[q * 4..q * 4 + 3].copy_from_slice(&col);
                    rgba[q * 4 + 3] = 255;
                    if lab[q] == 0 {
                        lab[q] = px_label;
                    }
                }
            }
        }
    }

    // ── selective outer outline, `o` px thick (1 at 32px), coloured from the nearest shape
    let o = (k as f64 * 0.6).round().max(1.0) as i32;
    let ring = neighbourhood(o);
    let opaque: Vec<bool> = (0..size * size).map(|q| rgba[q * 4 + 3] > 0).collect();
    let black = pal.index_of("black").unwrap();
    for y in 0..size {
        for x in 0..size {
            if opaque[at(x, y)] {
                continue;
            }
            for &(dy, dx) in &ring {
                let (yy, xx) = (y as i32 + dy, x as i32 + dx);
                if !((0..size as i32).contains(&yy) && (0..size as i32).contains(&xx)) {
                    continue;
                }
                let q = at(xx as usize, yy as usize);
                if !opaque[q] {
                    continue;
                }
                let li = lab[q];
                let m = if li > 0 && (li as usize) <= prims.len() { prims[(li - 1) as usize].m } else { black };
                let d = at(x, y);
                rgba[d * 4..d * 4 + 3].copy_from_slice(&pal.outline(m));
                rgba[d * 4 + 3] = 255;
                break;
            }
        }
    }
    rgba
}

/// This frame's offset for every px point, in sprite px, rounded to whole output pixels
/// (`1/k` sprite px; whole sprite pixels at 32px). Indexed `[prim][point]`; empty for non-px prims.
///
/// px pixels move as rigid **stamps** so a pattern (an eye, a heart) never tears under scale or
/// rotation. A stamp is a cluster of 8-connected px pixels that share a group and this frame's
/// transform: a heart and the glint drawn on it are one stamp, while two eyes are two stamps that
/// each follow their own spot on a turning, squashing head. Each stamp moves by where the
/// transform carries its anchor (the mean of its points, the same convention pivots use).
fn px_offsets(prims: &[Prim], pm: &[Mat], k: usize) -> Vec<Vec<[f64; 2]>> {
    // every px point: (prim, point index, cell)
    let mut pts: Vec<(usize, usize, [i64; 2])> = Vec::new();
    for (i, p) in prims.iter().enumerate().filter(|(_, p)| p.kind == Kind::Px) {
        for (j, q) in p.pts.iter().enumerate() {
            pts.push((i, j, [q[0].floor() as i64, q[1].floor() as i64]));
        }
    }
    // union-find over points that touch and share group + transform
    let mut parent: Vec<usize> = (0..pts.len()).collect();
    fn find(parent: &mut [usize], mut a: usize) -> usize {
        while parent[a] != a {
            parent[a] = parent[parent[a]];
            a = parent[a];
        }
        a
    }
    let same = |a: usize, b: usize| prims[a].g == prims[b].g && pm[a] == pm[b];
    for a in 0..pts.len() {
        for b in a + 1..pts.len() {
            let (ca, cb) = (pts[a].2, pts[b].2);
            if (ca[0] - cb[0]).abs() <= 1 && (ca[1] - cb[1]).abs() <= 1 && same(pts[a].0, pts[b].0) {
                let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                parent[ra] = rb;
            }
        }
    }
    // anchor per stamp → offset
    let mut sum: Vec<([f64; 2], usize)> = vec![([0.0; 2], 0); pts.len()];
    for a in 0..pts.len() {
        let r = find(&mut parent, a);
        let q = prims[pts[a].0].pts[pts[a].1];
        sum[r].0[0] += q[0];
        sum[r].0[1] += q[1];
        sum[r].1 += 1;
    }
    let kf = k as f64;
    let mut out: Vec<Vec<[f64; 2]>> = prims.iter().map(|p| if p.kind == Kind::Px { vec![[0.0; 2]; p.pts.len()] } else { vec![] }).collect();
    for a in 0..pts.len() {
        let r = find(&mut parent, a);
        let (s, n) = sum[r];
        let anchor = [s[0] / n as f64, s[1] / n as f64];
        let to = apply(&pm[pts[a].0], anchor);
        out[pts[a].0][pts[a].1] = [((to[0] - anchor[0]) * kf).round() / kf, ((to[1] - anchor[1]) * kf).round() / kf];
    }
    out
}

/// Offsets within radius `r`, nearest first. r = 1 is the classic 4-neighbourhood (down, right,
/// up, left), which keeps 32px output identical to the original renderer.
fn neighbourhood(r: i32) -> Vec<(i32, i32)> {
    if r <= 1 {
        return vec![(1, 0), (0, 1), (-1, 0), (0, -1)];
    }
    let mut v: Vec<(i32, i32)> = Vec::new();
    for dy in -r..=r {
        for dx in -r..=r {
            if (dx, dy) != (0, 0) && dx * dx + dy * dy <= r * r {
                v.push((dy, dx));
            }
        }
    }
    v.sort_by_key(|&(dy, dx)| dy * dy + dx * dx);
    v
}

pub fn render_anim(spec: &Spec, pal: &Palette, opts: &RenderOpts) -> Vec<Frame> {
    if !spec.is_animated() {
        return vec![render_frame(spec, 0.0, pal, opts)];
    }
    let f = spec.frames.max(1);
    (0..f).map(|i| render_frame(spec, i as f64 / f as f64, pal, opts)).collect()
}
