//! Loading specs from batch files, single-spec files or a library folder; atlas export.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sprite_core::Frame;
use std::path::{Path, PathBuf};

use crate::imgio::{self, Rgba};

/// Accepts batch files (JSON array), single-spec files, or directories of either.
pub fn load_specs(paths: &[PathBuf]) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut files: Vec<_> = std::fs::read_dir(p)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|f| f.extension().is_some_and(|x| x == "json"))
                .collect();
            files.sort();
            for f in files {
                push_file(&f, &mut out)?;
            }
        } else {
            push_file(p, &mut out)?;
        }
    }
    Ok(out)
}

fn push_file(p: &Path, out: &mut Vec<Value>) -> Result<()> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?)
        .with_context(|| format!("parse {}", p.display()))?;
    match v {
        Value::Array(a) => out.extend(a),
        Value::Object(_) => out.push(v),
        _ => bail!("{}: expected a spec object or an array of specs", p.display()),
    }
    Ok(())
}

pub fn spec_name(v: &Value) -> &str {
    v.get("name").and_then(Value::as_str).unwrap_or("sprite")
}

/// ms per frame for a stored spec (its `ms` × `hold`), falling back to 110 ms if it won't parse.
pub fn frame_durations(v: &Value, n: usize) -> Vec<u32> {
    match sprite_core::Spec::from_json(v, &sprite_core::Palette::new()) {
        Ok(s) => (0..n).map(|i| s.frame_ms(i)).collect(),
        Err(_) => vec![110; n],
    }
}

/// Default and hard ceiling for an atlas side, in output px. 4096 loads on every WebGL device;
/// 8192 on most desktops. Bigger textures fail to upload or get silently downscaled.
pub const ATLAS_MAX_SIDE: usize = 4096;
pub const ATLAS_SIDE_LIMIT: usize = 8192;

