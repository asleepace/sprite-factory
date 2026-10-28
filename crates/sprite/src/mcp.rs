//! `sprite mcp` — the Sprite Factory MCP server over stdio.
//!
//! Same tools as the TypeScript server, minus the Python bridge: rendering happens in-process.
//! blind_review tries, in order: a direct Anthropic API call (ANTHROPIC_API_KEY), MCP sampling
//! (MRTR input request on protocol ≥ 2026-07-28, server→client request on older protocols),
//! then a fallback that returns the unlabeled sheet and answer key.
#![allow(deprecated)] // sampling types are deprecated upstream (SEP-2577) but still how clients review

use anyhow::Result;
use base64::Engine;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, CreateMessageRequest, CreateMessageRequestParams,
    CreateMessageResult, Implementation, InputRequest, InputRequiredResult, ListToolsResult, PaginatedRequestParams,
    ProtocolVersion, Role, SamplingContent, SamplingMessage, SamplingMessageContentBlock, ServerCapabilities, ServerConfig,
    Tool,
};
use rmcp::model::{ContextInclusion, ImageContent, ResourceContents};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use serde_json::{json, Map, Value};
use sprite_core::lint::lint;
use sprite_core::{render_anim, Frame, Palette, RenderOpts, Spec};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::imgio::{self, Rgba, BACKDROP};
use crate::library;
use crate::store;

const CATEGORIES: [&str; 8] = ["animals", "food", "home", "nature", "vehicles", "places", "people", "misc"];
const MATERIALS: [&str; 24] = [
    "red", "orange", "yellow", "gold", "lime", "green", "forest", "teal", "sky", "blue", "navy", "purple", "pink",
    "brown", "wood", "tan", "skin", "cream", "white", "gray", "steel", "charcoal", "black", "glass",
];
const GUIDE: &str = include_str!("../../../AUTHORING.md");

pub fn serve(library: PathBuf) -> Result<()> {
    std::fs::create_dir_all(&library)?;
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let server = SpriteServer::new(library, Reviews::default());
        let running = server.serve(rmcp::transport::stdio()).await?;
        running.waiting().await?;
        Ok(())
    })
}

/// review token → sprite names in sheet order. Kept server-side (requestState is untrusted) and
/// shared across server instances, because over HTTP each MRTR leg can land on a fresh instance.
pub(crate) type Reviews = Arc<Mutex<HashMap<String, (std::time::Instant, Vec<String>)>>>;

#[derive(Clone)]
pub(crate) struct SpriteServer {
    lib: Arc<PathBuf>,
    reviews: Reviews,
    /// where this server's REST API is reachable (SPRITE_PUBLIC_URL), for download links
    public_url: Option<Arc<str>>,
}

impl SpriteServer {
    pub(crate) fn new(library: PathBuf, reviews: Reviews) -> Self {
        let public_url = std::env::var("SPRITE_PUBLIC_URL").ok().filter(|u| u.starts_with("http")).map(Arc::from);
        SpriteServer { lib: Arc::new(library), reviews, public_url }
    }
}

// ───────────────────────── tool schemas ─────────────────────────

fn prim_schema() -> Value {
    json!({
        "type": "object",
        "description": "One shape. Coordinates in the sprite's canvas pixels (0–32 by default, 0–64 or 0–128 with size), y down. See style_guide for every field.",
        "properties": {
            "t": {"type": "string", "enum": ["ellipse", "rect", "poly", "arc", "star", "ngon", "line", "px"]},
            "m": {"type": "string", "enum": MATERIALS, "description": "material (colour ramp); never raw colours"},
            "c": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2, "description": "centre [x,y]"},
            "r": {"description": "radius or [rx,ry]", "anyOf": [{"type": "number"}, {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2}]},
            "p": {"type": "array", "items": {"type": "number"}, "minItems": 4, "maxItems": 4, "description": "rect [x0,y0,x1,y1]"},
            "pts": {"type": "array", "maxItems": sprite_core::spec::limits::MAX_PTS, "items": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2}},
            "w": {"type": "number", "description": "line width"},
            "puff": {"type": "number"}, "height": {"type": "number"}, "round": {"type": "number"},
            "smooth": {"type": "integer", "minimum": 0, "maximum": 4},
            "flat": {"type": "boolean"}, "shade": {"type": "integer", "minimum": 0, "maximum": 4},
            "tone": {"type": "integer", "minimum": -2, "maximum": 2},
            "gloss": {"type": "boolean"}, "edge": {"type": "boolean"}, "weight": {"type": "number"},
            "rot": {"type": "number"}, "cut": {"type": "boolean"}, "clip": {"type": "integer"},
            "lit": {"type": "boolean"}, "cap": {"type": "boolean"},
            "g": {"type": "string", "description": "animation group"},
            "r2": {"type": "number"}, "n": {"type": "integer", "minimum": 3, "maximum": 64}, "a": {"type": "number"},
            "a0": {"type": "number"}, "a1": {"type": "number"}, "pie": {"type": "boolean"}
        },
        "required": ["t"],
        "additionalProperties": true
    })
}

fn track_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "g": {"type": "string", "description": "group name, or * for the whole sprite"},
            "prop": {"type": "string", "enum": ["dx", "dy", "rot", "sx", "sy"]},
            "amp": {"type": "number"},
            "wave": {"type": "string", "enum": ["sin", "abs", "abs2", "saw", "square"]},
            "phase": {"type": "number"},
            "pivot": {"anyOf": [{"type": "string", "enum": ["center", "bottom", "top", "each"]}, {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2}]}
        },
        "required": ["prop", "amp"]
    })
}

fn name_schema() -> Value {
    json!({"type": "string", "pattern": "^[a-z][a-z0-9_]{0,40}$", "description": "lowercase snake_case noun, e.g. teddy_bear"})
}

fn anim_schema() -> Value {
    json!({"type": "string", "enum": sprite_core::anim::PRESET_NAMES})
}

fn size_schema() -> Value {
    json!({"type": "integer", "enum": sprite_core::spec::limits::SIZES,
        "description": "canvas side in px (default 32). 64 or 128 for characters with hands and faces, or small props that need detail. Coordinates are in these pixels."})
}

