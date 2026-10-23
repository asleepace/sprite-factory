//! PNG/GIF encoding, sheets and atlases.

use anyhow::{Context, Result};
use sprite_core::{Frame, SIZE};
use std::collections::HashMap;
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

/// How a GIF is put together. The default is what most callers want: transparent, loops forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GifOpts {
    /// nearest-neighbour upscale factor
    pub scale: usize,
    /// flat background colour; `None` keeps the background transparent
    pub bg: Option<[u8; 3]>,
    /// play forward then backward (0 1 2 3 2 1 …) instead of wrapping around
    pub pingpong: bool,
    /// times to play; `None` loops forever
    pub repeat: Option<u16>,
}

impl Default for GifOpts {
    fn default() -> Self {
        GifOpts { scale: 4, bg: None, pingpong: false, repeat: None }
    }
}

/// What `encode_gif` produced, for callers that report on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GifInfo {
    pub side: usize,
    /// frames actually written, after identical neighbours were merged
    pub frames: usize,
    /// one pass through the loop, in ms
    pub loop_ms: u32,
}

/// Animated GIF from 32×32 sprite frames. `durations` is ms per frame (see `Spec::frame_ms`).
pub fn gif(frames: &[Frame], durations: &[u32], o: &GifOpts) -> Result<(Vec<u8>, GifInfo)> {
    let bufs: Vec<&[u8]> = frames.iter().map(|f| f.0.as_slice()).collect();
    encode_gif(&bufs, SIZE, durations, o)
}

/// Animated GIF from square RGBA buffers of side `side` (32px frames or HD frames).
///
/// Sprites only ever use the renderer's fixed colours, so the GIF gets one global palette built
/// from them: no quantization, no dithering, identical colours in every frame. Index 0 is the
/// transparent (or background) colour. Identical consecutive frames are merged into one longer
/// frame, and delays are rounded on the running total so a loop keeps its exact length.
pub fn encode_gif(frames: &[&[u8]], side: usize, durations: &[u32], o: &GifOpts) -> Result<(Vec<u8>, GifInfo)> {
    anyhow::ensure!(!frames.is_empty(), "no frames to encode");
    let scale = o.scale.max(1);
    let out_side = side * scale;
    anyhow::ensure!(out_side <= 4096, "GIF would be {out_side}px wide; keep side × scale ≤ 4096");

    // ── palette: [bg/transparent] + every colour the renderer can emit, padded to 256
    let colours = sprite_core::Palette::new().output_colors();
    let mut table: Vec<[u8; 3]> = Vec::with_capacity(256);
    table.push(o.bg.unwrap_or([0, 0, 0]));
    table.extend(&colours);
    let mut lookup: HashMap<[u8; 3], u8> = colours.iter().enumerate().map(|(i, c)| (*c, (i + 1) as u8)).collect();
    let flat: Vec<u8> = table.iter().flatten().copied().chain(std::iter::repeat(0)).take(768).collect();

    // ── play order
    let n = frames.len();
    let mut order: Vec<usize> = (0..n).collect();
    if o.pingpong && n > 2 {
        order.extend((1..n - 1).rev());
    }

    // ── index each frame (upscaled), merging identical neighbours
    let mut out_frames: Vec<(Vec<u8>, u32)> = Vec::new();
    for &i in &order {
        let src = frames[i];
        anyhow::ensure!(src.len() == side * side * 4, "frame {i} is not {side}×{side} RGBA");
        let mut idx = vec![0u8; out_side * out_side];
        for y in 0..side {
            for x in 0..side {
                let p = &src[(y * side + x) * 4..(y * side + x) * 4 + 4];
                let v = if p[3] == 0 {
                    0
                } else {
                    let c = [p[0], p[1], p[2]];
                    match lookup.get(&c) {
                        Some(&v) => v,
                        None => {
                            // not a renderer colour (shouldn't happen); use the nearest one
                            let v = nearest(&colours, c) as u8 + 1;
                            lookup.insert(c, v);
                            v
                        }
                    }
                };
                for yy in 0..scale {
                    let row = (y * scale + yy) * out_side + x * scale;
                    idx[row..row + scale].fill(v);
                }
            }
        }
        let ms = durations.get(i).copied().unwrap_or(110);
        match out_frames.last_mut() {
            Some((prev, d)) if *prev == idx => *d += ms,
            _ => out_frames.push((idx, ms)),
        }
    }

    // ── write
    let mut out = Vec::new();
    let mut total_ms = 0u32;
    {
        let s16 = out_side as u16;
        let mut enc = gif::Encoder::new(&mut out, s16, s16, &flat)?;
        // The loop extension counts *re*plays and 0 means forever, so "play once" omits it entirely.
        match o.repeat {
            None => enc.set_repeat(gif::Repeat::Infinite)?,
            Some(k) if k > 1 => enc.set_repeat(gif::Repeat::Finite(k - 1))?,
            Some(_) => {}
        }
        let mut t_cs = 0u32; // running total, in GIF centiseconds
        for (idx, ms) in &out_frames {
            total_ms += ms;
            let end_cs = (total_ms + 5) / 10;
            let delay = end_cs.saturating_sub(t_cs).max(2); // browsers turn delays < 2cs into 10cs
            t_cs += delay;
            let fr = gif::Frame {
                width: s16,
                height: s16,
                delay: delay.min(u16::MAX as u32) as u16,
                dispose: gif::DisposalMethod::Background,
                transparent: if o.bg.is_none() { Some(0) } else { None },
                buffer: std::borrow::Cow::Borrowed(idx),
                ..gif::Frame::default()
            };
            enc.write_frame(&fr)?;
        }
    }
    Ok((out, GifInfo { side: out_side, frames: out_frames.len(), loop_ms: total_ms }))
}

