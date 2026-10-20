//! Animation presets and per-frame transforms.

use crate::geom::{affine, bbox_of, mul, prim_center, Mat, I3};
use crate::spec::{Pivot, Prim, Prop, Spec, Track, Wave};

pub const PRESET_NAMES: [&str; 10] = ["none", "idle", "bob", "float", "hop", "wobble", "sway", "pulse", "spin", "drive"];

fn tr(prop: Prop, amp: f64, wave: Wave, phase: f64, pivot: Pivot) -> Track {
    Track { g: None, prop, amp, wave, phase, pivot }
}

pub fn preset(name: &str) -> Vec<Track> {
    use Pivot::*;
    use Prop::*;
    use Wave::*;
    match name {
        "idle" => vec![tr(Sy, 0.04, Sin, 0.0, Bottom), tr(Sx, -0.03, Sin, 0.0, Bottom)],
        "bob" => vec![tr(Dy, 1.2, Sin, 0.0, Center)],
        "float" => vec![tr(Dy, 1.5, Sin, 0.0, Center), tr(Rot, 4.0, Sin, 0.25, Center)],
        "hop" => vec![tr(Dy, -3.0, Abs, 0.0, Center), tr(Sy, 0.10, Abs, 0.0, Bottom), tr(Sx, -0.07, Abs, 0.0, Bottom)],
        "wobble" => vec![tr(Rot, 7.0, Sin, 0.0, Bottom)],
        "sway" => vec![tr(Rot, 5.0, Sin, 0.0, Bottom)],
        "pulse" => vec![tr(Sx, 0.07, Sin, 0.0, Center), tr(Sy, 0.07, Sin, 0.0, Center)],
        "spin" => vec![tr(Rot, 360.0, Saw, 0.0, Center)],
        "drive" => vec![tr(Dy, -0.6, Abs2, 0.0, Center)],
        _ => vec![],
    }
}

fn wave(w: Wave, t: f64) -> f64 {
    use std::f64::consts::PI;
    match w {
        Wave::Sin => (2.0 * PI * t).sin(),
        Wave::Abs => (PI * t).sin().abs(),
        Wave::Abs2 => (2.0 * PI * t).sin().abs(),
        Wave::Saw => t,
        Wave::Square => {
            if t < 0.5 {
                1.0
            } else {
                0.0
            }
        }
    }
}

fn resolve_pivot<'a>(pv: &Pivot, members: impl IntoIterator<Item = &'a Prim>) -> [f64; 2] {
    if let Pivot::At(p) = pv {
        return *p;
    }
    let [x0, y0, x1, y1] = bbox_of(members);
    match pv {
        Pivot::Bottom => [(x0 + x1) / 2.0, y1],
        Pivot::Top => [(x0 + x1) / 2.0, y0],
        _ => [(x0 + x1) / 2.0, (y0 + y1) / 2.0],
    }
}

/// One matrix per primitive for time t ∈ [0,1).
pub fn frame_transforms(spec: &Spec, t: f64) -> Vec<Mat> {
    let mut tracks = preset(&spec.anim);
    tracks.extend(spec.tracks.iter().cloned());
    let prims = &spec.prims;
    let mut g_mat = I3;
    let mut local = vec![I3; prims.len()];
    for trk in &tracks {
        let idxs: Vec<usize> = match &trk.g {
            None => (0..prims.len()).collect(),
            Some(g) => (0..prims.len()).filter(|&i| prims[i].g.as_deref() == Some(g)).collect(),
        };
        if idxs.is_empty() {
            continue;
        }
        let v = trk.amp * wave(trk.wave, (t + trk.phase).rem_euclid(1.0));
        let mk = |pivot: [f64; 2]| match trk.prop {
            Prop::Dx => affine(0.0, 1.0, 1.0, v, 0.0, pivot),
            Prop::Dy => affine(0.0, 1.0, 1.0, 0.0, v, pivot),
            Prop::Rot => affine(v, 1.0, 1.0, 0.0, 0.0, pivot),
            Prop::Sx => affine(0.0, 1.0 + v, 1.0, 0.0, 0.0, pivot),
            Prop::Sy => affine(0.0, 1.0, 1.0 + v, 0.0, 0.0, pivot),
        };
        if trk.g.is_none() {
            g_mat = mul(&mk(resolve_pivot(&trk.pivot, prims.iter())), &g_mat);
        } else if matches!(trk.pivot, Pivot::Each) {
            for &i in &idxs {
                local[i] = mul(&mk(prim_center(&prims[i])), &local[i]);
            }
        } else {
            let m = mk(resolve_pivot(&trk.pivot, idxs.iter().map(|&i| &prims[i])));
            for &i in &idxs {
                local[i] = mul(&m, &local[i]);
            }
        }
    }
    local.iter().map(|l| mul(&g_mat, l)).collect()
}