/// Timing fields shared by create_sprite and edit_sprite.
fn timing_props() -> [(&'static str, Value); 4] {
    [
        ("frames", json!({"type": "integer", "minimum": 1, "maximum": sprite_core::MAX_FRAMES, "description": "frames per loop (default 8)"})),
        ("ms", json!({"type": "integer", "minimum": 16, "description": "ms per frame (default 110)"})),
        ("hold", json!({"type": "array", "items": {"type": "integer", "minimum": 1, "maximum": 100},
            "description": "per-frame hold in units of ms, e.g. [3,1,1,1,3,1,1,1] lingers on the poses; missing entries are 1"})),
        ("snap", json!({"type": "boolean", "description": "round dx/dy motion to whole pixels for crisp, stepped movement"})),
    ]
}

const TIMING_KEYS: [&str; 4] = ["frames", "ms", "hold", "snap"];
/// export_gif returns the GIF inline only up to this size, so a big one can't flood the context.
const MAX_INLINE_GIF: usize = 1 << 20;

fn obj(props: Value, required: &[&str]) -> Arc<Map<String, Value>> {
    let v = json!({"type": "object", "properties": props, "required": required});
    Arc::new(v.as_object().unwrap().clone())
}

fn with_timing(mut props: Value) -> Value {
    let o = props.as_object_mut().unwrap();
    for (k, v) in timing_props() {
        o.insert(k.into(), v);
    }
    props
}

fn tools() -> Vec<Tool> {
    let t = |name: &'static str, title: &str, desc: &'static str, schema: Arc<Map<String, Value>>| {
        Tool::new(name, desc, schema).with_title(title)
    };
    vec![
        t("style_guide", "Style guide", "Returns the authoring guide (shape DSL, materials, animation, review checklist). Read before creating sprites.", obj(json!({}), &[])),
        t("list_sprites", "List sprites", "Lists sprites in the library, optionally by category. sheet=true adds a labelled contact sheet image.",
            obj(json!({"category": {"type": "string", "enum": CATEGORIES}, "sheet": {"type": "boolean"}}), &[])),
        t("get_sprite", "Get sprite spec", "Returns a sprite's JSON spec (useful as a reference for a related sprite).", obj(json!({"name": name_schema()}), &["name"])),
        t("create_sprite", "Create sprite",
            "Creates a sprite from shapes, renders it and saves it. Returns the sprite, its animation frames and style warnings. Keep shapes inside 1.5–(size − 1.5): 30.5 at 32px, 62.5 at 64, 126.5 at 128.",
            obj(with_timing(json!({
                "name": name_schema(),
                "size": size_schema(),
                "category": {"type": "string", "enum": CATEGORIES},
                "aka": {"type": "array", "items": {"type": "string"}, "description": "other words a child might use (bunny for rabbit); used by blind_review"},
                "anim": anim_schema(),
                "tracks": {"type": "array", "items": track_schema(), "maxItems": sprite_core::spec::limits::MAX_TRACKS},
                "prims": {"type": "array", "items": prim_schema(), "minItems": 1, "maxItems": sprite_core::spec::limits::MAX_PRIMS},
                "overwrite": {"type": "boolean"}
            })), &["name", "category", "prims"])),
        t("edit_sprite", "Edit sprite",
            "Changes an existing sprite and re-renders it. prim_ops (set/merge/insert/delete by index, applied in order) for surgical edits, or prims to replace all shapes. \
             Changing size doesn't move shapes, so it needs prims redrawn for the new canvas.",
            obj(with_timing(json!({
                "name": name_schema(),
                "size": size_schema(),
                "anim": anim_schema(),
                "tracks": {"type": "array", "items": track_schema(), "maxItems": sprite_core::spec::limits::MAX_TRACKS},
                "aka": {"type": "array", "items": {"type": "string"}},
                "prims": {"type": "array", "items": prim_schema(), "minItems": 1, "maxItems": sprite_core::spec::limits::MAX_PRIMS},
                "if_rev": {"type": "integer", "minimum": 0, "description": "the rev you read (from get_sprite or the last create/edit); the edit is refused if someone saved since"},
                "prim_ops": {"type": "array", "items": {"type": "object", "properties": {
                    "op": {"type": "string", "enum": ["set", "merge", "insert", "delete"]},
                    "index": {"type": "integer", "minimum": 0},
                    "prim": {"type": "object", "description": "set/insert: full prim; merge: only the fields to change", "additionalProperties": true}
                }, "required": ["op", "index"]}}
            })), &["name"])),
        t("render_sprite", "Render sprite", "Renders an existing sprite without changing it.",
            obj(json!({"name": name_schema(), "scale": {"type": "integer", "minimum": 1, "maximum": 16,
                "description": "upscale; default about 256px wide (8 at 32px, 4 at 64, 2 at 128), at most 512px"}}), &["name"])),
        t("delete_sprite", "Delete sprite", "Removes a sprite from the library. Its history is kept, so restore_sprite can bring it back.", obj(json!({"name": name_schema()}), &["name"])),
        t("sprite_history", "Sprite history", "Lists a sprite's saved revisions (rev, what changed, who, when), including sprites that were deleted.",
            obj(json!({"name": name_schema()}), &["name"])),
        t("restore_sprite", "Restore sprite",
            "Puts a sprite back to an earlier revision (saved as a new revision, so nothing is lost). Also undeletes. Returns the render.",
            obj(json!({"name": name_schema(), "rev": {"type": "integer", "minimum": 0}}), &["name", "rev"])),
        t("blind_review", "Blind review",
            "Tests whether sprites are recognisable without their names: a fresh model sees an unlabeled, shuffled sheet and its guesses are graded. Uses ANTHROPIC_API_KEY if set, else MCP sampling, else returns the sheet and key for a separate reviewer.",
            obj(json!({"names": {"type": "array", "items": name_schema(), "minItems": 1, "maxItems": 30}}), &["names"])),
        t("export_gif", "Export GIF",
            "Renders a sprite as an animated GIF and returns it as a preview image and as a GIF file resource the client can save \
             (plus a download URL when the server has a public address). \
             Uses the renderer's exact palette, so colours never shift and the background can be truly transparent. \
             Timing comes from the sprite's ms and hold. With a write token, also saved to the library's _export folder.",
            obj(json!({
                "name": name_schema(),
                "scale": {"type": "integer", "minimum": 1, "maximum": 16, "description": "nearest-neighbour upscale; default makes it 256px (8 at 32px, 4 at 64, 2 at 128), at most 512px"},
                "bg": {"type": "string", "description": "\"transparent\" (default) or a hex colour like #f6f5f0"},
                "pingpong": {"type": "boolean", "description": "play forward then backward instead of wrapping"},
                "repeat": {"type": "integer", "minimum": 1, "description": "times to play; omit to loop forever"}
            }), &["name"])),
        t("export_atlas", "Export atlas",
            "Packs sprites (all, a category, or named ones) into a PNG atlas + JSON frame map with animation timings (Phaser/Pixi JSON-hash style), written to the library's _export folder.",
            obj(json!({
                "names": {"type": "array", "items": name_schema()},
                "category": {"type": "string", "enum": CATEGORIES},
                "scale": {"type": "integer", "minimum": 1, "maximum": 8},
                "max_side": {"type": "integer", "minimum": 256, "maximum": library::ATLAS_SIDE_LIMIT, "description": "largest atlas width/height in px (default 4096, which every GPU loads)"},
                "file": {"type": "string", "pattern": "^[a-z0-9_-]{1,40}$"}
            }), &[])),
    ]
}

// ───────────────────────── helpers ─────────────────────────

type ToolOut = Result<CallToolResult, String>;

fn text(s: impl Into<String>) -> ContentBlock {
    ContentBlock::text(s)
}

fn image_b64(png: &[u8]) -> ContentBlock {
    ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(png), "image/png")
}

