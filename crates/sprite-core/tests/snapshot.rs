//! Byte-for-byte regression guard: every sample sprite must render exactly as it did when the
//! snapshot was taken, both at 32px and through the HD path. A change that is *meant* to alter
//! output bumps RENDER_VERSION and rewrites the snapshot with SNAPSHOT_WRITE=1.
//!
//! SNAPSHOT_LIB=<dir> SNAPSHOT_FILE=<file> runs the same check over any library folder.

use serde_json::{Map, Value};
use sprite_core::{render_anim, render_anim_hd, Palette, RenderOpts, Spec};
use std::path::PathBuf;

fn testdata() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

/// FNV-1a: stable across Rust versions, unlike DefaultHasher.
fn fnv(chunks: impl IntoIterator<Item = Vec<u8>>) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for c in chunks {
        for b in c.iter().chain(&(c.len() as u64).to_le_bytes()) {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    format!("{h:016x}")
}

fn specs() -> Vec<Value> {
    if let Ok(dir) = std::env::var("SNAPSHOT_LIB") {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
        files.sort();
        return files.iter().filter_map(|f| serde_json::from_slice(&std::fs::read(f).ok()?).ok()).collect();
    }
    ["animals", "food", "home", "world"]
        .iter()
        .flat_map(|f| serde_json::from_str::<Vec<Value>>(&std::fs::read_to_string(testdata().join(format!("nouns/{f}.json"))).unwrap()).unwrap())
        .collect()
}

#[test]
fn renders_are_byte_identical_to_the_snapshot() {
    let pal = Palette::new();
    let opts = RenderOpts::default();
    let mut got = Map::new();
    for v in specs() {
        let Ok(s) = Spec::from_json(&v, &pal) else { continue };
        let base = fnv(render_anim(&s, &pal, &opts).into_iter().map(|f| f.0));
        let hd = fnv(render_anim_hd(&s, &pal, &opts, 128).into_iter().map(|f| f.px));
        got.insert(s.name.clone(), Value::String(format!("{base} {hd}")));
    }
    let file = std::env::var("SNAPSHOT_FILE").map(PathBuf::from).unwrap_or_else(|_| testdata().join("golden/_snapshot.json"));
    if std::env::var("SNAPSHOT_WRITE").is_ok() {
        std::fs::write(&file, serde_json::to_string_pretty(&got).unwrap()).unwrap();
        return;
    }
    let want: Map<String, Value> = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(got.len(), want.len(), "sprite count");
    let changed: Vec<&String> = want.keys().filter(|k| got.get(*k) != want.get(*k)).collect();
    assert!(changed.is_empty(), "{} of {} sprites render differently: {changed:?}", changed.len(), want.len());
}
