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

    /// Frames side by side (all one sprite, so all one size).
    pub fn strip(frames: &[Frame]) -> Self {
        let side = frames.first().map_or(SIZE, Frame::size);
        let mut out = Rgba::new(side * frames.len(), side);
        for (k, f) in frames.iter().enumerate() {
            out.blit_frame(f, k * side, 0, 1);
        }
        out
    }

    pub fn blit_frame(&mut self, f: &Frame, x0: usize, y0: usize, scale: usize) {
        let side = f.size();
        for y in 0..side * scale {
            for x in 0..side * scale {
                let s = ((y / scale) * side + x / scale) * 4;
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
    /// GIF frames written: identical neighbours merged, frames over 655.35 s split
    pub frames: usize,
    /// one pass through the loop, in ms
    pub loop_ms: u64,
}

/// Animated GIF from one sprite's frames. `durations` is ms per frame (see `Spec::frame_ms`).
pub fn gif(frames: &[Frame], durations: &[u32], o: &GifOpts) -> Result<(Vec<u8>, GifInfo)> {
    let bufs: Vec<&[u8]> = frames.iter().map(|f| f.0.as_slice()).collect();
    encode_gif(&bufs, frames.first().map_or(SIZE, Frame::size), durations, o)
}

/// Shared display side for a set of frames of mixed sizes: at least `32 × scale`, a whole multiple
/// of the largest frame, so each frame fills it by an integer upscale (32px frames at `scale` when
/// every frame is 32px).
fn display_side<'a>(frames: impl IntoIterator<Item = &'a Frame>, scale: usize) -> usize {
    let biggest = frames.into_iter().map(Frame::size).max().unwrap_or(SIZE);
    biggest * (SIZE * scale).div_ceil(biggest).max(1)
}

/// Animated GIF from square RGBA buffers of side `side` (32px frames or HD frames).
///
/// Sprites only ever use the renderer's fixed colours, so the GIF gets one global palette built
/// from them: no quantization, no dithering, identical colours in every frame. Index 0 is the
/// transparent (or background) colour.
///
/// Identical consecutive frames are merged into one longer frame, and when the loop wraps from a
/// last frame that looks like the first, that frame is folded into the first too. Delays are
/// rounded on the running total so a loop keeps its exact length.
///
/// After the first frame, each frame only covers the rectangle that changed, with unchanged
/// pixels written as a transparent "keep" index (frames stack with `Keep` disposal). A GIF can't
/// draw transparency over an opaque pixel, so on a transparent background a frame whose
/// successor clears pixels grows its rectangle over them and uses `Background` disposal to erase
/// it. Frames are compared at 1× and re-indexed on the second pass, so memory stays at a few 1×
/// frames plus one upscaled rectangle however long the loop is.
pub fn encode_gif(frames: &[&[u8]], side: usize, durations: &[u32], o: &GifOpts) -> Result<(Vec<u8>, GifInfo)> {
    anyhow::ensure!(!frames.is_empty(), "no frames to encode");
    let scale = o.scale.max(1);
    let out_side = side * scale;
    anyhow::ensure!(out_side <= 4096, "GIF would be {out_side}px wide; keep side × scale ≤ 4096");
    if let Some(i) = frames.iter().position(|f| f.len() != side * side * 4) {
        anyhow::bail!("frame {i} is not {side}×{side} RGBA");
    }

    // ── palette: [bg/transparent] + every colour the renderer can emit [+ keep], padded to 256
    let colours = sprite_core::Palette::new().output_colors();
    anyhow::ensure!(colours.len() + 2 <= 256, "{} renderer colours don't fit a GIF palette", colours.len());
    let flat: Vec<u8> = std::iter::once(o.bg.unwrap_or([0, 0, 0])).chain(colours.iter().copied()).flatten().chain(std::iter::repeat(0)).take(768).collect();
    // on a transparent GIF "unchanged" is index 0 itself; on a background it needs a spare index
    let keep = if o.bg.is_none() { 0 } else { colours.len() as u8 + 1 };
    let mut index = Indexer::new(&colours);

    // ── play order, merged into runs of identical frames: (source frame, ms)
    let n = frames.len();
    let mut order: Vec<usize> = (0..n).collect();
    if o.pingpong && n > 2 {
        order.extend((1..n - 1).rev());
    }
    let looping = o.repeat != Some(1);
    let px = side * side;
    let (mut first, mut last, mut cur) = (vec![0u8; px], vec![0u8; px], vec![0u8; px]);
    let mut runs: Vec<(usize, u64)> = Vec::new();
    for &i in &order {
        index.frame(frames[i], side, &mut cur);
        let ms = durations.get(i).copied().unwrap_or(110) as u64;
        match runs.last_mut() {
            Some(r) if cur == last => r.1 += ms,
            _ => {
                if runs.is_empty() {
                    first.copy_from_slice(&cur);
                }
                runs.push((i, ms));
                std::mem::swap(&mut last, &mut cur);
            }
        }
    }
    // the loop wraps from the last frame straight into the first: same picture, one frame
    if looping && runs.len() > 1 && last == first {
        let (_, ms) = runs.pop().unwrap();
        runs[0].1 += ms;
    }

    let mut out = Vec::new();
    let mut w = FrameWriter { transparent: Some(keep), keep, total_ms: 0, t_cs: 0, written: 0 };
    {
        let mut enc = gif::Encoder::new(&mut out, out_side as u16, out_side as u16, &flat)?;
        // The loop extension counts *re*plays and 0 means forever, so "play once" omits it entirely.
        match o.repeat {
            None => enc.set_repeat(gif::Repeat::Infinite)?,
            Some(k) if k > 1 => enc.set_repeat(gif::Repeat::Finite(k - 1))?,
            Some(_) => {}
        }
        // `shown` is the 1× canvas before a frame is drawn (0 = cleared); `now` is the frame
        let (mut shown, mut now, mut next) = (vec![0u8; px], first.clone(), vec![0u8; px]);
        let mut rect_px = Vec::new();
        for k in 0..runs.len() {
            let has_next = if k + 1 < runs.len() {
                index.frame(frames[runs[k + 1].0], side, &mut next);
                true
            } else {
                next.copy_from_slice(&first);
                looping
            };
            // bounding box of what changes, plus (transparent GIFs) what the next frame clears
            let (mut x0, mut y0, mut x1, mut y1, mut erase) = (side, side, 0, 0, false);
            for p in 0..px {
                let clears = o.bg.is_none() && has_next && now[p] != 0 && next[p] == 0;
                if k == 0 || now[p] != shown[p] || clears {
                    erase |= clears;
                    let (x, y) = (p % side, p / side);
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
                }
            }
            if x0 >= x1 {
                // nothing to draw (a frame that only differs from one erased beneath it): 1 pixel
                (x0, y0, x1, y1) = (0, 0, 1, 1);
            }
            let (rw, rh) = (x1 - x0, y1 - y0);
            rect_px.clear();
            rect_px.resize(rw * rh * scale * scale, 0);
            for y in y0..y1 {
                let row0 = (y - y0) * scale * rw * scale;
                for x in x0..x1 {
                    let p = y * side + x;
                    debug_assert!(k == 0 || o.bg.is_some() || now[p] != 0 || shown[p] == 0, "transparent over opaque at {p}");
                    let v = if k > 0 && now[p] == shown[p] { keep } else { now[p] };
                    rect_px[row0 + (x - x0) * scale..row0 + (x - x0 + 1) * scale].fill(v);
                }
                for yy in 1..scale {
                    rect_px.copy_within(row0..row0 + rw * scale, row0 + yy * rw * scale);
                }
            }
            let area = [x0 * scale, y0 * scale, rw * scale, rh * scale].map(|v| v as u16);
            let dispose = if erase { gif::DisposalMethod::Background } else { gif::DisposalMethod::Keep };
            w.write(&mut enc, area, &rect_px, runs[k].1, dispose)?;

            shown.copy_from_slice(&now);
            if erase {
                for y in y0..y1 {
                    shown[y * side + x0..y * side + x1].fill(0);
                }
            }
            std::mem::swap(&mut now, &mut next);
        }
    }
    Ok((out, GifInfo { side: out_side, frames: w.written, loop_ms: w.total_ms }))
}

/// RGBA → palette index, remembering the last colour (sprites have long runs of one colour).
struct Indexer<'a> {
    colours: &'a [[u8; 3]],
    lookup: HashMap<[u8; 3], u8>,
    last: Option<([u8; 3], u8)>,
}

