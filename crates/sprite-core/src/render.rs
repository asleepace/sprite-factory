//! Spec → RGBA frames. Mirrors factory.py step for step.

use crate::anim::frame_transforms;
use crate::geom::{affine, apply, mul, prim_outline, Outline};
use crate::palette::{Palette, INK};
use crate::raster::{edt, fill_polygon, gaussian, gradient, stroke, Mask};
use crate::spec::{Kind, Spec};
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
    let n = SIZE * SS;
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
        let ssf = SS as f64;
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
        let margin = 8usize;
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
        let hs = gaussian(&hf, cw, ch, 1.2);
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
    let ss2 = (SS * SS) as f64;
    let cov_need = ss2 * 0.42;
    let mut lab = vec![0i32; SIZE * SIZE];
    let mut sh = vec![0i32; SIZE * SIZE];
    let mut vals: Vec<i32> = Vec::with_capacity(SS * SS);
    for by in 0..SIZE {
        for bx in 0..SIZE {
            vals.clear();
            for yy in 0..SS {
                for xx in 0..SS {
                    let k = (by * SS + yy) * n + bx * SS + xx;
                    if label[k] > 0 {
                        vals.push(label[k] * 8 + shade[k] as i32);
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
            lab[by * SIZE + bx] = best / 8;
            sh[by * SIZE + bx] = best % 8;
        }
    }
    let at = |x: usize, y: usize| y * SIZE + x;

    // ── cleanup: lone shade pixels inside one prim take the majority neighbour shade
    let mut new = sh.clone();
    for y in 1..SIZE - 1 {
        for x in 1..SIZE - 1 {
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

    // ── internal edge lines
    let mut edge_px = Vec::new();
    for y in 0..SIZE {
        for x in 0..SIZE {
            let li = lab[at(x, y)];
            if li == 0 {
                continue;
            }
            for (dy, dx) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                let (yy, xx) = (y as i32 + dy, x as i32 + dx);
                if (0..SIZE as i32).contains(&yy) && (0..SIZE as i32).contains(&xx) {
                    let lj = lab[at(xx as usize, yy as usize)];
                    if lj > li && prims[(lj - 1) as usize].edge {
                        edge_px.push(at(x, y));
                        break;
                    }
                }
            }
        }
    }
    for k in edge_px {
        sh[k] = 0;
    }

    // ── colourize
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    for k in 0..SIZE * SIZE {
        let li = lab[k];
        if li == 0 {
            continue;
        }
        let c = pal.ramp(prims[(li - 1) as usize].m)[sh[k] as usize];
        rgba[k * 4..k * 4 + 3].copy_from_slice(&c);
        rgba[k * 4 + 3] = 255;
    }

    // ── pixel overrides
    let px_label = prims.len() as i32 + 1;
    for (i, p) in prims.iter().enumerate() {
        if p.kind != Kind::Px {
            continue;
        }
        let col = pal.ramp(p.m)[p.shade as usize];
        for q in &p.pts {
            let a = apply(&pm[i], *q);
            let (xi, yi) = (a[0].floor(), a[1].floor());
            if xi >= 0.0 && yi >= 0.0 && (xi as usize) < SIZE && (yi as usize) < SIZE {
                let k = at(xi as usize, yi as usize);
                rgba[k * 4..k * 4 + 3].copy_from_slice(&col);
                rgba[k * 4 + 3] = 255;
                if lab[k] == 0 {
                    lab[k] = px_label;
                }
            }
        }
    }

    // ── selective outer outline
    let opaque: Vec<bool> = (0..SIZE * SIZE).map(|k| rgba[k * 4 + 3] > 0).collect();
    let black = pal.index_of("black").unwrap();
    for y in 0..SIZE {
        for x in 0..SIZE {
            if opaque[at(x, y)] {
                continue;
            }
            for (dy, dx) in [(1i32, 0i32), (0, 1), (-1, 0), (0, -1)] {
                let (yy, xx) = (y as i32 + dy, x as i32 + dx);
                if !((0..SIZE as i32).contains(&yy) && (0..SIZE as i32).contains(&xx)) {
                    continue;
                }
                let q = at(xx as usize, yy as usize);
                if !opaque[q] {
                    continue;
                }
                let li = lab[q];
                let m = if li > 0 && (li as usize) <= prims.len() { prims[(li - 1) as usize].m } else { black };
                let c = pal.ramp(m)[0];
                let k = at(x, y);
                for ch in 0..3 {
                    rgba[k * 4 + ch] = (0.55 * c[ch] as f64 + 0.45 * INK[ch]) as u8;
                }
                rgba[k * 4 + 3] = 255;
                break;
            }
        }
    }
    Frame(rgba)
}

pub fn render_anim(spec: &Spec, pal: &Palette, opts: &RenderOpts) -> Vec<Frame> {
    if !spec.is_animated() {
        return vec![render_frame(spec, 0.0, pal, opts)];
    }
    let f = spec.frames.max(1);
    (0..f).map(|i| render_frame(spec, i as f64 / f as f64, pal, opts)).collect()
}