pub(crate) fn valid_name(n: &str) -> bool {
    let mut c = n.chars();
    matches!(c.next(), Some('a'..='z')) && n.len() <= 41 && c.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn arg_str<'a>(a: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str)
}

fn norm(s: &str) -> String {
    let t: String = s.to_lowercase().chars().map(|c| if c == '_' || c == '-' { ' ' } else { c }).filter(|c| c.is_ascii_lowercase() || *c == ' ').collect();
    let t = t.trim().to_string();
    if let Some(stem) = t.strip_suffix("ies") {
        format!("{stem}y")
    } else if let Some(stem) = t.strip_suffix('s') {
        stem.to_string()
    } else {
        t
    }
}

fn render_spec(v: &Value) -> Result<(Spec, Vec<Frame>), Vec<String>> {
    let pal = Palette::new();
    let s = Spec::from_json(v, &pal)?;
    let f = render_anim(&s, &pal, &RenderOpts::default());
    Ok((s, f))
}

/// Upscale for a preview of a `side`px sprite: `want` if it keeps the image within 512px,
/// otherwise the largest that does; by default about 256px.
fn preview_scale(side: usize, want: Option<u64>) -> Result<usize, String> {
    let max = (512 / side).max(1);
    match want {
        None => Ok((256 / side).max(1)),
        Some(s) if (1..=max as u64).contains(&s) => Ok(s as usize),
        Some(_) => Err(format!("scale must be a whole number from 1 to {max} for a {side}px sprite (at most 512px)")),
    }
}

fn render_result(v: &Value, headline: &str, scale: Option<u64>) -> ToolOut {
    let (s, frames) = render_spec(v).map_err(|e| format!("Not saved — the spec is invalid:\n- {}", e.join("\n- ")))?;
    let side = s.size;
    let scale = preview_scale(side, scale)?;
    let l = lint(&s, &frames);
    let mut lines = vec![
        headline.to_string(),
        format!("stats: {}", json!({"bbox": l.bbox, "span_px": l.span_px, "colors": l.colors, "frames": l.frames, "prims": l.prims})),
    ];
    lines.push(if l.warnings.is_empty() { "style warnings: none".into() } else { format!("style warnings:\n- {}", l.warnings.join("\n- ")) });
    lines.push("Look at the image: would a 5-year-old name it instantly? Revise with edit_sprite if not.".into());
    let mut big = Rgba::new(side * scale, side * scale);
    big.blit_frame(&frames[0], 0, 0, scale);
    let still = big.on_backdrop(BACKDROP).png().map_err(|e| e.to_string())?;
    let mut content = vec![text(lines.join("\n")), image_b64(&still)];
    if frames.len() > 1 {
        let strip = Rgba::strip(&frames).scaled((scale / 2).max(64 / side).max(1)).on_backdrop(BACKDROP).png().map_err(|e| e.to_string())?;
        content.push(text(format!("animation frames ({}):", s.anim)));
        content.push(image_b64(&strip));
    }
    Ok(CallToolResult::success(content))
}

/// Replace the first line of a render_result's text (used once the save tells us the rev).
fn set_headline(res: &mut CallToolResult, head: &str) {
    if let Some(ContentBlock::Text(t)) = res.content.first_mut() {
        t.text = format!("{head}{}", t.text);
    }
}

/// Who is calling and whether they may use the keyed tools: the token label (or "anon") under
/// `sprite serve`, or a privileged "local" over stdio. An HTTP request that somehow skipped the
/// auth middleware is anonymous, never privileged.
fn caller(ctx: &RequestContext<RoleServer>) -> (String, bool) {
    match ctx.extensions.get::<axum::http::request::Parts>() {
        None => ("local".into(), true),
        Some(p) => p.extensions.get::<crate::http::Caller>().map_or_else(|| ("anon".into(), false), |c| (c.label.clone(), c.privileged)),
    }
}

/// How many sprites tokenless callers may create per hour, across all of them
/// (SPRITE_ANON_CREATES_PER_HOUR, default 60). Keeps the public library and atlas from being flooded.
fn anon_create_allowed() -> Result<(), String> {
    static RECENT: Mutex<Vec<std::time::Instant>> = Mutex::new(Vec::new());
    let limit: usize = std::env::var("SPRITE_ANON_CREATES_PER_HOUR").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
    let hour = std::time::Duration::from_secs(3600);
    let mut recent = RECENT.lock().unwrap();
    recent.retain(|t| t.elapsed() < hour);
    if recent.len() >= limit {
        return Err(format!("Tokenless callers have created {limit} sprites in the last hour, the limit. Try later, or use a write token."));
    }
    recent.push(std::time::Instant::now());
    Ok(())
}

impl SpriteServer {
    /// Tools that change or delete an existing sprite, spend API credits, or write export files
    /// need a write token. Everything else (reading, rendering, export_gif, creating a new sprite)
    /// is public; create_sprite itself refuses to replace or revive a sprite without one.
    pub(crate) fn keyed(tool: &str) -> bool {
        matches!(tool, "edit_sprite" | "delete_sprite" | "restore_sprite" | "blind_review" | "export_atlas")
    }

