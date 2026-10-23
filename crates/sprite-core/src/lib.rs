//! Sprite Factory core: shape specs → lit, palette-locked, outlined 32×32 pixel-art frames.
//!
//! No I/O and no platform dependencies, so the same code runs in the CLI, the MCP server and WASM.

pub mod anim;
pub mod geom;
pub mod lint;
pub mod palette;
pub mod raster;
pub mod render;
pub mod spec;

pub use palette::Palette;
pub use render::{hd_size_ok, render_anim, render_anim_hd, render_frame, render_frame_hd, Frame, HdFrame, RenderOpts};
pub use spec::Spec;

/// Output canvas size (px).
pub const SIZE: usize = 32;
/// Supersampling factor used for shading and coverage.
pub const SS: usize = 8;
/// Default frames per animation loop.
pub const FRAMES: usize = 8;
/// Upper bound on `frames`, so one spec can't ask for an unbounded render.
pub const MAX_FRAMES: usize = 64;

/// Parse + render in one call. Errors are human-readable spec problems.
pub fn render_json(spec: &serde_json::Value) -> Result<(Spec, Vec<Frame>), Vec<String>> {
    let pal = Palette::new();
    let s = Spec::from_json(spec, &pal)?;
    let frames = render_anim(&s, &pal, &RenderOpts::default());
    Ok((s, frames))
}
