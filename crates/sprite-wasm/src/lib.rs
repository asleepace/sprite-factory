//! Browser bindings for the Sprite Factory renderer.
//!
//! Build: `rustup target add wasm32-unknown-unknown && wasm-pack build crates/sprite-wasm --target web --release`
//!
//! ```js
//! import init, { render, palette } from "./pkg/sprite_wasm.js";
//! await init();
//! const out = render(JSON.stringify(spec));      // throws a readable error for bad specs
//! const frames = out.frames;                    // Uint8Array, out.frameCount × 32×32×4 RGBA
//! const lint = JSON.parse(out.lint);            // { warnings, bbox, span_px, colors, frames, prims }
//! ```
use sprite_core::{lint::lint, render_anim, Palette, RenderOpts, Spec, SIZE};
use wasm_bindgen::prelude::*;

/// Result of rendering one spec.
#[wasm_bindgen]
pub struct Rendered {
    frames: Vec<u8>,
    frame_count: usize,
    ms: u32,
    lint: String,
}

#[wasm_bindgen]
impl Rendered {
    /// All frames back to back, RGBA, 32×32 each.
    #[wasm_bindgen(getter)]
    pub fn frames(&self) -> Vec<u8> {
        self.frames.clone()
    }
    #[wasm_bindgen(getter, js_name = frameCount)]
    pub fn frame_count(&self) -> usize {
        self.frame_count
    }
    /// Milliseconds per frame.
    #[wasm_bindgen(getter)]
    pub fn ms(&self) -> u32 {
        self.ms
    }
    /// Style warnings and stats as JSON.
    #[wasm_bindgen(getter)]
    pub fn lint(&self) -> String {
        self.lint.clone()
    }
}

/// Render a spec (JSON text). Errors list every problem found, one per line.
#[wasm_bindgen]
pub fn render(spec_json: &str) -> Result<Rendered, JsError> {
    let (spec, frames) = render_inner(spec_json).map_err(|e| JsError::new(&e))?;
    let l = lint(&spec, &frames);
    Ok(Rendered {
        frame_count: frames.len(),
        frames: frames.into_iter().flat_map(|f| f.0).collect(),
        ms: spec.ms,
        lint: serde_json::to_string(&l).unwrap_or_default(),
    })
}

fn render_inner(spec_json: &str) -> Result<(Spec, Vec<sprite_core::Frame>), String> {
    let v: serde_json::Value = serde_json::from_str(spec_json).map_err(|e| format!("not valid JSON: {e}"))?;
    let pal = Palette::new();
    let spec = Spec::from_json(&v, &pal).map_err(|e| e.join("\n"))?;
    let frames = render_anim(&spec, &pal, &RenderOpts::default());
    Ok((spec, frames))
}

/// The locked palette as JSON: { material: ["#rrggbb" × 5] }.
#[wasm_bindgen]
pub fn palette() -> String {
    let m: serde_json::Map<String, serde_json::Value> = Palette::new()
        .iter()
        .map(|(n, r)| (n.to_string(), r.iter().map(|c| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])).collect()))
        .collect();
    serde_json::Value::Object(m).to_string()
}

/// Canvas size in pixels (32).
#[wasm_bindgen(js_name = spriteSize)]
pub fn sprite_size() -> usize {
    SIZE
}

#[cfg(test)]
mod tests {
    #[test]
    fn renders_natively() {
        let (s, f) = super::render_inner(r#"{"name":"b","anim":"bob","prims":[{"t":"ellipse","c":[16,16],"r":10,"m":"red"}]}"#).unwrap();
        assert_eq!(f.len(), 8);
        assert_eq!(s.ms, 110);
        assert!(super::render_inner(r#"{"prims":[{"t":"nope"}]}"#).unwrap_err().contains("unknown type"));
    }
}