    fn path(&self, name: &str) -> PathBuf {
        self.lib.join(format!("{name}.json"))
    }

    fn load(&self, name: &str) -> Result<Value, String> {
        if !valid_name(name) {
            return Err(format!("\"{name}\" is not a valid sprite name (lowercase snake_case)"));
        }
        let p = self.path(name);
        let s = std::fs::read_to_string(&p).map_err(|_| format!("No sprite named \"{name}\". Use list_sprites to see what exists."))?;
        serde_json::from_str(&s).map_err(|e| format!("{name}.json is not valid JSON: {e}"))
    }

    fn all(&self) -> (Vec<Value>, Vec<String>) {
        store::load_all(&self.lib)
    }

    /// Save `v` as a new revision, failing if the sprite moved past `expect_rev` (the rev the
    /// caller started from; None = there was no readable spec). The lock is held only for the
    /// check and the write, never while rendering.
    fn commit(&self, name: &str, v: &mut Value, expect_rev: Option<u64>, op: &str, who: &str) -> Result<u64, String> {
        let _g = store::lock();
        let bytes = std::fs::read(self.path(name)).ok();
        let current = bytes.as_deref().and_then(|b| serde_json::from_slice::<Value>(b).ok());
        match (expect_rev, &current) {
            (None, Some(_)) if op != "overwrite" => {
                return Err(format!("\"{name}\" was created by someone else just now. Nothing was saved; use edit_sprite, or pass overwrite: true."))
            }
            (None, None) if op == "create" && bytes.is_some() => {
                return Err(format!("{name}.json appeared just now. Nothing was saved; pass overwrite: true to replace it."))
            }
            (Some(want), Some(cur)) if store::rev_of(cur) != want => {
                return Err(format!(
                    "\"{name}\" changed since you read it (now rev {}, you started from rev {want}). Nothing was saved; get_sprite and try again.",
                    store::rev_of(cur)
                ))
            }
            (Some(_), None) if op == "edit" || op == "restore" => return Err(format!("\"{name}\" was deleted since you read it. Nothing was saved.")),
            _ => {}
        }
        store::save(&self.lib, name, v, op, who)
    }

    // ── tools ──

    fn list_sprites(&self, a: &Map<String, Value>) -> ToolOut {
        let (mut specs, broken) = self.all();
        if let Some(c) = arg_str(a, "category") {
            specs.retain(|s| s.get("cat").and_then(Value::as_str) == Some(c));
        }
        let mut by_cat: Vec<(String, Vec<String>)> = Vec::new();
        for s in &specs {
            let c = s.get("cat").and_then(Value::as_str).unwrap_or("misc").to_string();
            match by_cat.iter_mut().find(|(k, _)| *k == c) {
                Some((_, v)) => v.push(library::spec_name(s).into()),
                None => by_cat.push((c, vec![library::spec_name(s).into()])),
            }
        }
        let summary = if by_cat.is_empty() {
            "library is empty".to_string()
        } else {
            by_cat.iter().map(|(c, ns)| format!("{c} ({}): {}", ns.len(), ns.join(", "))).collect::<Vec<_>>().join("\n")
        };
        let mut head = format!("{} sprites\n{summary}", specs.len());
        if !broken.is_empty() {
            head.push_str(&format!("\n\n{} file(s) could not be read and were skipped:\n- {}", broken.len(), broken.join("\n- ")));
        }
        let mut content = vec![text(head)];
        if a.get("sheet").and_then(Value::as_bool).unwrap_or(false) && !specs.is_empty() {
            let items: Vec<(String, Frame)> = specs
                .iter()
                .filter_map(|v| render_spec(v).ok().map(|(_, f)| (library::spec_name(v).to_string(), f[0].clone())))
                .collect();
            content.push(image_b64(&imgio::contact_sheet(&items, 2, 12).png().map_err(|e| e.to_string())?));
        }
        Ok(CallToolResult::success(content))
    }

    fn create_sprite(&self, a: &Map<String, Value>, who: &str, privileged: bool) -> ToolOut {
        let name = arg_str(a, "name").unwrap_or_default();
        if !valid_name(name) {
            return Err("name must be lowercase snake_case, e.g. teddy_bear".into());
        }
        let cat = arg_str(a, "category").unwrap_or("misc");
        if !CATEGORIES.contains(&cat) {
            return Err(format!("category must be one of {CATEGORIES:?}"));
        }
        let overwrite = a.get("overwrite").and_then(Value::as_bool).unwrap_or(false);
        let on_disk = std::fs::read(self.path(name)).ok();
        let existing = on_disk.as_deref().and_then(|b| serde_json::from_slice::<Value>(b).ok());
        if on_disk.is_some() && !overwrite {
            return Err(match existing {
                Some(_) => format!("\"{name}\" already exists. Use edit_sprite, or pass overwrite: true to replace it."),
                None => format!("{name}.json exists but is not valid JSON. Pass overwrite: true to replace it; the damaged file is kept in sprite_history."),
            });
        }
        let was_deleted = store::is_deleted(&self.lib, name);
        // checked against what's on disk, not the overwrite flag: replacing a sprite is an edit, and
        // bringing back a deleted name is a restore
        if !privileged {
            if on_disk.is_some() {
                return Err(format!("Replacing \"{name}\" needs a write token sent as an Authorization: Bearer header. Pick a new name instead."));
            }
            if was_deleted {
                return Err(format!("\"{name}\" was deleted; reusing its name needs a write token sent as an Authorization: Bearer header. Pick a new name instead."));
            }
            anon_create_allowed()?;
        }
        let mut spec = Map::new();
        spec.insert("name".into(), json!(name));
        spec.insert("cat".into(), json!(cat));
        if let Some(size) = a.get("size").filter(|x| !x.is_null()) {
            spec.insert("size".into(), size.clone());
        }
        if let Some(aka) = a.get("aka") {
            spec.insert("aka".into(), aka.clone());
        }
        spec.insert("anim".into(), a.get("anim").cloned().unwrap_or(json!("none")));
        for k in TIMING_KEYS {
            if let Some(x) = a.get(k).filter(|x| !x.is_null()) {
                spec.insert(k.into(), x.clone());
            }
        }
        spec.insert("tracks".into(), a.get("tracks").cloned().unwrap_or(json!([])));
        spec.insert("prims".into(), a.get("prims").cloned().unwrap_or(json!([])));
        let mut v = Value::Object(spec);
        // validate + render before touching the library
        let mut res = render_result(&v, "", None)?;
        let expect = existing.as_ref().map(store::rev_of);
        let rev = self.commit(name, &mut v, expect, if on_disk.is_some() { "overwrite" } else { "create" }, who)?;
        let mut head = format!("Created \"{name}\" (rev {rev}).");
        if was_deleted {
            head.push_str(" A deleted sprite had this name; its old revisions are still in sprite_history.");
        }
        set_headline(&mut res, &head);
        Ok(res)
    }

