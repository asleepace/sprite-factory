//! The locked palette: 24 materials × 5 hue-shifted shades (0 outline … 4 highlight).

pub type Rgb = [u8; 3];

/// Base colours (ramp index 2), in display order.
pub const BASES: [(&str, &str); 24] = [
    ("red", "#d8343f"),
    ("orange", "#ec7a2c"),
    ("yellow", "#f5cf3a"),
    ("gold", "#d9a521"),
    ("lime", "#8ccf3d"),
    ("green", "#3fa548"),
    ("forest", "#2c7a46"),
    ("teal", "#2aa6a0"),
    ("sky", "#5cb8ec"),
    ("blue", "#3a6fd6"),
    ("navy", "#2e3d8f"),
    ("purple", "#8a4ec4"),
    ("pink", "#ef7fb0"),
    ("brown", "#8a5232"),
    ("wood", "#b97c45"),
    ("tan", "#d9b07a"),
    ("skin", "#f0b98c"),
    ("cream", "#f2e6c9"),
    ("white", "#eef1f5"),
    ("gray", "#9aa0a8"),
    ("steel", "#6f8299"),
    ("charcoal", "#4a4a57"),
    ("black", "#2a2833"),
    ("glass", "#a8e0f0"),
];

/// Shared ink the outer outline is pulled toward, so silhouettes read as one set.
pub const INK: [f64; 3] = [34.0, 26.0, 46.0];

#[derive(Debug, Clone)]
pub struct Palette {
    names: Vec<&'static str>,
    ramps: Vec<[Rgb; 5]>,
}

impl Default for Palette {
    fn default() -> Self {
        Self::new()
    }
}

impl Palette {
    pub fn new() -> Self {
        let mut names = Vec::new();
        let mut ramps = Vec::new();
        for (n, hex) in BASES {
            names.push(n);
            ramps.push(ramp(hex));
        }
        Palette { names, ramps }
    }

    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| *n == name)
    }

    pub fn ramp(&self, idx: usize) -> &[Rgb; 5] {
        &self.ramps[idx]
    }

    pub fn names(&self) -> &[&'static str] {
        &self.names
    }

    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &[Rgb; 5])> {
        self.names.iter().copied().zip(self.ramps.iter())
    }

    pub fn color_count(&self) -> usize {
        self.ramps.len() * 5
    }
}

fn hex(h: &str) -> [f64; 3] {
    let h = h.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap() as f64 / 255.0;
    [p(0), p(2), p(4)]
}

/// Python's float `%` (result takes the sign of the divisor).
fn pymod(a: f64, b: f64) -> f64 {
    let r = a % b;
    if r != 0.0 && (r < 0.0) != (b < 0.0) {
        r + b
    } else {
        r
    }
}

/// Python's round(): half to even.
fn pyround(x: f64) -> f64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 {
        2.0 * (x / 2.0).round()
    } else {
        r
    }
}

fn rgb_to_hls(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let maxc = r.max(g).max(b);
    let minc = r.min(g).min(b);
    let sumc = maxc + minc;
    let rangec = maxc - minc;
    let l = sumc / 2.0;
    if minc == maxc {
        return (0.0, l, 0.0);
    }
    let s = if l <= 0.5 { rangec / sumc } else { rangec / (2.0 - maxc - minc) };
    let rc = (maxc - r) / rangec;
    let gc = (maxc - g) / rangec;
    let bc = (maxc - b) / rangec;
    let h = if r == maxc {
        bc - gc
    } else if g == maxc {
        2.0 + rc - bc
    } else {
        4.0 + gc - rc
    };
    (pymod(h / 6.0, 1.0), l, s)
}

fn hls_to_rgb(h: f64, l: f64, s: f64) -> (f64, f64, f64) {
    if s == 0.0 {
        return (l, l, l);
    }
    let m2 = if l <= 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let m1 = 2.0 * l - m2;
    let v = |hue: f64| {
        let hue = pymod(hue, 1.0);
        if hue < 1.0 / 6.0 {
            m1 + (m2 - m1) * hue * 6.0
        } else if hue < 0.5 {
            m2
        } else if hue < 2.0 / 3.0 {
            m1 + (m2 - m1) * (2.0 / 3.0 - hue) * 6.0
        } else {
            m1
        }
    };
    (v(h + 1.0 / 3.0), v(h), v(h - 1.0 / 3.0))
}

/// 5-step ramp: shadows drift toward blue-violet, highlights toward warm yellow.
fn ramp(base_hex: &str) -> [Rgb; 5] {
    let [r, g, b] = hex(base_hex);
    let (h, l, s) = rgb_to_hls(r, g, b);
    let steps: [(f64, f64, f64); 5] = [
        (-0.34, -1.0, 1.10),
        (-0.15, -0.5, 1.05),
        (0.00, 0.0, 1.00),
        (0.12, 0.5, 0.95),
        (0.24, 1.0, 0.80),
    ];
    let mut out = [[0u8; 3]; 5];
    for (k, (dl, dh, ds)) in steps.into_iter().enumerate() {
        let mut hh = h;
        if dh < 0.0 {
            hh = if (0.70 - h).abs() < 0.5 { h + (0.70 - h) * 0.12 * -dh } else { h - 0.04 * -dh };
        } else if dh > 0.0 {
            hh = if (0.14 - h).abs() < 0.5 { h + (0.14 - h) * 0.10 * dh } else { h + 0.03 * dh };
        }
        let mut ll = (l + dl).clamp(0.06, 0.96);
        let ss = (s * ds).min(1.0);
        if l > 0.85 && dl < 0.0 {
            ll = (l + dl * 1.25).clamp(0.06, 0.96);
        }
        let (rr, gg, bb) = hls_to_rgb(pymod(hh, 1.0), ll, ss);
        out[k] = [pyround(rr * 255.0) as u8, pyround(gg * 255.0) as u8, pyround(bb * 255.0) as u8];
    }
    out
}