impl<'a> Indexer<'a> {
    fn new(colours: &'a [[u8; 3]]) -> Self {
        let lookup = colours.iter().enumerate().map(|(i, c)| (*c, (i + 1) as u8)).collect();
        Indexer { colours, lookup, last: None }
    }

    fn index(&mut self, c: [u8; 3]) -> u8 {
        if let Some((lc, li)) = self.last {
            if lc == c {
                return li;
            }
        }
        let v = match self.lookup.get(&c) {
            Some(&v) => v,
            None => {
                debug_assert!(false, "colour {c:?} is not in Palette::output_colors()");
                let v = nearest(self.colours, c) as u8 + 1;
                self.lookup.insert(c, v);
                v
            }
        };
        self.last = Some((c, v));
        v
    }

    /// Index one `side`×`side` RGBA frame into `out` (0 = transparent).
    fn frame(&mut self, src: &[u8], side: usize, out: &mut [u8]) {
        for (p, v) in out.iter_mut().enumerate().take(side * side) {
            let c = &src[p * 4..p * 4 + 4];
            *v = if c[3] == 0 { 0 } else { self.index([c[0], c[1], c[2]]) };
        }
    }
}

/// Writes frames with delays rounded on the running total. GIF delays are u16 centiseconds, so a
/// frame longer than 655.35 s is written as the frame plus "keep" frames over the same rectangle;
/// only the last of them carries the real disposal.
struct FrameWriter {
    transparent: Option<u8>,
    keep: u8,
    total_ms: u64,
    t_cs: u64,
    written: usize,
}

