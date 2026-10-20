//! PNG/GIF encoding, sheets and atlases.

use anyhow::{Context, Result};
use sprite_core::{Frame, SIZE};
use std::io::Cursor;
use std::path::Path;

use crate::font;

pub const BACKDROP: [u8; 3] = [246, 245, 240];

/// An RGBA image buffer.
pub struct Rgba {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Rgba {
    pub fn new(w: usize, h: usize) -> Self {
        Rgba { w, h, px: vec![0; w * h * 4] }
    }

    /// Frames side by side.
    pub fn strip(frames: &[Frame]) -> Self {
        let mut out = Rgba::new(SIZE * frames.len(), SIZE);
        for (k, f) in frames.iter().enumerate() {
            out.blit_frame(f, k * SIZE, 0, 1);
        }
        out
    }

    pub fn blit_frame(&mut self, f: &Frame, x0: usize, y0: usize, scale: usize) {
        for y in 0..SIZE * scale {
            for x in 0..SIZE * scale {
                let s = ((y / scale) * SIZE + x / scale) * 4;
                let d = ((y0 + y) * self.w + x0 + x) * 4;
                self.px[d..d + 4].copy_from_slice(&f.0[s..s + 4]);
            }
        }
    }

    pub fn scaled(&self, k: usize) -> Self {
        let mut out = Rgba::new(self.w * k, self.h * k);
        for y in 0..out.h {
            for x in 0..out.w {
                let s = ((y / k) * self.w + x / k) * 4;
                let d = (y * out.w + x) * 4;
                out.px[d..d + 4].copy_from_slice(&self.px[s..s + 4]);
            }
        }
        out
    }

    /// Composite onto a flat backdrop (sprites are fully opaque or fully transparent).
    pub fn on_backdrop(&self, bg: [u8; 3]) -> Rgb {
        let mut out = Rgb::new(self.w, self.h, bg);
        for i in 0..self.w * self.h {
            if self.px[i * 4 + 3] > 0 {
                out.px[i * 3..i * 3 + 3].copy_from_slice(&self.px[i * 4..i * 4 + 3]);
            }
        }
        out
    }

    pub fn png(&self) -> Result<Vec<u8>> {
        encode_png(&self.px, self.w, self.h, png::ColorType::Rgba)
    }
}

pub struct Rgb {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Rgb {
    pub fn new(w: usize, h: usize, bg: [u8; 3]) -> Self {
        Rgb { w, h, px: bg.iter().copied().cycle().take(w * h * 3).collect() }
    }

    pub fn paste(&mut self, img: &Rgba, x0: usize, y0: usize) {
        for y in 0..img.h {
            for x in 0..img.w {
                let s = (y * img.w + x) * 4;
                if img.px[s + 3] == 0 || x0 + x >= self.w || y0 + y >= self.h {
                    continue;
                }
                let d = ((y0 + y) * self.w + x0 + x) * 3;
                self.px[d..d + 3].copy_from_slice(&img.px[s..s + 3]);
            }
        }
    }

    pub fn fill(&mut self, x0: usize, y0: usize, w: usize, h: usize, c: [u8; 3]) {
        for y in y0..(y0 + h).min(self.h) {
            for x in x0..(x0 + w).min(self.w) {
                self.px[(y * self.w + x) * 3..(y * self.w + x) * 3 + 3].copy_from_slice(&c);
            }
        }
    }

    pub fn text(&mut self, x: usize, y: usize, s: &str, scale: usize, c: [u8; 3]) {
        font::draw_text(&mut self.px, self.w, self.h, x, y, s, scale, c);
    }

    pub fn png(&self) -> Result<Vec<u8>> {
        encode_png(&self.px, self.w, self.h, png::ColorType::Rgb)
    }
}

fn encode_png(px: &[u8], w: usize, h: usize, ct: png::ColorType) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(Cursor::new(&mut out), w as u32, h as u32);
        enc.set_color(ct);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::High);
        let mut wr = enc.write_header()?;
        wr.write_image_data(px)?;
    }
    Ok(out)
}

