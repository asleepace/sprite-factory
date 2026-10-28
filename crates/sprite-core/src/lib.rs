//! Sprite Factory core: shape specs → lit, palette-locked, outlined pixel-art frames on a 32×32
//! (default), 64×64 or 128×128 canvas.
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
pub use render::{hd_size_ok, hd_size_ok_for, render_anim, render_anim_hd, render_frame, render_frame_hd, Frame, HdFrame, RenderOpts};
pub use spec::Spec;

/// Default canvas size (px); a spec's `size` can ask for 64 or 128. Everything tuned in pixels
/// was tuned at this size and scales by `spec.size / SIZE`.
pub const SIZE: usize = 32;
/// Supersampling factor used for shading and coverage at 32px (fewer at bigger canvases).
pub const SS: usize = 8;
/// Default frames per animation loop.
pub const FRAMES: usize = 8;
/// Upper bound on `frames`, so one spec can't ask for an unbounded render.
pub const MAX_FRAMES: usize = 64;
/// Bumped whenever the same spec renders differently, so HTTP ETags change with the output.
pub const RENDER_VERSION: u64 = 2;

/// Parse + render in one call. Errors are human-readable spec problems.
pub fn render_json(spec: &serde_json::Value) -> Result<(Spec, Vec<Frame>), Vec<String>> {
    let pal = Palette::new();
    let s = Spec::from_json(spec, &pal)?;
    let frames = render_anim(&s, &pal, &RenderOpts::default());
    Ok((s, frames))
}