impl FrameWriter {
    fn write<W: std::io::Write>(&mut self, enc: &mut gif::Encoder<W>, [left, top, width, height]: [u16; 4], idx: &[u8], ms: u64, dispose: gif::DisposalMethod) -> Result<()> {
        self.total_ms += ms;
        let end_cs = (self.total_ms + 5) / 10;
        let mut delay = end_cs.saturating_sub(self.t_cs).max(2); // browsers turn delays < 2cs into 10cs
        self.t_cs += delay;
        let mut hold: Option<Vec<u8>> = None;
        while delay > 0 {
            let d = delay.min(u16::MAX as u64);
            delay -= d;
            let buffer = match &hold {
                None => std::borrow::Cow::Borrowed(idx),
                Some(h) => std::borrow::Cow::Borrowed(h.as_slice()),
            };
            let fr = gif::Frame {
                left,
                top,
                width,
                height,
                delay: d as u16,
                dispose: if delay == 0 { dispose } else { gif::DisposalMethod::Keep },
                transparent: self.transparent,
                buffer,
                ..gif::Frame::default()
            };
            enc.write_frame(&fr)?;
            self.written += 1;
            hold.get_or_insert_with(|| vec![self.keep; idx.len()]);
        }
        Ok(())
    }
}

fn nearest(colours: &[[u8; 3]], c: [u8; 3]) -> usize {
    let d = |a: &[u8; 3]| (0..3).map(|k| (a[k] as i32 - c[k] as i32).pow(2)).sum::<i32>();
    (0..colours.len()).min_by_key(|&i| d(&colours[i])).unwrap_or(0)
}

