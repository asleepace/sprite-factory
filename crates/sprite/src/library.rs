//! Loading specs from batch files, single-spec files or a library folder; atlas export.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sprite_core::{Frame, SIZE};
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
/// Frames are packed into a near-square grid of SIZE×scale cells: each sprite's frames sit in a
/// run left to right (wrapping onto a new row only if the run is longer than a row), and frames
/// that are pixel-identical — a still sprite, a hold pose — share one cell, so the frame map can
/// point several keys at the same rectangle. Returns an error, before allocating anything, if
/// the result would be wider or taller than `max_side` px.
pub fn atlas(sprites: &[(Value, Vec<Frame>)], scale: usize, image_name: &str, max_side: usize) -> Result<(Rgba, Value)> {
    let scale = scale.max(1);
    let cell = SIZE * scale;
    let max_cols = (max_side / cell).max(1);
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
    let longest = uniq.iter().map(|u| u.0.len()).max().unwrap_or(1);
    // square-ish, but at least one sprite's whole run per row when it fits
    let cols = ((total as f64).sqrt().ceil() as usize).max(longest).clamp(1, max_cols);
    // place runs (shelf packing on a fixed grid)
    let (mut cx, mut cy) = (0usize, 0usize);
    let mut placed: Vec<Vec<(usize, usize)>> = Vec::with_capacity(uniq.len());
    for (cells, _) in &uniq {
        let run = cells.len();
        if cx > 0 && cx + run > cols && run <= cols {
            cx = 0;
            cy += 1;
        }
        let mut at = Vec::with_capacity(run);
        for _ in 0..run {
            if cx == cols {
                cx = 0;
                cy += 1;
            }
            at.push((cx, cy));
            cx += 1;
        }
        placed.push(at);
    }
    let rows = if cx == 0 { cy } else { cy + 1 }.max(1);
    let (w, h) = (cols * cell, rows * cell);
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
        for (c, &(x, y)) in cells.iter().zip(at) {
            img.blit_frame(c, x * cell, y * cell, scale);
        }
        let mut keys = Vec::with_capacity(fr.len());
        for (k, &u) in map.iter().enumerate() {
            let (x, y) = at[u];
            let key = format!("{name}_{k}");
            frames.insert(key.clone(), json!({"frame": {"x": x * cell, "y": y * cell, "w": cell, "h": cell}}));
            keys.push(key);
        }
        anims.insert(
            name.to_string(),
            json!({
                "frames": keys,
                "frameMs": spec.get("ms").and_then(Value::as_u64).unwrap_or(110),
                "durations": frame_durations(spec, fr.len()),
                "category": spec.get("cat"),
            }),
        );
    }
    let meta = json!({
        "meta": {"image": image_name, "size": {"w": img.w, "h": img.h}, "scale": scale, "spriteSize": cell},
        "frames": frames,
        "animations": anims,
    });
    Ok((img, meta))
}

pub fn save_sprite_outputs(dir: &Path, name: &str, frames: &[Frame], durations: &[u32]) -> Result<()> {
    let mut first = Rgba::new(SIZE, SIZE);
    first.blit_frame(&frames[0], 0, 0, 1);
    imgio::write(&dir.join(format!("{name}.png")), &first.png()?)?;
    imgio::write(&dir.join(format!("{name}_sheet.png")), &Rgba::strip(frames).png()?)?;
    if frames.len() > 1 {
        let (gif, _) = imgio::gif(frames, durations, &imgio::GifOpts { scale: 6, ..Default::default() })?;
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
