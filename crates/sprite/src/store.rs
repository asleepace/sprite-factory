//! The sprite library on disk: one `NAME.json` per sprite, plus
//!
//! ```text
//! _history/NAME/000003.json   every saved revision, byte for byte
//! _history/NAME/log.jsonl     one line per change: {"rev","op","by","at"}
//! _trash/NAME-<unix>.json     deleted sprites (also restorable from _history)
//! ```
//!
//! Writes go to a temp file that is renamed into place, so a crash never leaves half a spec.
//! Each spec carries a `rev` number; edits can pass the rev they read (`if_rev`) and are refused
//! if someone saved in between, instead of silently overwriting their work.

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One writer at a time per process. Writes are rare and quick next to rendering; the lock makes
/// read-modify-write (edit, create-if-absent, rev checks) atomic for every MCP session in `serve`.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

pub fn lock() -> std::sync::MutexGuard<'static, ()> {
    WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn spec_path(lib: &Path, name: &str) -> PathBuf {
    lib.join(format!("{name}.json"))
}

fn history_dir(lib: &Path, name: &str) -> PathBuf {
    lib.join("_history").join(name)
}

/// The rev stored in a spec (0 for specs saved before revs existed).
pub fn rev_of(v: &Value) -> u64 {
    v.get("rev").and_then(Value::as_u64).unwrap_or(0)
}

/// Write `bytes` to `path` atomically: temp file in the same directory, fsync, rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let file = path.file_name().and_then(|f| f.to_str()).unwrap_or("file");
    let tmp = dir.join(format!(".{file}.{}-{nanos}.tmp", std::process::id()));
    let res = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `2026-10-24T21:37:05Z` from unix seconds (UTC, proleptic Gregorian).
pub fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn log(lib: &Path, name: &str, rev: u64, op: &str, by: &str) -> std::io::Result<()> {
    let dir = history_dir(lib, name);
    std::fs::create_dir_all(&dir)?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("log.jsonl"))?;
    writeln!(f, "{}", json!({"rev": rev, "op": op, "by": by, "at": rfc3339(unix_now())}))
}

fn pretty(v: &Value) -> Vec<u8> {
    (serde_json::to_string_pretty(v).unwrap() + "\n").into_bytes()
}

/// Save `v` as the next revision of its sprite and return the new rev. Call with [`lock`] held.
/// If the sprite has no history yet (it predates revs), its current file is kept as revision 0
/// first, so the very first change is undoable too.
pub fn save(lib: &Path, name: &str, v: &mut Value, op: &str, by: &str) -> Result<u64, String> {
    let path = spec_path(lib, name);
    let hdir = history_dir(lib, name);
    let prev = std::fs::read(&path).ok();
    let prev_rev = prev.as_deref().and_then(|b| serde_json::from_slice::<Value>(b).ok()).map(|p| rev_of(&p));
    // a new sprite may reuse a deleted one's name: keep counting from its last rev
    let rev = prev_rev.unwrap_or_else(|| latest_rev(lib, name).unwrap_or(0)) + 1;
    if let (Some(bytes), Some(r)) = (&prev, prev_rev) {
        let snap = hdir.join(format!("{r:06}.json"));
        if !snap.exists() {
            write_atomic(&snap, bytes).map_err(|e| format!("could not keep history: {e}"))?;
            log(lib, name, r, "import", "-").map_err(|e| e.to_string())?;
        }
    }
    v.as_object_mut().ok_or("spec is not an object")?.insert("rev".into(), json!(rev));
    let bytes = pretty(v);
    write_atomic(&hdir.join(format!("{rev:06}.json")), &bytes).map_err(|e| format!("could not keep history: {e}"))?;
    write_atomic(&path, &bytes).map_err(|e| format!("could not save {name}: {e}"))?;
    log(lib, name, rev, op, by).map_err(|e| e.to_string())?;
    Ok(rev)
}

/// Move a sprite to `_trash` (its history stays). Call with [`lock`] held.
pub fn delete(lib: &Path, name: &str, by: &str) -> Result<u64, String> {
    let path = spec_path(lib, name);
    let bytes = std::fs::read(&path).map_err(|_| format!("No sprite named \"{name}\"."))?;
    let rev = serde_json::from_slice::<Value>(&bytes).map(|v| rev_of(&v)).unwrap_or(0);
    let snap = history_dir(lib, name).join(format!("{rev:06}.json"));
    if !snap.exists() {
        write_atomic(&snap, &bytes).map_err(|e| e.to_string())?;
    }
    let trash = lib.join("_trash");
    std::fs::create_dir_all(&trash).map_err(|e| e.to_string())?;
    std::fs::rename(&path, trash.join(format!("{name}-{}.json", unix_now()))).map_err(|e| e.to_string())?;
    log(lib, name, rev, "delete", by).map_err(|e| e.to_string())?;
    Ok(rev)
}

/// The highest revision kept for `name`, live or deleted.
pub fn latest_rev(lib: &Path, name: &str) -> Option<u64> {
    revisions(lib, name).into_iter().map(|r| r.rev).max()
}

