//! Measurable style checks an author should react to.

use serde::Serialize;
use std::collections::HashSet;

use crate::render::Frame;
use crate::spec::Spec;
use crate::SIZE;

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
    let mut warnings = Vec::new();
    let (mut x0, mut y0, mut x1, mut y1) = (SIZE, SIZE, 0, 0);
    let mut colors = HashSet::new();
    for y in 0..SIZE {
        for x in 0..SIZE {
            if f0.alpha(x, y) > 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
                let k = (y * SIZE + x) * 4;
                colors.insert([f0.0[k], f0.0[k + 1], f0.0[k + 2]]);
            }
        }
    }
    if colors.is_empty() {
        return Lint { warnings: vec!["sprite rendered empty".into()], bbox: [0; 4], span_px: 0, colors: 0, frames: frames.len(), prims: spec.prims.len() };
    }
    let span = (x1 - x0 + 1).max(y1 - y0 + 1);
    if span < 26 {
        warnings.push(format!("subject spans {span}px on its longest axis; aim for 26–30px so it fills the canvas"));
    }
    for (k, f) in frames.iter().enumerate() {
        let edge = (0..SIZE).any(|i| f.alpha(i, 0) > 0 || f.alpha(i, SIZE - 1) > 0 || f.alpha(0, i) > 0 || f.alpha(SIZE - 1, i) > 0);
        if edge {
            warnings.push(format!("frame {k} touches the canvas edge, so the outline is clipped; keep shapes inside 1.5–30.5"));
            break;
        }
    }
    let upright = matches!(spec.cat.as_deref(), Some("animals" | "home" | "places" | "vehicles"));
    if upright && y1 < 29 {
        warnings.push(format!("lowest pixel is y={y1}; upright things should rest near y=30"));
    }
    if colors.len() < 6 {
        warnings.push(format!("only {} colours; sprite may look flat — vary materials or shading", colors.len()));
    }
    Lint { warnings, bbox: [x0, y0, x1, y1], span_px: span, colors: colors.len(), frames: frames.len(), prims: spec.prims.len() }
}