    fn edit_sprite(&self, a: &Map<String, Value>, who: &str) -> ToolOut {
        let name = arg_str(a, "name").unwrap_or_default();
        let mut v = self.load(name)?;
        let base_rev = store::rev_of(&v);
        if let Some(want) = a.get("if_rev").and_then(Value::as_u64) {
            if want != base_rev {
                return Err(format!(
                    "\"{name}\" is at rev {base_rev}, not {want}: someone saved since you read it. Nothing was changed; get_sprite and redo the edit."
                ));
            }
        }
        let o = v.as_object_mut().ok_or("stored spec is not an object")?;
        if let Some(size) = a.get("size").filter(|x| !x.is_null()) {
            let old = o.get("size").and_then(Value::as_u64).unwrap_or(32);
            if size.as_u64() != Some(old) && a.get("prims").is_none() {
                return Err(format!(
                    "Changing size from {old} to {size} doesn't move or resize any shapes (coordinates are canvas pixels). \
                     Send prims redrawn for the new canvas along with size. Nothing was changed."
                ));
            }
            o.insert("size".into(), size.clone());
        }
        for k in ["anim", "tracks", "aka", "prims"] {
            if let Some(x) = a.get(k) {
                o.insert(k.into(), x.clone());
            }
        }
        for k in TIMING_KEYS {
            match a.get(k) {
                // null puts a timing field back to its default
                Some(Value::Null) => {
                    o.remove(k);
                }
                Some(x) => {
                    o.insert(k.into(), x.clone());
                }
                None => {}
            }
        }
        if let Some(ops) = a.get("prim_ops").and_then(Value::as_array) {
            let prims = o.get_mut("prims").and_then(Value::as_array_mut).ok_or("spec has no prims")?;
            for op in ops {
                let kind = op.get("op").and_then(Value::as_str).unwrap_or("");
                let idx = op.get("index").and_then(Value::as_u64).ok_or("each prim_op needs an index")? as usize;
                let prim = op.get("prim").cloned();
                let n = prims.len();
                match kind {
                    "insert" => {
                        let p = prim.filter(|p| p.get("t").is_some()).ok_or("insert needs a prim with a type (t)")?;
                        if idx > n {
                            return Err(format!("insert at {idx} needs index ≤ {n}"));
                        }
                        prims.insert(idx, p);
                    }
                    _ if idx >= n => return Err(format!("prim index {idx} out of range (sprite has {n} prims)")),
                    "delete" => {
                        prims.remove(idx);
                    }
                    "set" => prims[idx] = prim.filter(|p| p.get("t").is_some()).ok_or("set needs a full prim with a type (t)")?,
                    "merge" => {
                        let patch = prim.and_then(|p| p.as_object().cloned()).ok_or("merge needs a prim object")?;
                        let tgt = prims[idx].as_object_mut().ok_or("prim is not an object")?;
                        for (k, val) in patch {
                            tgt.insert(k, val);
                        }
                    }
                    other => return Err(format!("unknown op {other:?}")),
                }
            }
        }
        let mut res = render_result(&v, "", None)?;
        let rev = self.commit(name, &mut v, Some(base_rev), "edit", who)?;
        set_headline(&mut res, &format!("Updated \"{name}\" (rev {rev})."));
        Ok(res)
    }

    fn delete_sprite(&self, a: &Map<String, Value>, who: &str) -> ToolOut {
        let n = arg_str(a, "name").unwrap_or_default();
        self.load(n)?;
        let rev = {
            let _g = store::lock();
            store::delete(&self.lib, n, who)?
        };
        Ok(CallToolResult::success(vec![text(format!(
            "Deleted \"{n}\" (was rev {rev}). Undo with restore_sprite {{\"name\": \"{n}\", \"rev\": {rev}}}."
        ))]))
    }

    fn sprite_history(&self, a: &Map<String, Value>) -> ToolOut {
        let n = arg_str(a, "name").unwrap_or_default();
        if !valid_name(n) {
            return Err(format!("\"{n}\" is not a valid sprite name (lowercase snake_case)"));
        }
        let revs = store::revisions(&self.lib, n);
        if revs.is_empty() {
            return Err(format!("No history for \"{n}\"."));
        }
        let live = std::fs::read(self.path(n)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).map(|v| store::rev_of(&v));
        let lines: Vec<String> = revs
            .iter()
            .map(|r| format!("rev {:>3}  {:<9} {:<12} {}{}", r.rev, r.op, r.by, r.at, if Some(r.rev) == live { "  ← current" } else { "" }))
            .collect();
        let status = if live.is_some() { "" } else { "\n(currently deleted — restore_sprite brings any revision back)" };
        Ok(CallToolResult::success(vec![text(format!("{n}: {} revision(s){status}\n{}", revs.len(), lines.join("\n")))]))
    }

    fn restore_sprite(&self, a: &Map<String, Value>, who: &str) -> ToolOut {
        let n = arg_str(a, "name").unwrap_or_default();
        if !valid_name(n) {
            return Err(format!("\"{n}\" is not a valid sprite name (lowercase snake_case)"));
        }
        let rev = a.get("rev").and_then(Value::as_u64).ok_or("rev is required")?;
        let mut v = store::load_rev(&self.lib, n, rev)?;
        let mut res = render_result(&v, "", None)?;
        let on_disk = std::fs::read(self.path(n)).ok();
        let current = on_disk.as_deref().and_then(|b| serde_json::from_slice::<Value>(b).ok()).map(|v| store::rev_of(&v));
        // commit refuses if the sprite was saved, deleted or recreated since we read `current`
        let new = self.commit(n, &mut v, current, "restore", who)?;
        let from = match (current, &on_disk) {
            (Some(c), _) => format!("from rev {c} "),
            (None, Some(_)) => "over a damaged file (kept in history) ".into(),
            (None, None) => "from the trash ".into(),
        };
        set_headline(&mut res, &format!("Restored \"{n}\" {from}to the shapes of rev {rev}, saved as rev {new}."));
        Ok(res)
    }

