//! Measurable style checks an author should react to.

use serde::Serialize;
use std::collections::HashSet;

use crate::render::Frame;
use crate::spec::{Kind, Prop, Spec};

#[derive(Debug, Clone, Serialize)]
pub struct Lint {
    pub warnings: Vec<String>,
    pub bbox: [usize; 4],
    pub span_px: usize,
    pub colors: usize,
    pub frames: usize,
    pub prims: usize,
}

pub fn lint(spec: &Spec, frames: &[Frame]) -> Lint {
    let f0 = &frames[0];
    let size = spec.size;
    // the checks below were tuned on the 32px canvas; `u` scales their pixel thresholds
    let u = size / 32;
    let mut warnings = Vec::new();
    let (mut x0, mut y0, mut x1, mut y1) = (size, size, 0, 0);
    let mut colors = HashSet::new();
    for y in 0..size {
        for x in 0..size {
            if f0.alpha(x, y) > 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
                let k = (y * size + x) * 4;
                colors.insert([f0.0[k], f0.0[k + 1], f0.0[k + 2]]);
            }
        }
    }
    if colors.is_empty() {
        return Lint { warnings: vec!["sprite rendered empty".into()], bbox: [0; 4], span_px: 0, colors: 0, frames: frames.len(), prims: spec.prims.len() };
    }
    let span = (x1 - x0 + 1).max(y1 - y0 + 1);
    if span < 26 * u {
        warnings.push(format!(
            "subject spans {span}px on its longest axis; aim for {}–{}px so it fills the {size}px canvas",
            26 * u,
            30 * u
        ));
    }
    for (k, f) in frames.iter().enumerate() {
        let last = size - 1;
        let edge = (0..size).any(|i| f.alpha(i, 0) > 0 || f.alpha(i, last) > 0 || f.alpha(0, i) > 0 || f.alpha(last, i) > 0);
        if edge {
            warnings.push(format!(
                "frame {k} touches the canvas edge, so the outline is clipped; keep shapes inside 1.5–{}.5",
                size - 2
            ));
            break;
        }
    }
    let upright = matches!(spec.cat.as_deref(), Some("animals" | "home" | "places" | "vehicles"));
    if upright && y1 + 3 * u < size {
        warnings.push(format!("lowest pixel is y={y1}; upright things should rest near y={}", size - 2));
    }
    if colors.len() < 6 {
        warnings.push(format!("only {} colours; sprite may look flat — vary materials or shading", colors.len()));
    }
    for (k, t) in spec.tracks.iter().enumerate() {
        let Some(g) = t.g.as_deref() else { continue };
        if !matches!(t.prop, Prop::Rot | Prop::Sx | Prop::Sy) {
            continue;
        }
        if spec.prims.iter().any(|p| p.kind == Kind::Px && p.g.as_deref() == Some(g)) {
            warnings.push(format!(
                "tracks[{k}] {} group {g:?}, which has px pixels. px are rigid stamps: they travel with the group but \
                 never {} themselves. Draw that part with shapes if it should {}.",
                if t.prop == Prop::Rot { "rotates" } else { "scales" },
                if t.prop == Prop::Rot { "turn" } else { "grow or shrink" },
                if t.prop == Prop::Rot { "turn" } else { "change size" },
            ));
        }
    }
    Lint { warnings, bbox: [x0, y0, x1, y1], span_px: span, colors: colors.len(), frames: frames.len(), prims: spec.prims.len() }
}
