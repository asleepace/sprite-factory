//! Sprite specs. The JSON format is deliberately loose (numbers where ints are expected,
//! `r` as a number or a pair, extra fields kept), so parsing mirrors Python dict semantics:
//! missing → default, truthiness for flags, numbers coerced.

use serde_json::{Map, Value};

use crate::palette::Palette;

pub type Pt = [f64; 2];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ellipse,
    Rect,
    Poly,
    Arc,
    Star,
    Ngon,
    Line,
    Px,
}

impl Kind {
    pub const ALL: [&'static str; 8] = ["ellipse", "rect", "poly", "arc", "star", "ngon", "line", "px"];

    fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "ellipse" => Kind::Ellipse,
            "rect" => Kind::Rect,
            "poly" => Kind::Poly,
            "arc" => Kind::Arc,
            "star" => Kind::Star,
            "ngon" => Kind::Ngon,
            "line" => Kind::Line,
            "px" => Kind::Px,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Prim {
    pub kind: Kind,
    /// palette index
    pub m: usize,
    pub c: Pt,
    pub r: Pt,
    pub r2: Option<f64>,
    pub n: usize,
    pub a: f64,
    pub a0: f64,
    pub a1: f64,
    pub pie: bool,
    pub p: [f64; 4],
    pub round: f64,
    pub pts: Vec<Pt>,
    pub w: f64,
    pub smooth: usize,
    pub puff: Option<f64>,
    pub height: f64,
    pub flat: bool,
    pub shade: i32,
    pub tone: i32,
    pub gloss: bool,
    pub edge: bool,
    pub weight: f64,
    pub rot: f64,
    pub flipx: bool,
    pub cut: bool,
    pub clip: Option<usize>,
    pub lit: bool,
    pub cap: bool,
    pub g: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Pivot {
    Center,
    Bottom,
    Top,
    Each,
    At(Pt),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prop {
    Dx,
    Dy,
    Rot,
    Sx,
    Sy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wave {
    Sin,
    Abs,
    Abs2,
    Saw,
    Square,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub g: Option<String>, // None = whole sprite ("*")
    pub prop: Prop,
    pub amp: f64,
    pub wave: Wave,
    pub phase: f64,
    pub pivot: Pivot,
}

#[derive(Debug, Clone)]
pub struct Spec {
    pub name: String,
    pub cat: Option<String>,
    pub anim: String,
    pub frames: usize,
    pub ms: u32,
    /// per-frame hold, in units of `ms` (frame i shows for `ms * hold[i]`); missing entries are 1
    pub hold: Vec<u32>,
    /// round every dx/dy (preset and track) to whole sprite pixels, for crisp stepped motion
    pub snap: bool,
    pub tracks: Vec<Track>,
    pub prims: Vec<Prim>,
    /// the original JSON, kept for round-tripping
    pub raw: Value,
}

// ───────────── lenient accessors (Python dict semantics) ─────────────
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|x| x != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn num(o: &Map<String, Value>, k: &str) -> Option<f64> {
    o.get(k).and_then(|v| match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    })
}

fn pt(v: &Value) -> Option<Pt> {
    let a = v.as_array()?;
    Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?])
}

fn pts(o: &Map<String, Value>, k: &str) -> Result<Vec<Pt>, String> {
    match o.get(k) {
        None => Ok(vec![]),
        Some(Value::Array(a)) => a.iter().map(|q| pt(q).ok_or_else(|| format!("{k}: expected [x,y] pairs"))).collect(),
        _ => Err(format!("{k}: expected an array of [x,y]")),
    }
}

impl Spec {
    pub fn from_json(v: &Value, pal: &Palette) -> Result<Spec, Vec<String>> {
        let mut errs = Vec::new();
        let o = match v.as_object() {
            Some(o) => o,
            None => return Err(vec!["spec must be a JSON object".into()]),
        };
        let name = o.get("name").and_then(Value::as_str).unwrap_or("sprite").to_string();
        let anim = o.get("anim").and_then(Value::as_str).unwrap_or("none").to_string();
        if !crate::anim::PRESET_NAMES.contains(&anim.as_str()) {
            errs.push(format!("unknown anim {anim:?} (use one of {:?})", crate::anim::PRESET_NAMES));
        }
        let prims_v = match o.get("prims").and_then(Value::as_array) {
            Some(a) if !a.is_empty() => a.clone(),
            _ => {
                errs.push("spec.prims must be a non-empty array".into());
                return Err(errs);
            }
        };
        let mut prims = Vec::with_capacity(prims_v.len());
        for (i, pv) in prims_v.iter().enumerate() {
            match parse_prim(pv, i, pal) {
                Ok(p) => prims.push(p),
                Err(e) => errs.extend(e),
            }
        }
        let mut tracks = Vec::new();
        if let Some(arr) = o.get("tracks").and_then(Value::as_array) {
            for (k, tv) in arr.iter().enumerate() {
                match parse_track(tv) {
                    Ok(t) => {
                        if let Some(g) = &t.g {
                            if !prims_v.iter().any(|p| p.get("g").and_then(Value::as_str) == Some(g)) {
                                errs.push(format!("tracks[{k}]: no prim has group {g:?}"));
                            }
                        }
                        tracks.push(t)
                    }
                    Err(e) => errs.push(format!("tracks[{k}]: {e}")),
                }
            }
        }
        let frames = num(o, "frames").map(|f| f.clamp(1.0, crate::MAX_FRAMES as f64) as usize).unwrap_or(crate::FRAMES);
        let mut hold = Vec::new();
        match o.get("hold") {
            None | Some(Value::Null) => {}
            Some(Value::Array(a)) => {
                if a.len() > frames {
                    errs.push(format!("hold has {} entries but the loop has {frames} frames", a.len()));
                }
                for (k, h) in a.iter().enumerate() {
                    match h.as_f64() {
                        Some(x) if (1.0..=100.0).contains(&x) && x.fract() == 0.0 => hold.push(x as u32),
                        _ => errs.push(format!("hold[{k}] must be a whole number from 1 to 100")),
                    }
                }
            }
            Some(_) => errs.push("hold must be an array of whole numbers, one per frame".into()),
        }
        if !errs.is_empty() {
            return Err(errs);
        }
        Ok(Spec {
            name,
            cat: o.get("cat").and_then(Value::as_str).map(str::to_string),
            anim,
            frames,
            ms: num(o, "ms").map(|f| f.clamp(20.0, 10_000.0) as u32).unwrap_or(110),
            hold,
            snap: truthy(o.get("snap")),
            tracks,
            prims,
            raw: v.clone(),
        })
    }

    pub fn is_animated(&self) -> bool {
        self.anim != "none" || !self.tracks.is_empty()
    }

    /// How long frame `i` is shown, in ms (`ms` × its hold).
    pub fn frame_ms(&self, i: usize) -> u32 {
        self.ms * self.hold.get(i).copied().unwrap_or(1)
    }
}

fn parse_prim(v: &Value, i: usize, pal: &Palette) -> Result<Prim, Vec<String>> {
    let mut errs = Vec::new();
    let o = match v.as_object() {
        Some(o) => o,
        None => return Err(vec![format!("prims[{i}]: must be an object")]),
    };
    let t = o.get("t").and_then(Value::as_str).unwrap_or("");
    let kind = match Kind::parse(t) {
        Some(k) => k,
        None => {
            return Err(vec![format!("prims[{i}]: unknown type {t:?} (use one of {:?})", Kind::ALL)]);
        }
    };
    let default_m = if kind == Kind::Px { "black" } else { "gray" };
    let mname = o.get("m").and_then(Value::as_str).unwrap_or(default_m);
    let m = pal.index_of(mname).unwrap_or_else(|| {
        errs.push(format!("prims[{i}]: unknown material {mname:?}"));
        0
    });
    let c = o.get("c").and_then(pt).unwrap_or([16.0, 16.0]);
    let r = match o.get("r") {
        Some(Value::Number(n)) => {
            let x = n.as_f64().unwrap_or(1.0);
            [x, x]
        }
        Some(v @ Value::Array(_)) => pt(v).unwrap_or([1.0, 1.0]),
        _ => [1.0, 1.0],
    };
    let p = o
        .get("p")
        .and_then(Value::as_array)
        .and_then(|a| {
            let f: Vec<f64> = a.iter().filter_map(Value::as_f64).collect();
            (f.len() == 4).then(|| [f[0], f[1], f[2], f[3]])
        })
        .unwrap_or([0.0; 4]);
    let pts_v = pts(o, "pts").unwrap_or_else(|e| {
        errs.push(format!("prims[{i}]: {e}"));
        vec![]
    });
    match kind {
        Kind::Ellipse | Kind::Arc | Kind::Star | Kind::Ngon if o.get("c").is_none() || o.get("r").is_none() => {
            errs.push(format!("prims[{i}]: {t} needs c and r"))
        }
        Kind::Rect if o.get("p").is_none() => errs.push(format!("prims[{i}]: rect needs p:[x0,y0,x1,y1]")),
        Kind::Poly if pts_v.len() < 3 => errs.push(format!("prims[{i}]: poly needs ≥3 pts")),
        Kind::Line if pts_v.len() < 2 => errs.push(format!("prims[{i}]: line needs ≥2 pts")),
        Kind::Px if pts_v.is_empty() => errs.push(format!("prims[{i}]: px needs pts")),
        _ => {}
    }
    let clip = match o.get("clip") {
        None | Some(Value::Null) => None,
        Some(v) => match v.as_f64() {
            Some(x) if x >= 0.0 && (x as usize) < i && x.fract() == 0.0 => Some(x as usize),
            _ => {
                errs.push(format!("prims[{i}]: clip must be the index of an EARLIER prim"));
                None
            }
        },
    };
    if !errs.is_empty() {
        return Err(errs);
    }
    Ok(Prim {
        kind,
        m,
        c,
        r,
        r2: num(o, "r2"),
        n: num(o, "n").map(|x| x.max(3.0) as usize).unwrap_or(5),
        a: num(o, "a").unwrap_or(-90.0),
        a0: num(o, "a0").unwrap_or(180.0),
        a1: num(o, "a1").unwrap_or(360.0),
        pie: o.get("pie").map(|v| truthy(Some(v))).unwrap_or(true),
        p,
        round: num(o, "round").unwrap_or(0.0),
        pts: pts_v,
        w: num(o, "w").unwrap_or(1.0),
        smooth: num(o, "smooth").map(|x| x.max(0.0) as usize).unwrap_or(0),
        puff: num(o, "puff"),
        height: num(o, "height").unwrap_or(1.0),
        flat: truthy(o.get("flat")),
        shade: num(o, "shade").map(|x| x as i32).unwrap_or(if kind == Kind::Px { 1 } else { 2 }).clamp(0, 4),
        tone: num(o, "tone").map(|x| x as i32).unwrap_or(0),
        gloss: truthy(o.get("gloss")),
        edge: truthy(o.get("edge")),
        weight: num(o, "weight").unwrap_or(1.0),
        rot: num(o, "rot").unwrap_or(0.0),
        flipx: truthy(o.get("flipx")),
        cut: truthy(o.get("cut")),
        clip,
        lit: truthy(o.get("lit")),
        cap: o.get("cap").map(|v| truthy(Some(v))).unwrap_or(true),
        g: o.get("g").and_then(Value::as_str).map(str::to_string),
    })
}

fn parse_track(v: &Value) -> Result<Track, String> {
    let o = v.as_object().ok_or("must be an object")?;
    let g = match o.get("g").and_then(Value::as_str) {
        None | Some("*") => None,
        Some(s) => Some(s.to_string()),
    };
    let prop = match o.get("prop").and_then(Value::as_str) {
        Some("dx") => Prop::Dx,
        Some("dy") => Prop::Dy,
        Some("rot") => Prop::Rot,
        Some("sx") => Prop::Sx,
        Some("sy") => Prop::Sy,
        _ => return Err("prop must be dx|dy|rot|sx|sy".into()),
    };
    let wave = match o.get("wave").and_then(Value::as_str).unwrap_or("sin") {
        "sin" => Wave::Sin,
        "abs" => Wave::Abs,
        "abs2" => Wave::Abs2,
        "saw" => Wave::Saw,
        "square" => Wave::Square,
        w => return Err(format!("unknown wave {w:?}")),
    };
    let pivot = match o.get("pivot") {
        None | Some(Value::Null) => Pivot::Center,
        Some(Value::String(s)) => match s.as_str() {
            "center" => Pivot::Center,
            "bottom" => Pivot::Bottom,
            "top" => Pivot::Top,
            "each" => Pivot::Each,
            other => return Err(format!("unknown pivot {other:?}")),
        },
        Some(v) => Pivot::At(pt(v).ok_or("pivot must be a name or [x,y]")?),
    };
    Ok(Track {
        g,
        prop,
        amp: num(o, "amp").ok_or("amp is required")?,
        wave,
        phase: num(o, "phase").unwrap_or(0.0),
        pivot,
    })
}
