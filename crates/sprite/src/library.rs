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

/// Pack every frame of every sprite into one PNG + a Phaser/Pixi JSON-hash frame map.
pub fn atlas(sprites: &[(Value, Vec<Frame>)], scale: usize, image_name: &str) -> Result<(Rgba, Value)> {
    let cols = sprites.iter().map(|s| s.1.len()).max().unwrap_or(1);
    let mut img = Rgba::new(cols * SIZE, sprites.len() * SIZE);
    let mut frames = serde_json::Map::new();
    let mut anims = serde_json::Map::new();
    for (r, (spec, fr)) in sprites.iter().enumerate() {
        let name = spec_name(spec);
        let mut keys = Vec::new();
        for (k, f) in fr.iter().enumerate() {
            img.blit_frame(f, k * SIZE, r * SIZE, 1);
            let key = format!("{name}_{k}");
            frames.insert(
                key.clone(),
                json!({"frame": {"x": k * SIZE * scale, "y": r * SIZE * scale, "w": SIZE * scale, "h": SIZE * scale}}),
            );
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
    let img = if scale > 1 { img.scaled(scale) } else { img };
    let meta = json!({
        "meta": {"image": image_name, "size": {"w": img.w, "h": img.h}, "scale": scale, "spriteSize": SIZE * scale},
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