/// Parse a background argument: "transparent"/"none"/"" → None, "#rrggbb" or "rrggbb" → colour.
pub fn parse_bg(s: &str) -> std::result::Result<Option<[u8; 3]>, String> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("transparent") || s.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let t = s.strip_prefix('#').unwrap_or(s);
    if t.len() != 6 || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("bg must be \"transparent\" or a hex colour like #f6f5f0, not {s:?}"));
    }
    let p = |i: usize| u8::from_str_radix(&t[i..i + 2], 16).unwrap();
    Ok(Some([p(0), p(2), p(4)]))
}

/// Grid of first frames with a label under each — the image a model critiques.
/// Every sprite fills the same cell whatever its canvas size, so a blind reviewer compares them fairly.
pub fn contact_sheet(items: &[(String, Frame)], scale: usize, cols: usize) -> Rgb {
    let cols = cols.max(1).min(items.len().max(1));
    let label_scale = if scale >= 4 { 2 } else { 1 };
    let d = display_side(items.iter().map(|i| &i.1), scale);
    let cell_w = (d + 16).max(font::text_width("MMMMMMMMMMMM", label_scale) / 2);
    let cell_h = d + 16 + font::GLYPH_H * label_scale + 6;
    let rows = items.len().div_ceil(cols).max(1);
    let mut sheet = Rgb::new(cols * cell_w, rows * cell_h, [236, 232, 224]);
    for (k, (label, f)) in items.iter().enumerate() {
        let (x, y) = ((k % cols) * cell_w, (k / cols) * cell_h);
        sheet.fill(x + 4, y + 4, d + 8, d + 8, BACKDROP);
        let mut img = Rgba::new(d, d);
        img.blit_frame(f, 0, 0, d / f.size());
        sheet.paste(&img, x + 8, y + 8);
        sheet.text(x + 6, y + d + 16, &label.replace('_', " "), label_scale, [40, 40, 50]);
    }
    sheet
}