fn nearest(colours: &[[u8; 3]], c: [u8; 3]) -> usize {
    let d = |a: &[u8; 3]| (0..3).map(|k| (a[k] as i32 - c[k] as i32).pow(2)).sum::<i32>();
    (0..colours.len()).min_by_key(|&i| d(&colours[i])).unwrap_or(0)
}

/// Parse a background argument: "transparent"/"none"/"" → None, "#rrggbb" or "rrggbb" → colour.
pub fn parse_bg(s: &str) -> std::result::Result<Option<[u8; 3]>, String> {
    let t = s.trim().trim_start_matches('#');
    if t.is_empty() || t.eq_ignore_ascii_case("transparent") || t.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    if t.len() != 6 || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("bg must be \"transparent\" or a hex colour like #f6f5f0, not {s:?}"));
    }
    let p = |i: usize| u8::from_str_radix(&t[i..i + 2], 16).unwrap();
    Ok(Some([p(0), p(2), p(4)]))
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

#[cfg(test)]
mod tests {
    use super::*;
    use sprite_core::{render_anim, Palette, RenderOpts, Spec};

    struct Decoded {
        delays: Vec<u16>,
        transparent: Vec<Option<u8>>,
        palette_len: usize,
        repeat: gif::Repeat,
        first: Vec<u8>,
        side: u16,
    }

    fn decode(bytes: &[u8]) -> Decoded {
        let mut opts = gif::DecodeOptions::new();
        opts.set_color_output(gif::ColorOutput::Indexed);
        let mut d = opts.read_info(bytes).unwrap();
        let (mut delays, mut transparent, mut first) = (vec![], vec![], None);
        while let Some(f) = d.read_next_frame().unwrap() {
            delays.push(f.delay);
            transparent.push(f.transparent);
            first.get_or_insert_with(|| f.buffer.to_vec());
            assert!(f.palette.is_none(), "frames share the global palette");
        }
        Decoded {
            delays,
            transparent,
            palette_len: d.global_palette().map_or(0, |p| p.len() / 3),
            repeat: d.repeat(),
            first: first.unwrap(),
            side: d.width(),
        }
    }

    fn sprite(v: serde_json::Value) -> (Spec, Vec<Frame>) {
        let pal = Palette::new();
        let s = Spec::from_json(&v, &pal).unwrap();
        let f = render_anim(&s, &pal, &RenderOpts::default());
        (s, f)
    }

    fn durations(s: &Spec, n: usize) -> Vec<u32> {
        (0..n).map(|i| s.frame_ms(i)).collect()
    }