#[derive(Debug, Clone)]
pub struct Revision {
    pub rev: u64,
    pub op: String,
    pub by: String,
    pub at: String,
}

/// Every kept revision of `name`, oldest first, with who made it and when (from the log).
pub fn revisions(lib: &Path, name: &str) -> Vec<Revision> {
    let dir = history_dir(lib, name);
    let mut log_lines: Vec<Value> = std::fs::read_to_string(dir.join("log.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    log_lines.retain(|l| l.get("op").and_then(Value::as_str) != Some("delete"));
    let mut revs: Vec<u64> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().to_str().and_then(|f| f.strip_suffix(".json")).and_then(|s| s.parse().ok()))
                .collect()
        })
        .unwrap_or_default();
    revs.sort_unstable();
    revs.into_iter()
        .map(|rev| {
            let l = log_lines.iter().rev().find(|l| l.get("rev").and_then(Value::as_u64) == Some(rev));
            let s = |k: &str| l.and_then(|l| l.get(k)).and_then(Value::as_str).unwrap_or("?").to_string();
            Revision { rev, op: s("op"), by: s("by"), at: s("at") }
        })
        .collect()
}

/// The spec as it was at `rev`.
pub fn load_rev(lib: &Path, name: &str, rev: u64) -> Result<Value, String> {
    let p = history_dir(lib, name).join(format!("{rev:06}.json"));
    let bytes = std::fs::read(&p).map_err(|_| format!("\"{name}\" has no revision {rev}; sprite_history lists them."))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("revision {rev} of {name} is not valid JSON: {e}"))
}

/// Was this sprite deleted at some point and not recreated? (for "did you mean to restore")
pub fn is_deleted(lib: &Path, name: &str) -> bool {
    !spec_path(lib, name).exists() && history_dir(lib, name).exists()
}

/// Load every `NAME.json` in the library, skipping (and reporting) files that don't parse, so one
/// bad file can't make the whole library look empty.
pub fn load_all(lib: &Path) -> (Vec<Value>, Vec<String>) {
    let mut files: Vec<PathBuf> = std::fs::read_dir(lib)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "json"))
                .filter(|p| p.file_stem().and_then(|s| s.to_str()).is_some_and(crate::mcp::valid_name))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    let (mut ok, mut bad) = (Vec::new(), Vec::new());
    for f in files {
        let name = f.file_stem().unwrap().to_string_lossy().to_string();
        match std::fs::read(&f).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| e.to_string())) {
            Ok(v) if v.is_object() => ok.push(v),
            Ok(_) => bad.push(format!("{name}.json: not a spec object")),
            Err(e) => bad.push(format!("{name}.json: {e}")),
        }
    }
    (ok, bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> PathBuf {
        let d = std::env::temp_dir().join(format!("sprite-store-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rfc3339_formats_known_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_793_050_625), "2026-10-26T21:37:05Z");
    }

    #[test]
    fn revisions_trash_and_legacy_files() {
        let l = lib();
        // a spec written before revs existed becomes revision 0 on its first save
        std::fs::write(spec_path(&l, "cup"), r#"{"name":"cup","prims":[{"t":"rect","p":[8,8,24,28]}]}"#).unwrap();
        let mut v = json!({"name": "cup", "prims": [{"t": "rect", "p": [8, 8, 24, 29]}]});
        assert_eq!(save(&l, "cup", &mut v, "edit", "colin").unwrap(), 1);
        let mut v2 = v.clone();
        assert_eq!(save(&l, "cup", &mut v2, "edit", "colin").unwrap(), 2);
        let revs: Vec<(u64, String)> = revisions(&l, "cup").into_iter().map(|r| (r.rev, r.op)).collect();
        assert_eq!(revs, vec![(0, "import".into()), (1, "edit".into()), (2, "edit".into())]);
        assert!(load_rev(&l, "cup", 0).unwrap()["prims"][0]["p"][3] == 28);

        assert_eq!(delete(&l, "cup", "colin").unwrap(), 2);
        assert!(!spec_path(&l, "cup").exists());
        assert!(is_deleted(&l, "cup"));
        assert_eq!(std::fs::read_dir(l.join("_trash")).unwrap().count(), 1);
        // recreating the name keeps counting, so old revisions are never overwritten
        let mut v3 = json!({"name": "cup", "prims": [{"t": "ellipse", "c": [16, 16], "r": 8}]});
        assert_eq!(save(&l, "cup", &mut v3, "create", "colin").unwrap(), 3);
        assert_eq!(load_rev(&l, "cup", 2).unwrap()["prims"][0]["t"], "rect");
    }

    #[test]
    fn one_bad_file_does_not_hide_the_rest() {
        let l = lib();
        std::fs::write(l.join("apple.json"), r#"{"name":"apple","prims":[]}"#).unwrap();
        std::fs::write(l.join("broken.json"), "{ not json").unwrap();
        std::fs::write(l.join(".apple.json.1-2.tmp"), "partial").unwrap();
        let (ok, bad) = load_all(&l);
        assert_eq!(ok.len(), 1);
        assert_eq!(bad.len(), 1);
        assert!(bad[0].starts_with("broken.json"));
    }
}
