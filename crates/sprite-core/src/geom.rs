//! Shapes → point lists, and 2D affine matrices.

use crate::spec::{Kind, Prim, Pt};

/// Row-major 3×3 affine matrix.
pub type Mat = [[f64; 3]; 3];

pub const I3: Mat = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

pub fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    r
}

/// translate(-pivot) → scale → rotate (degrees, clockwise on screen) → translate(pivot + d)
pub fn affine(rot: f64, sx: f64, sy: f64, dx: f64, dy: f64, pivot: Pt) -> Mat {
    let [px, py] = pivot;
    let (s, c) = rot.to_radians().sin_cos();
    let t1 = [[1.0, 0.0, -px], [0.0, 1.0, -py], [0.0, 0.0, 1.0]];
    let sc = [[sx, 0.0, 0.0], [0.0, sy, 0.0], [0.0, 0.0, 1.0]];
    let r = [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]];
    let t2 = [[1.0, 0.0, px + dx], [0.0, 1.0, py + dy], [0.0, 0.0, 1.0]];
    mul(&t2, &mul(&r, &mul(&sc, &t1)))
}

pub fn apply(m: &Mat, p: Pt) -> Pt {
    [m[0][0] * p[0] + m[0][1] * p[1] + m[0][2], m[1][0] * p[0] + m[1][1] * p[1] + m[1][2]]
}

fn linspace(a: f64, b: f64, n: usize, endpoint: bool) -> Vec<f64> {
    if n == 1 {
        return vec![a];
    }
    let div = if endpoint { (n - 1) as f64 } else { n as f64 };
    (0..n).map(|i| a + (b - a) * i as f64 / div).collect()
}

fn ellipse_pts(cx: f64, cy: f64, rx: f64, ry: f64, a0: f64, a1: f64, n: usize) -> Vec<Pt> {
    let endpoint = (a1 - a0).rem_euclid(360.0) != 0.0;
    linspace(a0, a1, n, endpoint)
        .into_iter()
        .map(|d| {
            let t = d.to_radians();
            [cx + rx * t.cos(), cy + ry * t.sin()]
        })
        .collect()
}

fn rrect_pts(x0: f64, y0: f64, x1: f64, y1: f64, r: f64) -> Vec<Pt> {
    let r = r.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0).max(0.0);
    if r < 1e-3 {
        return vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
    }
    let mut pts = Vec::with_capacity(40);
    for (cx, cy, a) in [(x1 - r, y0 + r, -90.0), (x1 - r, y1 - r, 0.0), (x0 + r, y1 - r, 90.0), (x0 + r, y0 + r, 180.0)] {
        for d in linspace(a, a + 90.0, 10, true) {
            let t: f64 = d.to_radians();
            pts.push([cx + r * t.cos(), cy + r * t.sin()]);
        }
    }
    pts
}

pub fn chaikin(mut pts: Vec<Pt>, it: usize) -> Vec<Pt> {
    for _ in 0..it {
        let n = pts.len();
        let mut out = Vec::with_capacity(n * 2);
        for i in 0..n {
            let a = pts[i];
            let b = pts[(i + 1) % n];
            out.push([0.75 * a[0] + 0.25 * b[0], 0.75 * a[1] + 0.25 * b[1]]);
            out.push([0.25 * a[0] + 0.75 * b[0], 0.25 * a[1] + 0.75 * b[1]]);
        }
        pts = out;
    }
    pts
}

fn mean(pts: &[Pt]) -> Pt {
    let n = pts.len().max(1) as f64;
    let (sx, sy) = pts.iter().fold((0.0, 0.0), |(x, y), p| (x + p[0], y + p[1]));
    [sx / n, sy / n]
}

pub enum Outline {
    Poly(Vec<Pt>),
    Line(Vec<Pt>, f64),
}

/// Shape outline in sprite space (before transforms) plus its local centre.
pub fn prim_outline(p: &Prim) -> (Outline, Pt) {
    match p.kind {
        Kind::Ellipse => {
            let [cx, cy] = p.c;
            (Outline::Poly(ellipse_pts(cx, cy, p.r[0], p.r[1], 0.0, 360.0, 72)), p.c)
        }
        Kind::Arc => {
            let [cx, cy] = p.c;
            let mut pts = ellipse_pts(cx, cy, p.r[0], p.r[1], p.a0, p.a1, 48);
            if p.pie {
                pts.insert(0, p.c);
            }
            (Outline::Poly(pts), p.c)
        }
        Kind::Rect => {
            let [x0, y0, x1, y1] = p.p;
            (Outline::Poly(rrect_pts(x0, y0, x1, y1, p.round)), [(x0 + x1) / 2.0, (y0 + y1) / 2.0])
        }
        Kind::Poly => {
            let pts = chaikin(p.pts.clone(), p.smooth);
            let c = mean(&pts);
            (Outline::Poly(pts), c)
        }
        Kind::Star | Kind::Ngon => {
            let [cx, cy] = p.c;
            let r1 = p.r[0];
            let star = p.kind == Kind::Star;
            let r2 = if star { p.r2.unwrap_or(r1 * 0.45) } else { r1 };
            let k = if star { p.n * 2 } else { p.n };
            let a0 = p.a.to_radians();
            let pts: Vec<Pt> = (0..k)
                .map(|i| {
                    let rr = if !star || i % 2 == 0 { r1 } else { r2 };
                    let a = a0 + i as f64 * 2.0 * std::f64::consts::PI / k as f64;
                    [cx + rr * a.cos(), cy + rr * a.sin()]
                })
                .collect();
            (Outline::Poly(chaikin(pts, p.smooth)), p.c)
        }
        Kind::Line => (Outline::Line(p.pts.clone(), p.w), mean(&p.pts)),
        Kind::Px => (Outline::Poly(p.pts.clone()), mean(&p.pts)),
    }
}

pub fn outline_pts(o: &Outline) -> &[Pt] {
    match o {
        Outline::Poly(p) | Outline::Line(p, _) => p,
    }
}

/// [x0, y0, x1, y1] of a set of prims' raw outline points (px uses its pixel points).
pub fn bbox_of<'a>(prims: impl IntoIterator<Item = &'a Prim>) -> [f64; 4] {
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for p in prims {
        let (o, _) = prim_outline(p);
        for q in outline_pts(&o) {
            b[0] = b[0].min(q[0]);
            b[1] = b[1].min(q[1]);
            b[2] = b[2].max(q[0]);
            b[3] = b[3].max(q[1]);
        }
    }
    b
}

pub fn prim_center(p: &Prim) -> Pt {
    if p.kind == Kind::Px {
        return mean(&p.pts);
    }
    let b = bbox_of([p]);
    [(b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0]
}