/// Pack every frame of every sprite into one PNG + a Phaser/Pixi JSON-hash frame map.
///
/// Each frame gets a cell of its canvas size × scale, so a 64px sprite's cells are twice as wide
/// as a 32px one's. Cells are shelf-packed into a near-square sheet: each sprite's frames sit in a
/// run left to right (wrapping onto a new row only if the run is longer than a row), and a row is
/// as tall as its tallest cell. When every sprite is the same size this is a plain grid. Frames
/// that are pixel-identical — a still sprite, a hold pose — share one cell, so the frame map can
/// point several keys at the same rectangle. Returns an error, before allocating anything, if
/// the result would be wider or taller than `max_side` px.
pub fn atlas(sprites: &[(Value, Vec<Frame>)], scale: usize, image_name: &str, max_side: usize) -> Result<(Rgba, Value)> {
    let scale = scale.max(1);
    let cell_of = |fr: &[Frame]| fr.first().map_or(sprite_core::SIZE, Frame::size) * scale;
    // width is measured in units of the smallest cell, which every other cell is a multiple of
    let unit = sprites.iter().map(|(_, fr)| cell_of(fr)).min().unwrap_or(sprite_core::SIZE * scale);
    let max_cols = (max_side / unit).max(1);
    // unique frames per sprite, and for each frame which unique cell it uses
    let mut uniq: Vec<(Vec<&Frame>, Vec<usize>)> = Vec::with_capacity(sprites.len());
    for (_, fr) in sprites {
        let mut cells: Vec<&Frame> = Vec::new();
        let mut map = Vec::with_capacity(fr.len());
        for f in fr {
            let k = cells.iter().position(|c| *c == f).unwrap_or_else(|| {
                cells.push(f);
                cells.len() - 1
            });
            map.push(k);
        }
        uniq.push((cells, map));
    }
    let total: usize = uniq.iter().map(|u| u.0.len()).sum();
    // in units: a cell k units wide covers k×k units of area
    let units = |(_, fr): &(Value, Vec<Frame>)| cell_of(fr) / unit;
    let area: usize = sprites.iter().zip(&uniq).map(|(sp, u)| units(sp) * units(sp) * u.0.len()).sum();
    let longest = sprites.iter().zip(&uniq).map(|(sp, u)| units(sp) * u.0.len()).max().unwrap_or(1);
    let widest = sprites.iter().map(units).max().unwrap_or(1);
    // square-ish, but at least one sprite's whole run per row when it fits
    let cols = ((area as f64).sqrt().ceil() as usize).max(longest).clamp(widest.min(max_cols), max_cols.max(widest));
    // place runs (shelf packing): (x, y) in px of each unique cell
    let row_w = cols * unit;
    let (mut cx, mut cy, mut row_h) = (0usize, 0usize, 0usize);
    let mut placed: Vec<Vec<(usize, usize)>> = Vec::with_capacity(uniq.len());
    for ((_, fr), (cells, _)) in sprites.iter().zip(&uniq) {
        let cell = cell_of(fr);
        let run = cells.len() * cell;
        if cx > 0 && cx + run > row_w && run <= row_w {
            (cx, cy, row_h) = (0, cy + row_h, 0);
        }
        let mut at = Vec::with_capacity(cells.len());
        for _ in 0..cells.len() {
            if cx + cell > row_w {
                (cx, cy, row_h) = (0, cy + row_h, 0);
            }
            at.push((cx, cy));
            cx += cell;
            row_h = row_h.max(cell);
        }
        placed.push(at);
    }
    let (w, h) = (row_w, (cy + row_h).max(unit));
    if w > max_side || h > max_side {
        bail!(
            "{} sprites ({total} distinct frames) need a {w}×{h}px atlas at scale {scale}, over the {max_side}px limit \
             that GPUs reliably load. Export fewer sprites (a category or a names list), or use a smaller scale.",
            sprites.len()
        );
    }
    let mut img = Rgba::new(w, h);
    let mut frames = serde_json::Map::new();
    let mut anims = serde_json::Map::new();
    for (((spec, fr), (cells, map)), at) in sprites.iter().zip(&uniq).zip(&placed) {
        let name = spec_name(spec);
        let cell = cell_of(fr);
        for (c, &(x, y)) in cells.iter().zip(at) {
            img.blit_frame(c, x, y, scale);
        }
        let mut keys = Vec::with_capacity(fr.len());
        for (k, &u) in map.iter().enumerate() {
            let (x, y) = at[u];
            let key = format!("{name}_{k}");
            frames.insert(key.clone(), json!({"frame": {"x": x, "y": y, "w": cell, "h": cell}, "sourceSize": {"w": cell, "h": cell}}));
            keys.push(key);
        }
        anims.insert(
            name.to_string(),
            json!({
                "frames": keys,
                "frameMs": spec.get("ms").and_then(Value::as_u64).unwrap_or(110),
                "durations": frame_durations(spec, fr.len()),
                "category": spec.get("cat"),
                "size": cell / scale,
            }),
        );
    }
    // spriteSize is the one cell size when every sprite shares it; with mixed sizes read each
    // frame's w/h (or the animation's size) instead
    let sizes: std::collections::BTreeSet<usize> = sprites.iter().map(|(_, fr)| cell_of(fr)).collect();
    let sprite_size = if sizes.len() == 1 { json!(sizes.first()) } else { Value::Null };
    let meta = json!({
        "meta": {"image": image_name, "size": {"w": img.w, "h": img.h}, "scale": scale, "spriteSize": sprite_size},
        "frames": frames,
        "animations": anims,
    });
    Ok((img, meta))
}

pub fn save_sprite_outputs(dir: &Path, name: &str, frames: &[Frame], durations: &[u32]) -> Result<()> {
    let side = frames[0].size();
    let mut first = Rgba::new(side, side);
    first.blit_frame(&frames[0], 0, 0, 1);
    imgio::write(&dir.join(format!("{name}.png")), &first.png()?)?;
    imgio::write(&dir.join(format!("{name}_sheet.png")), &Rgba::strip(frames).png()?)?;
    if frames.len() > 1 {
        // 192px at 32, the same footprint at bigger canvases
        let (gif, _) = imgio::gif(frames, durations, &imgio::GifOpts { scale: (192 / side).max(1), ..Default::default() })?;
        imgio::write(&dir.join(format!("{name}.gif")), &gif)?;
    }
    Ok(())
}