    /// Only privileged callers get a copy in _export; for everyone else the GIF is returned inline
    /// and via the URL, so the public can't fill the disk or replace the owner's exports.
    fn export_gif(&self, a: &Map<String, Value>, save: bool) -> ToolOut {
        let name = arg_str(a, "name").unwrap_or_default();
        let v = self.load(name)?;
        let (spec, frames) = render_spec(&v).map_err(|e| format!("\"{name}\" has an invalid spec:\n- {}", e.join("\n- ")))?;
        let scale = match a.get("scale") {
            None | Some(Value::Null) => preview_scale(spec.size, None)?,
            Some(v) => preview_scale(spec.size, Some(v.as_u64().ok_or("scale must be a whole number")?))?,
        };
        let bg_arg = arg_str(a, "bg").unwrap_or("transparent");
        let bg = imgio::parse_bg(bg_arg)?;
        let pingpong = a.get("pingpong").and_then(Value::as_bool).unwrap_or(false);
        let repeat = match a.get("repeat") {
            None | Some(Value::Null) => None,
            Some(r) => Some(r.as_u64().filter(|&n| (1..=u16::MAX as u64).contains(&n)).ok_or("repeat must be a whole number from 1 to 65535")? as u16),
        };
        let opts = imgio::GifOpts { scale, bg, pingpong, repeat };
        let durations: Vec<u32> = (0..frames.len()).map(|i| spec.frame_ms(i)).collect();
        let (gif, info) = imgio::gif(&frames, &durations, &opts).map_err(|e| e.to_string())?;

        if save {
            let dir = self.lib.join("_export");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            store::write_atomic(&dir.join(format!("{name}.gif")), &gif).map_err(|e| e.to_string())?;
        }

        let mut lines = vec![format!(
            "GIF for \"{name}\": {side}×{side}px, {n} frame{s} ({src} rendered), {ms} ms per loop{bg}{pp}{rep}, {kb:.1} KB.",
            side = info.side,
            n = info.frames,
            s = if info.frames == 1 { "" } else { "s" },
            src = frames.len(),
            ms = info.loop_ms,
            bg = if bg.is_some() { format!(", background {bg_arg}") } else { ", transparent".into() },
            pp = if pingpong { ", ping-pong" } else { "" },
            rep = repeat.map(|r| format!(", plays {r}×")).unwrap_or_default(),
            kb = gif.len() as f64 / 1024.0,
        )];
        if !spec.is_animated() {
            lines.push("This sprite isn't animated, so the GIF is a single still frame. Give it an anim or tracks to animate it.".into());
        }
        if let Some(base) = &self.public_url {
            let mut q = vec![format!("scale={scale}")];
            if let Some(c) = bg {
                q.push(format!("bg={:02x}{:02x}{:02x}", c[0], c[1], c[2]));
            }
            if pingpong {
                q.push("loop=pingpong".into());
            }
            if let Some(r) = repeat {
                q.push(format!("repeat={r}"));
            }
            lines.push(format!(
                "URL: {}/v1/sprites/{name}.gif?{}",
                base.trim_end_matches('/'),
                q.join("&")
            ));
        }
        if save {
            lines.push(format!("Saved to _export/{name}.gif in the sprite library."));
        }
        let mut content = vec![];
        if gif.len() <= MAX_INLINE_GIF {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&gif);
            content.push(ContentBlock::image(b64.clone(), "image/gif"));
            // the same bytes as a file resource, so a client can save the actual GIF
            content.push(ContentBlock::resource(
                ResourceContents::blob(b64, format!("sprite://{name}.gif")).with_mime_type("image/gif"),
            ));
        } else {
            lines.push(format!(
                "At {:.0} KB it's too big to return inline (limit {} KB); use the URL, or a smaller scale or fewer frames.",
                gif.len() as f64 / 1024.0,
                MAX_INLINE_GIF / 1024
            ));
        }
        content.insert(0, text(lines.join("\n")));
        Ok(CallToolResult::success(content))
    }

    fn export_atlas(&self, a: &Map<String, Value>) -> ToolOut {
        let (mut specs, _) = self.all();
        if let Some(names) = a.get("names").and_then(Value::as_array) {
            let want: Vec<&str> = names.iter().filter_map(Value::as_str).collect();
            specs.retain(|s| want.contains(&library::spec_name(s)));
        }
        if let Some(c) = arg_str(a, "category") {
            specs.retain(|s| s.get("cat").and_then(Value::as_str) == Some(c));
        }
        if specs.is_empty() {
            return Err("No sprites matched.".into());
        }
        let base = arg_str(a, "file").unwrap_or("atlas");
        if !base.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') || base.is_empty() || base.len() > 40 {
            return Err("file must match ^[a-z0-9_-]{1,40}$".into());
        }
        let scale = a.get("scale").and_then(Value::as_u64).unwrap_or(1).clamp(1, 8) as usize;
        let rendered: Vec<(Value, Vec<Frame>)> =
            specs.into_iter().filter_map(|v| render_spec(&v).ok().map(|(_, f)| (v, f))).collect();
        let dir = self.lib.join("_export");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let png_name = format!("{base}.png");
        let max_side = a.get("max_side").and_then(Value::as_u64).map(|m| m as usize).unwrap_or(library::ATLAS_MAX_SIDE).clamp(256, library::ATLAS_SIDE_LIMIT);
        let (img, meta) = library::atlas(&rendered, scale, &png_name, max_side).map_err(|e| e.to_string())?;
        let png_path = dir.join(&png_name);
        let json_path = dir.join(format!("{base}.json"));
        store::write_atomic(&png_path, &img.png().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        store::write_atomic(&json_path, serde_json::to_string_pretty(&meta).unwrap().as_bytes()).map_err(|e| e.to_string())?;
        let frames: usize = rendered.iter().map(|r| r.1.len()).sum();
        // preview: a 1× copy, doubled only while it stays small
        let preview = {
            let (small, _) = library::atlas(&rendered, 1, &png_name, library::ATLAS_SIDE_LIMIT).map_err(|e| e.to_string())?;
            let k = if small.w.max(small.h) <= 1024 { 2 } else { 1 };
            small.scaled(k).on_backdrop(BACKDROP).png().map_err(|e| e.to_string())?
        };
        Ok(CallToolResult::success(vec![
            text(format!(
                "Exported {} sprites, {frames} frames, {}×{}px, to _export/{png_name} and _export/{base}.json in the sprite library.",
                rendered.len(),
                img.w,
                img.h,
            )),
            image_b64(&preview),
        ]))
    }

    // ── blind review ──

    fn review_sheet(&self, names: &[String]) -> Result<(Vec<Value>, Vec<u8>), String> {
        let mut specs = names.iter().map(|n| self.load(n)).collect::<Result<Vec<_>, _>>()?;
        // shuffle so the order gives nothing away (xorshift seeded from time; no rand dep)
        let mut seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(7) | 1;
        for i in (1..specs.len()).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            specs.swap(i, (seed % (i as u64 + 1)) as usize);
        }
        let items: Vec<(String, Frame)> = specs
            .iter()
            .enumerate()
            .map(|(i, v)| render_spec(v).map(|(_, f)| (format!("#{}", i + 1), f[0].clone())).map_err(|e| e.join("; ")))
            .collect::<Result<_, _>>()?;
        let png = imgio::contact_sheet(&items, 5, 5).png().map_err(|e| e.to_string())?;
        Ok((specs, png))
    }

    fn review_prompt(n: usize) -> String {
        format!(
            "This image shows {n} numbered pixel-art sprites from a picture-vocabulary game for 4–6 year olds. \
             For each, give the word a young child would most likely say (1–2 words), your confidence 0–100, and up to two \
             things it could be mistaken for. Reply with ONLY a JSON array: [{{\"n\":1,\"guess\":\"...\",\"conf\":90,\"alts\":[\"...\"]}}]"
        )
    }

    fn grade(specs: &[Value], reply: &str) -> ToolOut {
        let (a, b) = (reply.find('['), reply.rfind(']'));
        let guesses: Vec<Value> = match (a, b) {
            (Some(a), Some(b)) if b > a => serde_json::from_str(&reply[a..=b]).map_err(|e| format!("Reviewer reply was not valid JSON: {e}"))?,
            _ => return Err(format!("Reviewer reply had no JSON array: {}", reply.chars().take(200).collect::<String>())),
        };
        let mut passed = 0;
        let mut lines = Vec::new();
        for (i, s) in specs.iter().enumerate() {
            let name = library::spec_name(s);
            let g = guesses.iter().find(|g| g.get("n").and_then(Value::as_u64) == Some(i as u64 + 1));
            let mut accepted = vec![norm(name)];
            if let Some(aka) = s.get("aka").and_then(Value::as_array) {
                accepted.extend(aka.iter().filter_map(Value::as_str).map(norm));
            }
            let line = match g {
                None => format!("NO ANSWER {name}"),
                Some(g) => {
                    let guess = g.get("guess").and_then(Value::as_str).unwrap_or("");
                    let conf = g.get("conf").and_then(Value::as_f64).unwrap_or(0.0);
                    let alts: Vec<&str> = g.get("alts").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                    let ok = accepted.contains(&norm(guess));
                    let verdict = if !ok { "FAIL" } else if conf >= 80.0 { "PASS" } else { "WEAK" };
                    if verdict == "PASS" {
                        passed += 1;
                    }
                    let alts = if alts.is_empty() { String::new() } else { format!("; could be {}", alts.join(", ")) };
                    format!("{verdict:<5} {name}: read as \"{guess}\" ({conf}%){alts}")
                }
            };
            lines.push(line);
        }
        Ok(CallToolResult::success(vec![text(format!(
            "{passed}/{} passed.\n{}\n\nFix FAIL and WEAK sprites so they cannot be confused with the alternatives, then run blind_review again.",
            specs.len(),
            lines.join("\n")
        ))]))
    }

    fn sampling_params(png: &[u8], n: usize) -> CreateMessageRequestParams {
        let img = SamplingMessageContentBlock::Image(ImageContent::new(base64::engine::general_purpose::STANDARD.encode(png), "image/png"));
        let mut p = CreateMessageRequestParams::new(
            vec![SamplingMessage::new_multiple(Role::User, vec![img, SamplingMessageContentBlock::text(Self::review_prompt(n))])],
            1500,
        );
        p.system_prompt = Some("You are a strict blind reviewer. Judge only from the picture.".into());
        p.include_context = Some(ContextInclusion::None);
        p
    }

    fn reply_text(r: &CreateMessageResult) -> String {
        let blocks: Vec<&SamplingMessageContentBlock> = match &r.message.content {
            SamplingContent::Single(b) => vec![b],
            SamplingContent::Multiple(v) => v.iter().collect(),
        };
        blocks
            .into_iter()
            .filter_map(|b| match b {
                SamplingMessageContentBlock::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn blind_review(&self, a: &Map<String, Value>, req: &CallToolRequestParams, ctx: &RequestContext<RoleServer>) -> Result<CallToolResponse, String> {
        // retry leg of an MRTR round trip
        if let (Some(responses), Some(token)) = (&req.input_responses, &req.request_state) {
            let names = self.reviews.lock().unwrap().remove(token).map(|(_, n)| n).ok_or("This review expired; run blind_review again.")?;
            let specs = names.iter().map(|n| self.load(n)).collect::<Result<Vec<_>, _>>()?;
            let raw = responses.get("review").ok_or("client did not answer the review request")?;
            let result: CreateMessageResult = serde_json::from_value(raw.clone()).map_err(|e| format!("bad sampling result: {e}"))?;
            return Self::grade(&specs, &Self::reply_text(&result)).map(Into::into);
        }

        let names: Vec<String> = a.get("names").and_then(Value::as_array).map(|v| v.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
        if names.is_empty() || names.len() > 30 {
            return Err("names must list 1–30 sprites".into());
        }
        let (specs, png) = self.review_sheet(&names)?;
        let shuffled: Vec<String> = specs.iter().map(|s| library::spec_name(s).to_string()).collect();

        // 1) direct API call, if configured
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            let png2 = png.clone();
            let n = specs.len();
            let reply = tokio::task::spawn_blocking(move || anthropic_review(&key, &png2, n)).await.map_err(|e| e.to_string())??;
            return Self::grade(&specs, &reply).map(Into::into);
        }

        // 2) MCP sampling
        let caps = ctx.meta.client_capabilities().or_else(|| ctx.peer.peer_info().map(|i| i.capabilities.clone()));
        let version = ctx.meta.protocol_version().or_else(|| ctx.peer.peer_info().map(|i| i.protocol_version.clone()));
        if caps.as_ref().is_some_and(|c| c.sampling.is_some()) {
            let params = Self::sampling_params(&png, specs.len());
            let modern = version.as_ref().is_some_and(|v| v.as_str() >= ProtocolVersion::V_2026_07_28.as_str());
            if modern {
                let token = format!("rv-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
                let mut pending = self.reviews.lock().unwrap();
                // forget reviews a client started but never answered
                pending.retain(|_, (t, _)| t.elapsed() < std::time::Duration::from_secs(600));
                pending.insert(token.clone(), (std::time::Instant::now(), shuffled));
                drop(pending);
                let mut reqs = std::collections::BTreeMap::new();
                reqs.insert("review".to_string(), InputRequest::CreateMessage(CreateMessageRequest::new(params)));
                return Ok(InputRequiredResult::new(Some(reqs), Some(token)).into());
            }
            let out = ctx.peer.create_message(params).await.map_err(|e| format!("Sampling failed: {e}"))?;
            return Self::grade(&specs, &Self::reply_text(&out)).map(Into::into);
        }

        // 3) fallback
        let key: Vec<String> = shuffled.iter().enumerate().map(|(i, n)| format!("#{}={n}", i + 1)).collect();
        Ok(CallToolResult::success(vec![
            text(format!(
                "No reviewer is available here (no ANTHROPIC_API_KEY and the client does not support sampling). Give the image to a \
                 reviewer that has NOT seen the names and ask what a young child would call each numbered sprite.\nKey: {}",
                key.join(", ")
            )),
            image_b64(&png),
        ])
        .into())
    }
}

/// Ask Claude directly (Messages API). ANTHROPIC_BASE_URL and SPRITE_REVIEW_MODEL override the defaults.
fn anthropic_review(key: &str, png: &[u8], n: usize) -> Result<String, String> {
    let base = std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| "https://api.anthropic.com".into());
    let model = std::env::var("SPRITE_REVIEW_MODEL").unwrap_or_else(|_| "claude-sonnet-5-5".into());
    let body = json!({
        "model": model,
        "max_tokens": 1500,
        "system": "You are a strict blind reviewer. Judge only from the picture.",
        "messages": [{"role": "user", "content": [
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": base64::engine::general_purpose::STANDARD.encode(png)}},
            {"type": "text", "text": SpriteServer::review_prompt(n)}
        ]}]
    });
    let mut resp = ureq::post(format!("{}/v1/messages", base.trim_end_matches('/')))
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .send_json(&body)
        .map_err(|e| format!("Review API call failed: {e}"))?;
    let v: Value = resp.body_mut().read_json().map_err(|e| format!("Review API returned bad JSON: {e}"))?;
    let reply: Vec<&str> = v
        .get("content")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect())
        .unwrap_or_default();
    if reply.is_empty() {
        return Err(format!("Review API returned no text: {v}"));
    }
    Ok(reply.join("\n"))
}

impl ServerHandler for SpriteServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("sprite-factory", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Sprite Factory makes 32×32 pixel-art sprites in one locked house style. Call style_guide once before authoring. \
                 You describe shapes and materials; the renderer handles colour, light, outline and animation. Always look at the \
                 returned image and revise. Before calling a sprite done, run blind_review on it.",
            )
    }

    async fn list_tools(&self, _request: Option<PaginatedRequestParams>, _context: RequestContext<RoleServer>) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().find(|t| t.name == name)
    }

    async fn call_tool(&self, request: CallToolRequestParams, context: RequestContext<RoleServer>) -> Result<CallToolResponse, McpError> {
        let args = request.arguments.clone().unwrap_or_default();
        let this = self.clone();
        let (who, privileged) = caller(&context);
        let name = request.name.to_string();
        if !privileged && Self::keyed(&name) {
            return Ok(CallToolResult::error(vec![text(format!(
                "{name} needs a write token sent as an Authorization: Bearer header. Reading, rendering, export_gif and \
                 creating new sprites work without one."
            ))])
            .into());
        }
        let out: Result<CallToolResponse, String> = match name.as_str() {
            "blind_review" => self.blind_review(&args, &request, &context).await,
            "style_guide" => {
                let guide = GUIDE.split("## Review loop").next().unwrap_or(GUIDE).to_string()
                    + "## Review loop\nEvery create/edit returns the rendered sprite and its frames. Critique, then edit_sprite. Finish with blind_review.\n";
                let pal: Map<String, Value> = Palette::new().iter().map(|(n, r)| (n.to_string(), json!(r.iter().map(|c| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])).collect::<Vec<_>>()))).collect();
                Ok(CallToolResult::success(vec![text(guide), text(format!("materials (shade 0→4): {}", Value::Object(pal)))]).into())
            }
            "get_sprite" => self.load(arg_str(&args, "name").unwrap_or_default()).map(|v| CallToolResult::success(vec![text(serde_json::to_string_pretty(&v).unwrap())]).into()),
            // CPU-bound tools and anything that touches the library's files (or takes the write
            // lock) run off the async executor
            "list_sprites" | "create_sprite" | "edit_sprite" | "delete_sprite" | "sprite_history" | "restore_sprite" | "render_sprite" | "export_gif"
            | "export_atlas" => {
                let res = tokio::task::spawn_blocking(move || match name.as_str() {
                    "list_sprites" => this.list_sprites(&args),
                    "delete_sprite" => this.delete_sprite(&args, &who),
                    "sprite_history" => this.sprite_history(&args),
                    "create_sprite" => this.create_sprite(&args, &who, privileged),
                    "edit_sprite" => this.edit_sprite(&args, &who),
                    "restore_sprite" => this.restore_sprite(&args, &who),
                    "export_gif" => this.export_gif(&args, privileged),
                    "render_sprite" => {
                        let n = arg_str(&args, "name").unwrap_or_default();
                        let scale = args.get("scale").and_then(Value::as_u64);
                        this.load(n).and_then(|v| render_result(&v, &format!("\"{n}\""), scale))
                    }
                    _ => this.export_atlas(&args),
                })
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                res.map(Into::into)
            }
            other => return Err(McpError::invalid_params(format!("unknown tool {other}"), None)),
        };
        Ok(out.unwrap_or_else(|msg| CallToolResult::error(vec![text(msg)]).into()))
    }
}