/// One row per sprite with every animation frame.
pub fn strip_sheet(rows: &[(String, Vec<Frame>)], scale: usize) -> Rgb {
    let nmax = rows.iter().map(|r| r.1.len()).max().unwrap_or(1);
    let d = display_side(rows.iter().flat_map(|r| &r.1), scale);
    let cw = d + 4;
    let label_w = 100;
    let mut sheet = Rgb::new(label_w + nmax * cw, rows.len().max(1) * cw, [236, 232, 224]);
    for (r, (name, frames)) in rows.iter().enumerate() {
        sheet.text(4, r * cw + cw / 2 - 4, &name.replace('_', " "), 1, [40, 40, 50]);
        for (k, f) in frames.iter().enumerate() {
            let (x, y) = (label_w + k * cw, r * cw + 2);
            sheet.fill(x, y, d, d, BACKDROP);
            let mut img = Rgba::new(d, d);
            img.blit_frame(f, 0, 0, d / f.size());
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
        let keep = Palette::new().output_colors().len() as u8 + 1;
        assert!(d.transparent.iter().all(|t| *t == Some(keep)), "only the spare keep index is transparent");
        assert_eq!(d.first[0], 0, "corner is the background index");
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
    fn frames_longer_than_a_gif_delay_are_split() {
        // 10 s × hold 100 = 1000 s per frame; one GIF delay tops out at 655.35 s
        let (s, f) = sprite(serde_json::json!({"name": "b", "anim": "spin", "frames": 2, "ms": 10000, "hold": [100, 1],
            "prims": [{"t": "star", "c": [16, 16], "r": 12, "m": "gold"}]}));
        let (bytes, info) = gif(&f, &durations(&s, f.len()), &GifOpts::default()).unwrap();
        let d = decode(&bytes);
        assert_eq!(d.delays, vec![65535, 34465, 1000]);
        assert_eq!(info.loop_ms, 1_010_000);
        assert_eq!(info.frames, 3);
    }

    #[test]
    fn upscale_matches_the_source_pixels() {
        let (s, f) = sprite(serde_json::json!({"name": "b", "prims": [{"t": "ellipse", "c": [16, 16], "r": 10, "m": "red"}]}));
        let d = decode(&gif(&f, &durations(&s, 1), &GifOpts { scale: 3, ..Default::default() }).unwrap().0);
        for y in 0..96 {
            for x in 0..96 {
                let src_opaque = f[0].0[((y / 3) * 32 + x / 3) * 4 + 3] > 0;
                assert_eq!(d.first[y * 96 + x] != 0, src_opaque, "pixel ({x},{y})");
            }
        }
    }

    /// Play a GIF the way a browser does (frame rectangles, transparent index, disposal) for
    /// `loops` passes and return the picture after each frame: None = transparent.
    fn play(bytes: &[u8], loops: usize) -> Vec<Vec<Option<u8>>> {
        let mut shown = Vec::new();
        let mut canvas: Vec<Option<u8>> = vec![];
        for _ in 0..loops {
            let mut opts = gif::DecodeOptions::new();
            opts.set_color_output(gif::ColorOutput::Indexed);
            let mut d = opts.read_info(bytes).unwrap();
            let w = d.width() as usize;
            canvas.resize(w * d.height() as usize, None);
            while let Some(f) = d.read_next_frame().unwrap() {
                let (l, t, fw) = (f.left as usize, f.top as usize, f.width as usize);
                for (i, &v) in f.buffer.iter().enumerate() {
                    if Some(v) != f.transparent {
                        canvas[(t + i / fw) * w + l + i % fw] = Some(v);
                    }
                }
                shown.push(canvas.clone());
                if f.dispose == gif::DisposalMethod::Background {
                    for i in 0..f.buffer.len() {
                        canvas[(t + i / fw) * w + l + i % fw] = None;
                    }
                }
            }
        }
        shown.dedup(); // split long frames repeat the same picture
        shown
    }

    /// What `play` should show: each distinct picture in play order (identical neighbours, and a
    /// last frame equal to the first when looping, collapse), upscaled, for `loops` passes.
    fn expected(f: &[Frame], o: &GifOpts, loops: usize) -> Vec<Vec<Option<u8>>> {
        let colours = Palette::new().output_colors();
        let mut ix = Indexer::new(&colours);
        let mut order: Vec<usize> = (0..f.len()).collect();
        if o.pingpong && f.len() > 2 {
            order.extend((1..f.len() - 1).rev());
        }
        order.dedup_by_key(|i| f[*i].0.clone());
        if o.repeat != Some(1) && order.len() > 1 && f[order[0]].0 == f[*order.last().unwrap()].0 {
            order.pop();
        }
        let (s, side) = (o.scale, SIZE * o.scale);
        let pics: Vec<Vec<Option<u8>>> = order
            .iter()
            .map(|&i| {
                (0..side * side)
                    .map(|p| {
                        let q = ((p / side) / s * SIZE + (p % side) / s) * 4;
                        let c = &f[i].0[q..q + 4];
                        if c[3] == 0 { o.bg.map(|_| 0) } else { Some(ix.index([c[0], c[1], c[2]])) }
                    })
                    .collect()
            })
            .collect();
        let mut all: Vec<_> = (0..loops).flat_map(|_| pics.iter().cloned()).collect();
        all.dedup();
        all
    }

    #[test]
    fn delta_frames_replay_exactly_over_two_loops() {
        let star = serde_json::json!({"name": "b", "anim": "spin", "prims": [{"t": "star", "c": [16, 16], "r": 12, "m": "gold"}]});
        let bob = serde_json::json!({"name": "b", "anim": "bob", "prims": [{"t": "ellipse", "c": [16, 14], "r": 7, "m": "red"}, {"t": "rect", "p": [10, 24, 22, 28], "m": "teal"}]});
        let blink = serde_json::json!({"name": "b", "frames": 4, "prims": [{"t": "ellipse", "c": [16, 16], "r": 10, "m": "red"}],
            "tracks": [{"prop": "sx", "amp": -1, "wave": "square"}]});
        for spec in [star, bob, blink] {
            let (s, f) = sprite(spec.clone());
            let dur = durations(&s, f.len());
            for o in [
                GifOpts::default(),
                GifOpts { scale: 3, bg: Some([246, 245, 240]), ..Default::default() },
                GifOpts { pingpong: true, scale: 2, ..Default::default() },
                GifOpts { repeat: Some(1), scale: 1, ..Default::default() },
            ] {
                let (bytes, _) = gif(&f, &dur, &o).unwrap();
                let loops = if o.repeat == Some(1) { 1 } else { 2 }; // play-once never wraps
                assert!(play(&bytes, loops) == expected(&f, &o, loops), "{spec} with {o:?}");
            }
        }
    }

    #[test]
    fn later_frames_only_cover_what_changed() {
        // only the hand moves: frames after the first are a small rectangle, not the canvas
        let (s, f) = sprite(serde_json::json!({"name": "b", "frames": 4, "prims": [{"t": "rect", "p": [4, 4, 28, 28], "m": "teal"},
            {"t": "ellipse", "c": [24, 8], "r": 2, "m": "red", "g": "hand"}], "tracks": [{"g": "hand", "prop": "dy", "amp": 2}]}));
        let (bytes, _) = gif(&f, &durations(&s, f.len()), &GifOpts { scale: 8, ..Default::default() }).unwrap();
        let mut d = gif::DecodeOptions::new().read_info(bytes.as_slice()).unwrap();
        let mut areas = vec![];
        while let Some(fr) = d.read_next_frame().unwrap() {
            areas.push(fr.width as usize * fr.height as usize);
        }
        assert_eq!(areas[0], 256 * 256);
        assert!(areas[1..].iter().all(|&a| a <= 64 * 64), "{areas:?}");
    }

    #[test]
    fn a_last_frame_matching_the_first_folds_into_it() {
        // square wave shifted a quarter loop: A A B B B B A A, which loops as A(4) B(4)
        let (s, f) = sprite(serde_json::json!({"name": "b", "snap": true, "ms": 100, "prims": [{"t": "ellipse", "c": [16, 16], "r": 10, "m": "red"}],
            "tracks": [{"prop": "dy", "amp": 1, "wave": "square", "phase": 0.75}]}));
        assert_eq!(f.len(), 8);
        assert!(f[0].0 == f[7].0 && f[0].0 != f[3].0);
        let dur = durations(&s, f.len());
        let (bytes, info) = gif(&f, &dur, &GifOpts::default()).unwrap();
        assert_eq!((info.frames, info.loop_ms), (2, 800));
        assert_eq!(decode(&bytes).delays, vec![40, 40]);
        // played once there is no wrap, so the last frames stay
        let (once, info) = gif(&f, &dur, &GifOpts { repeat: Some(1), ..Default::default() }).unwrap();
        assert_eq!(info.frames, 3);
        assert_eq!(decode(&once).delays, vec![20, 40, 20]);
    }

    #[test]
    fn parse_bg_accepts_hex_and_transparent() {
        assert_eq!(parse_bg("transparent").unwrap(), None);
        assert_eq!(parse_bg("").unwrap(), None);
        assert_eq!(parse_bg("#F6f5f0").unwrap(), Some([246, 245, 240]));
        assert_eq!(parse_bg("102030").unwrap(), Some([16, 32, 48]));
        assert!(parse_bg("red").is_err() && parse_bg("#fff").is_err() && parse_bg("#").is_err());
    }
}