/// Decode a PNG into RGBA8 (used by the golden-image check).
pub fn decode_png(path: &Path) -> Result<Rgba> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut dec = png::Decoder::new(std::io::BufReader::new(file));
    dec.set_transformations(png::Transformations::EXPAND);
    let mut reader = dec.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size().context("png too large")?];
    let info = reader.next_frame(&mut buf)?;
    let (w, h) = (info.width as usize, info.height as usize);
    let px = match info.color_type {
        png::ColorType::Rgba => buf[..w * h * 4].to_vec(),
        png::ColorType::Rgb => buf[..w * h * 3].chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf[..w * h * 2].chunks(2).flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
        png::ColorType::Grayscale => buf[..w * h].iter().flat_map(|&g| [g, g, g, 255]).collect(),
        other => anyhow::bail!("unsupported png colour type {other:?}"),
    };
    Ok(Rgba { w, h, px })
}

/// Looping animated GIF at `scale`, transparent background.
pub fn gif(frames: &[Frame], scale: usize, ms: u32) -> Result<Vec<u8>> {
    let side = (SIZE * scale) as u16;
    let mut out = Vec::new();
    {
        let mut enc = gif::Encoder::new(&mut out, side, side, &[])?;
        enc.set_repeat(gif::Repeat::Infinite)?;
        for f in frames {
            let mut img = Rgba::new(SIZE * scale, SIZE * scale);
            img.blit_frame(f, 0, 0, scale);
            let mut fr = gif::Frame::from_rgba_speed(side, side, &mut img.px, 10);
            fr.delay = (ms / 10) as u16;
            fr.dispose = gif::DisposalMethod::Background;
            enc.write_frame(&fr)?;
        }
    }
    Ok(out)
}

/// Grid of first frames with a label under each — the image a model critiques.
pub fn contact_sheet(items: &[(String, Frame)], scale: usize, cols: usize) -> Rgb {
    let cols = cols.max(1).min(items.len().max(1));
    let label_scale = if scale >= 4 { 2 } else { 1 };
    let cell_w = (SIZE * scale + 16).max(font::text_width("MMMMMMMMMMMM", label_scale) / 2);
    let cell_h = SIZE * scale + 16 + font::GLYPH_H * label_scale + 6;
    let rows = items.len().div_ceil(cols).max(1);
    let mut sheet = Rgb::new(cols * cell_w, rows * cell_h, [236, 232, 224]);
    for (k, (label, f)) in items.iter().enumerate() {
        let (x, y) = ((k % cols) * cell_w, (k / cols) * cell_h);
        sheet.fill(x + 4, y + 4, SIZE * scale + 8, SIZE * scale + 8, BACKDROP);
        let mut img = Rgba::new(SIZE * scale, SIZE * scale);
        img.blit_frame(f, 0, 0, scale);
        sheet.paste(&img, x + 8, y + 8);
        sheet.text(x + 6, y + SIZE * scale + 16, &label.replace('_', " "), label_scale, [40, 40, 50]);
    }
    sheet
}

/// One row per sprite with every animation frame.
pub fn strip_sheet(rows: &[(String, Vec<Frame>)], scale: usize) -> Rgb {
    let nmax = rows.iter().map(|r| r.1.len()).max().unwrap_or(1);
    let cw = SIZE * scale + 4;
    let label_w = 100;
    let mut sheet = Rgb::new(label_w + nmax * cw, rows.len().max(1) * cw, [236, 232, 224]);
    for (r, (name, frames)) in rows.iter().enumerate() {
        sheet.text(4, r * cw + cw / 2 - 4, &name.replace('_', " "), 1, [40, 40, 50]);
        for (k, f) in frames.iter().enumerate() {
            let (x, y) = (label_w + k * cw, r * cw + 2);
            sheet.fill(x, y, SIZE * scale, SIZE * scale, BACKDROP);
            let mut img = Rgba::new(SIZE * scale, SIZE * scale);
            img.blit_frame(f, 0, 0, scale);
            sheet.paste(&img, x, y);
        }
    }
    sheet
}

pub fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}