/// Map over items on every core (scoped threads; no extra dependency).
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(items.len().max(1));
    let chunk = items.len().div_ceil(threads.max(1)).max(1);
    std::thread::scope(|sc| {
        let handles: Vec<_> = items.chunks(chunk).map(|c| sc.spawn(|| c.iter().map(&f).collect::<Vec<R>>())).collect();
        handles.into_iter().flat_map(|h| h.join().expect("render thread panicked")).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sprite(name: &str, anim: &str) -> (Value, Vec<Frame>) {
        let v = json!({"name": name, "anim": anim, "prims": [{"t": "ellipse", "c": [16, 16], "r": 10, "m": "red"}]});
        let (_, f) = sprite_core::render_json(&v).unwrap();
        (v, f)
    }

    #[test]
    fn packs_tightly_and_shares_identical_frames() {
        let still = sprite("dot", "none");
        let moving = sprite("ball", "wobble");
        let mut distinct: Vec<&Frame> = Vec::new();
        for f in &moving.1 {
            if !distinct.contains(&f) {
                distinct.push(f);
            }
        }
        let (img, meta) = atlas(&[moving.clone(), still.clone(), still.clone()], 2, "a.png", ATLAS_MAX_SIDE).unwrap();
        let frames = meta["frames"].as_object().unwrap();
        let cell_of = |k: &str| (frames[k]["frame"]["x"].as_u64().unwrap() as usize, frames[k]["frame"]["y"].as_u64().unwrap() as usize);
        // every frame key points inside the image, at a whole cell
        for k in frames.keys() {
            let (x, y) = cell_of(k);
            assert!(x + 64 <= img.w && y + 64 <= img.h && x % 64 == 0 && y % 64 == 0);
        }
        // identical frames share a cell; distinct ones don't
        let mut cells: Vec<(usize, usize)> = (0..8).map(|i| cell_of(&format!("ball_{i}"))).collect();
        cells.sort();
        cells.dedup();
        assert_eq!(cells.len(), distinct.len());
        // the whole thing is no bigger than the distinct frames need, rounded up to a grid
        let used = distinct.len() + 2;
        assert!(img.w * img.h <= (used + img.w / 64) * 64 * 64, "{}×{} for {used} cells", img.w, img.h);
        assert_eq!(meta["animations"]["ball"]["frames"].as_array().unwrap().len(), 8);
    }

    #[test]
    fn mixed_canvas_sizes_pack_without_overlap() {
        let sized = |name: &str, size: usize, anim: &str| {
            let h = size as f64 / 2.0;
            let v = json!({"name": name, "size": size, "anim": anim, "prims": [{"t": "ellipse", "c": [h, h], "r": h * 0.6, "m": "red"}]});
            let (_, f) = sprite_core::render_json(&v).unwrap();
            (v, f)
        };
        let set = [sized("a", 32, "wobble"), sized("b", 128, "none"), sized("c", 64, "bob"), sized("d", 32, "none")];
        let (img, meta) = atlas(&set, 1, "a.png", ATLAS_MAX_SIDE).unwrap();
        let rects: Vec<(String, [u64; 4])> = meta["frames"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| {
                let f = &v["frame"];
                (k.clone(), [f["x"].as_u64().unwrap(), f["y"].as_u64().unwrap(), f["w"].as_u64().unwrap(), f["h"].as_u64().unwrap()])
            })
            .collect();
        for (k, r) in &rects {
            let want = match &k[..1] { "b" => 128, "c" => 64, _ => 32 };
            assert_eq!((r[2], r[3]), (want, want), "{k}");
            assert!(r[0] + r[2] <= img.w as u64 && r[1] + r[3] <= img.h as u64, "{k} inside the image");
        }
        // distinct cells never overlap
        for (i, (ka, a)) in rects.iter().enumerate() {
            for (kb, b) in &rects[i + 1..] {
                let overlap = a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3];
                assert!(!overlap || a == b, "{ka} {a:?} overlaps {kb} {b:?}");
            }
        }
        assert_eq!(meta["animations"]["b"]["size"], 128);
        assert!(meta["meta"]["spriteSize"].is_null());
        // the pixels landed where the map says: b's centre is opaque
        let b = rects.iter().find(|(k, _)| k == "b_0").unwrap().1;
        let (cx, cy) = ((b[0] + 64) as usize, (b[1] + 64) as usize);
        assert_eq!(img.px[(cy * img.w + cx) * 4 + 3], 255);
    }

    #[test]
    fn refuses_atlases_gpus_cannot_load() {
        let (v, f) = sprite("s", "none");
        // 300 stills at scale 8 need 300 cells of 256px; a 4096px side holds 16×16
        let many: Vec<_> = (0..300).map(|i| (json!({"name": format!("s{i}"), "prims": v["prims"]}), f.clone())).collect();
        let err = atlas(&many, 8, "a.png", ATLAS_MAX_SIDE).err().expect("too big").to_string();
        assert!(err.contains("over the 4096px limit"), "{err}");
        let (img, _) = atlas(&many, 2, "a.png", ATLAS_MAX_SIDE).unwrap();
        assert!(img.w <= ATLAS_MAX_SIDE && img.h <= ATLAS_MAX_SIDE);
    }
}