    #[test]
    fn transparent_by_default_with_one_global_palette() {
        let (s, f) = sprite(serde_json::json!({"name": "b", "anim": "bob", "prims": [{"t": "ellipse", "c": [16, 16], "r": 10, "m": "red"}]}));
        let (bytes, info) = gif(&f, &durations(&s, f.len()), &GifOpts::default()).unwrap();
        let d = decode(&bytes);
        assert_eq!((d.side, info.side), (128, 128), "default scale 4");
        assert_eq!(d.palette_len, 256);
        assert!(d.transparent.iter().all(|t| *t == Some(0)));
        assert_eq!(d.first[0], 0, "corner is the transparent index");
        assert_eq!(d.repeat, gif::Repeat::Infinite);
    }

    #[test]
    fn background_colour_replaces_transparency() {
        let (s, f) = sprite(serde_json::json!({"name": "b", "prims": [{"t": "ellipse", "c": [16, 16], "r": 10, "m": "red"}]}));
        let o = GifOpts { scale: 2, bg: Some([246, 245, 240]), ..Default::default() };
        let d = decode(&gif(&f, &durations(&s, f.len()), &o).unwrap().0);
        assert!(d.transparent.iter().all(|t| t.is_none()));
        assert_eq!(d.side, 64);
    }

    #[test]
    fn timing_follows_hold_and_identical_frames_merge() {
        // square-wave bob with snap: frames 0–3 are identical, as are 4–7
        let (s, f) = sprite(serde_json::json!({"name": "b", "snap": true, "ms": 100, "hold": [2], "prims": [{"t": "ellipse", "c": [16, 16], "r": 10, "m": "red"}],
            "tracks": [{"prop": "dy", "amp": 1, "wave": "square"}]}));
        assert_eq!(f.len(), 8);
        let (bytes, info) = gif(&f, &durations(&s, f.len()), &GifOpts::default()).unwrap();
        let d = decode(&bytes);
        assert_eq!(info.frames, 2, "identical neighbours merged");
        assert_eq!(d.delays, vec![50, 40], "200+100+100+100 ms, then 4×100 ms, in centiseconds");
        assert_eq!(info.loop_ms, 900);
    }

    #[test]
    fn delays_round_on_the_running_total() {
        // 8 × 115 ms = 920 ms; per-frame rounding would give 8 × 12 = 960 or 8 × 11 = 880
        let (s, f) = sprite(serde_json::json!({"name": "b", "anim": "spin", "ms": 115, "prims": [{"t": "star", "c": [16, 16], "r": 12, "m": "gold"}]}));
        let d = decode(&gif(&f, &durations(&s, f.len()), &GifOpts::default()).unwrap().0);
        assert_eq!(d.delays.iter().map(|&x| x as u32).sum::<u32>(), 92);
    }

    #[test]
    fn pingpong_and_repeat() {
        let (s, f) = sprite(serde_json::json!({"name": "b", "anim": "spin", "prims": [{"t": "star", "c": [16, 16], "r": 12, "m": "gold"}]}));
        let dur = durations(&s, f.len());
        let pp = decode(&gif(&f, &dur, &GifOpts { pingpong: true, ..Default::default() }).unwrap().0);
        assert_eq!(pp.delays.len(), 14, "0..7 then 6..1");
        let three = decode(&gif(&f, &dur, &GifOpts { repeat: Some(3), ..Default::default() }).unwrap().0);
        assert_eq!(three.repeat, gif::Repeat::Finite(2), "3 plays = 2 replays");
        let once = decode(&gif(&f, &dur, &GifOpts { repeat: Some(1), ..Default::default() }).unwrap().0);
        assert_ne!(once.repeat, gif::Repeat::Infinite, "play-once must not loop");
    }

    #[test]
    fn parse_bg_accepts_hex_and_transparent() {
        assert_eq!(parse_bg("transparent").unwrap(), None);
        assert_eq!(parse_bg("").unwrap(), None);
        assert_eq!(parse_bg("#F6f5f0").unwrap(), Some([246, 245, 240]));
        assert_eq!(parse_bg("102030").unwrap(), Some([16, 32, 48]));
        assert!(parse_bg("red").is_err() && parse_bg("#fff").is_err());
    }
}
