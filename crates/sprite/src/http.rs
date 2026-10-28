//! `sprite serve` — HTTP server: MCP (Streamable HTTP) at /mcp plus a small read-only REST API
//! for game prototypes. Reading, rendering and creating need no token; changing or deleting an
//! existing sprite, and anything that spends API credits (blind_review), needs a write token sent
//! as a bearer header. See `SpriteServer::keyed` for the exact list.

use anyhow::{bail, Context, Result};
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::{json, Value};
use sprite_core::lint::lint;
use sprite_core::{render_anim, Frame, Palette, RenderOpts, Spec, SIZE};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;

use crate::imgio::{self, Rgba};
use crate::library;
use crate::mcp::{valid_name, Reviews, SpriteServer};

pub struct ServeOpts {
    pub bind: SocketAddr,
    pub library: PathBuf,
    pub tokens_file: Option<PathBuf>,
    pub hosts: Vec<String>,
    pub cors: Vec<String>,
}

/// A stored sprite's frames and how long each one shows (ms × hold).
struct Rendered {
    frames: Vec<Frame>,
    durations: Vec<u32>,
}

/// sprite name → (hash of its spec file, rendered frames)
type RenderCache = Arc<Mutex<HashMap<String, (u64, Arc<Rendered>)>>>;

#[derive(Clone)]
struct AppState {
    lib: Arc<PathBuf>,
    tokens: Arc<Vec<Token>>,
    cache: RenderCache,
}

// ───────────────────────── tokens ─────────────────────────

/// What a token may do beyond the public tools. `Read` only labels the caller (for history and
/// logs); `Write`, sent as a bearer header, also unlocks the keyed MCP tools (edit, delete,
/// restore, overwrite, blind_review).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Read,
    Write,
}

#[derive(Debug, Clone)]
struct Token {
    label: String,
    scope: Scope,
    /// false for `label:token` entries, which default to write
    explicit: bool,
    secret: String,
}

/// Tokens come from SPRITE_TOKENS (comma-separated) and/or a file with one entry per line
/// (`#` comments). An entry is `label:scope:token`, `label:token` or a bare `token`, where scope
/// is `read` or `write`. Entries without a scope are `write`, which is what every token could do
/// before scopes existed. Tokens shorter than 24 characters are rejected.
fn load_tokens(file: Option<&PathBuf>) -> Result<Vec<Token>> {
    let mut out: Vec<Token> = Vec::new();
    let mut add = |line: &str, src: &str| -> Result<()> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return Ok(());
        }
        let parts: Vec<&str> = line.split(':').map(str::trim).collect();
        let (label, scope, secret) = match parts[..] {
            [t] => (format!("{src}#{}", out.len() + 1), None, t),
            [l, t] => (l.to_string(), None, t),
            [l, s, t] => {
                let scope = match s {
                    "read" => Scope::Read,
                    "write" => Scope::Write,
                    other => bail!("token {l:?}: scope must be read or write, not {other:?}"),
                };
                (l.to_string(), Some(scope), t)
            }
            _ => bail!("token entries look like label:scope:token (got {} fields)", parts.len()),
        };
        if secret.len() < 24 {
            bail!("token for {label:?} is too short (need ≥ 24 chars; try `openssl rand -hex 32`)");
        }
        out.push(Token { label, scope: scope.unwrap_or(Scope::Write), explicit: scope.is_some(), secret: secret.to_string() });
        Ok(())
    };
    if let Ok(env) = std::env::var("SPRITE_TOKENS") {
        for t in env.split(',') {
            add(t, "env")?;
        }
    }
    if let Some(f) = file {
        let text = std::fs::read_to_string(f).with_context(|| format!("read tokens file {}", f.display()))?;
        for l in text.lines() {
            add(l, "file")?;
        }
    }
    Ok(out)
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Who is behind a request: a token label (or "anon"), and whether they may change or delete
/// existing sprites. Only a write token sent as a bearer header is privileged: `?key=` tokens end
/// up in URLs, bundles and browser history, so they never authorize changes.
#[derive(Debug, Clone)]
pub(crate) struct Caller {
    pub label: String,
    pub privileged: bool,
}

/// No token → anonymous (public tools only). A token that doesn't match is a 401 rather than a
/// silent downgrade, so a typo'd key shows up as an error instead of missing permissions.
async fn auth(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    if req.uri().path() == "/healthz" || req.method() == Method::OPTIONS {
        return next.run(req).await;
    }
    let from_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    let via_header = from_header.is_some();
    let from_query = req.uri().query().and_then(|q| {
        q.split('&').find_map(|kv| kv.strip_prefix("key=").map(|v| v.to_string()))
    });
    let caller = match from_header.or(from_query) {
        None => Caller { label: "anon".into(), privileged: false },
        Some(p) => match st.tokens.iter().find(|t| ct_eq(t.secret.as_bytes(), p.as_bytes())) {
            Some(tok) => Caller { label: tok.label.clone(), privileged: tok.scope == Scope::Write && via_header },
            None => {
                return (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Bearer")], Json(json!({"error": "invalid token"})))
                    .into_response()
            }
        },
    };
    req.extensions_mut().insert(caller);
    next.run(req).await
}

// ───────────────────────── rendering with a small cache ─────────────────────────

fn hash_bytes(b: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

#[derive(Debug)]
struct ApiError(StatusCode, Value);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

fn not_found(name: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, json!({"error": format!("no sprite named {name:?}")}))
}

impl AppState {
    /// Spec bytes + content hash for a stored sprite.
    fn read_spec(&self, name: &str) -> Result<(Value, Vec<u8>, u64), ApiError> {
        if !valid_name(name) {
            return Err(ApiError(StatusCode::BAD_REQUEST, json!({"error": "sprite names are lowercase snake_case"})));
        }
        let bytes = std::fs::read(self.lib.join(format!("{name}.json"))).map_err(|_| not_found(name))?;
        let v: Value = serde_json::from_slice(&bytes)
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": format!("{name}.json: {e}")})))?;
        let h = hash_bytes(&bytes);
        Ok((v, bytes, h))
    }

    /// Frames for a stored sprite, re-rendered only when its file changed.
    fn frames(&self, name: &str) -> Result<(Value, u64, Arc<Rendered>), ApiError> {
        let (v, _, h) = self.read_spec(name)?;
        if let Some((ch, f)) = self.cache.lock().unwrap().get(name) {
            if *ch == h {
                return Ok((v, h, f.clone()));
            }
        }
        let pal = Palette::new();
        let spec = Spec::from_json(&v, &pal)
            .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, json!({"error": "invalid stored spec", "details": e})))?;
        let frames = render_anim(&spec, &pal, &RenderOpts::default());
        let durations = (0..frames.len()).map(|i| spec.frame_ms(i)).collect();
        let f = Arc::new(Rendered { frames, durations });
        self.cache.lock().unwrap().insert(name.to_string(), (h, f.clone()));
        Ok((v, h, f))
    }

    fn all_names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.lib.as_ref())
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let p = e.path();
                        (p.extension().is_some_and(|x| x == "json") && p.is_file())
                            .then(|| p.file_stem().unwrap().to_string_lossy().to_string())
                    })
                    .filter(|n| valid_name(n))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

fn etag(parts: &[u64]) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    sprite_core::RENDER_VERSION.hash(&mut h);
    parts.hash(&mut h);
    format!("W/\"{:016x}\"", h.finish())
}

fn cached(headers: &HeaderMap, tag: &str, content_type: &'static str, body: Vec<u8>) -> Response {
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(tag) {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, tag.to_string())]).into_response();
    }
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (header::ETAG, HeaderValue::from_str(tag).unwrap()),
            (header::CACHE_CONTROL, HeaderValue::from_static("private, no-cache")),
        ],
        body,
    )
        .into_response()
}

fn qnum(q: &HashMap<String, String>, k: &str, default: usize, lo: usize, hi: usize) -> usize {
    q.get(k).and_then(|v| v.parse().ok()).unwrap_or(default).clamp(lo, hi)
}

/// GIF options from the query string: ?scale=, ?bg=transparent|rrggbb, ?loop=pingpong, ?repeat=N.
fn gif_opts(q: &HashMap<String, String>, scale: usize) -> Result<imgio::GifOpts, ApiError> {
    let bad = |m: String| ApiError(StatusCode::BAD_REQUEST, json!({"error": m}));
    let bg = imgio::parse_bg(q.get("bg").map(String::as_str).unwrap_or("")).map_err(bad)?;
    let pingpong = match q.get("loop").map(String::as_str) {
        None | Some("") | Some("forward") => false,
        Some("pingpong") => true,
        Some(o) => return Err(bad(format!("loop must be forward or pingpong, not {o:?}"))),
    };
    let repeat = match q.get("repeat") {
        None => None,
        Some(r) => Some(r.parse::<u16>().ok().filter(|&n| n >= 1).ok_or_else(|| bad("repeat must be a whole number from 1 to 65535 (omit it to loop forever)".into()))?),
    };
    Ok(imgio::GifOpts { scale, bg, pingpong, repeat })
}

/// The GIF options as one number for the ETag (0 for anything that isn't a GIF).
fn gif_key(o: &imgio::GifOpts, kind: &str) -> u64 {
    if kind != "gif" {
        return 0;
    }
    let bg = o.bg.map_or(1 << 24, |c| u32::from_be_bytes([0, c[0], c[1], c[2]])) as u64;
    bg | (o.pingpong as u64) << 25 | (o.repeat.map_or(0, |r| r as u64 + 1)) << 26
}

fn png_of(frames: &[Frame], which: &str, scale: usize, frame: usize, durations: &[u32], gif: &imgio::GifOpts) -> Result<(Vec<u8>, &'static str), ApiError> {
    let err = |e: anyhow::Error| ApiError(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e.to_string()}));
    Ok(match which {
        "gif" => (imgio::gif(frames, durations, gif).map_err(err)?.0, "image/gif"),
        "sheet" => (Rgba::strip(frames).scaled(scale).png().map_err(err)?, "image/png"),
        _ => {
            let f = &frames[frame.min(frames.len() - 1)];
            let mut img = Rgba::new(SIZE * scale, SIZE * scale);
            img.blit_frame(f, 0, 0, scale);
            (img.png().map_err(err)?, "image/png")
        }
    })
}

// ───────────────────────── handlers ─────────────────────────

async fn healthz() -> &'static str {
    "ok"
}

/// GET /v1/sprites?category=
async fn list(State(st): State<AppState>, Query(q): Query<HashMap<String, String>>) -> Result<Json<Value>, ApiError> {
    let want = q.get("category").cloned();
    let rows = tokio::task::spawn_blocking(move || {
        st.all_names()
            .into_iter()
            .filter_map(|n| {
                let (v, _, _) = st.read_spec(&n).ok()?;
                let cat = v.get("cat").and_then(Value::as_str).unwrap_or("misc").to_string();
                if want.as_deref().is_some_and(|w| w != cat) {
                    return None;
                }
                let animated = v.get("anim").and_then(Value::as_str).is_some_and(|a| a != "none")
                    || v.get("tracks").and_then(Value::as_array).is_some_and(|t| !t.is_empty());
                Some(json!({
                    "name": n, "category": cat,
                    "anim": v.get("anim").cloned().unwrap_or(json!("none")),
                    "frames": if animated { v.get("frames").and_then(Value::as_u64).unwrap_or(8) } else { 1 },
                    "frameMs": v.get("ms").and_then(Value::as_u64).unwrap_or(110),
                    "png": format!("/v1/sprites/{n}.png"), "gif": format!("/v1/sprites/{n}.gif"),
                    "sheet": format!("/v1/sprites/{n}.sheet.png"), "spec": format!("/v1/sprites/{n}.json"),
                }))
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e.to_string()})))?;
    Ok(Json(json!({"count": rows.len(), "sprites": rows})))
}

/// GET /v1/sprites/{name}.json | .png | .gif | .sheet.png   (?scale=1..16, ?frame=N for .png;
/// for .gif also ?bg=transparent|rrggbb, ?loop=forward|pingpong, ?repeat=N)
async fn sprite_file(
    State(st): State<AppState>,
    UrlPath(file): UrlPath<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (name, kind) = if let Some(n) = file.strip_suffix(".sheet.png") {
        (n, "sheet")
    } else if let Some(n) = file.strip_suffix(".png") {
        (n, "png")
    } else if let Some(n) = file.strip_suffix(".gif") {
        (n, "gif")
    } else if let Some(n) = file.strip_suffix(".json") {
        (n, "json")
    } else {
        return Err(ApiError(StatusCode::NOT_FOUND, json!({"error": "use NAME.json, NAME.png, NAME.gif or NAME.sheet.png"})));
    };
    let name = name.to_string();
    if kind == "json" {
        let (_, bytes, h) = st.read_spec(&name)?;
        return Ok(cached(&headers, &etag(&[h]), "application/json", bytes));
    }
    let default_scale = if kind == "gif" { 4 } else { 1 };
    let scale = qnum(&q, "scale", default_scale, 1, 16);
    // the frame index only matters for a single PNG; GIF options only for a GIF
    let frame = if kind == "png" { qnum(&q, "frame", 0, 0, 255) } else { 0 };
    let gopts = if kind == "gif" { gif_opts(&q, scale)? } else { imgio::GifOpts { scale, ..Default::default() } };
    let res = tokio::task::spawn_blocking(move || -> Result<Response, ApiError> {
        let (_, h, r) = st.frames(&name)?;
        let tag = etag(&[h, scale as u64, frame as u64, kind.len() as u64, gif_key(&gopts, kind)]);
        if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(tag.as_str()) {
            return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, tag)]).into_response());
        }
        let (bytes, ct) = png_of(&r.frames, kind, scale, frame, &r.durations, &gopts)?;
        Ok(cached(&headers, &tag, ct, bytes))
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e.to_string()})))?;
    res
}

/// GET /v1/atlas.png | /v1/atlas.json   (?category=, ?names=a,b, ?scale=1..8)
async fn atlas(
    State(st): State<AppState>,
    UrlPath(file): UrlPath<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let as_png = match file.as_str() {
        "atlas.png" => true,
        "atlas.json" => false,
        _ => return Err(ApiError(StatusCode::NOT_FOUND, json!({"error": "use /v1/atlas.png or /v1/atlas.json"}))),
    };
    let scale = qnum(&q, "scale", 1, 1, 8);
    let max_side = qnum(&q, "max", library::ATLAS_MAX_SIDE, 256, library::ATLAS_SIDE_LIMIT);
    let category = q.get("category").cloned();
    let names: Option<Vec<String>> = q.get("names").map(|s| s.split(',').map(|x| x.trim().to_string()).collect());
    // the JSON points at the matching PNG with the same filters. Never echo the token: atlas.json
    // gets saved next to game code and committed.
    let mut qs: Vec<String> = q.iter().filter(|(k, _)| k.as_str() != "key").map(|(k, v)| format!("{k}={v}")).collect();
    qs.sort();
    let image_ref = if qs.is_empty() { "atlas.png".to_string() } else { format!("atlas.png?{}", qs.join("&")) };
    let res = tokio::task::spawn_blocking(move || -> Result<Response, ApiError> {
        let mut picked = Vec::new();
        let mut parts = vec![scale as u64];
        let wanted: Vec<String> =
            st.all_names().into_iter().filter(|n| names.as_ref().is_none_or(|ns| ns.contains(n))).collect();
        // render cache misses on every core, then assemble in name order
        let rendered = library::par_map(&wanted, |n| st.frames(n).ok());
        for r in rendered {
            let Some((v, h, frames)) = r else { continue };
            if category.as_deref().is_some_and(|c| v.get("cat").and_then(Value::as_str) != Some(c)) {
                continue;
            }
            parts.push(h);
            picked.push((v, frames.frames.clone()));
        }
        if picked.is_empty() {
            return Err(ApiError(StatusCode::NOT_FOUND, json!({"error": "no sprites matched"})));
        }
        let tag = etag(&[hash_bytes(format!("{parts:?}|{as_png}|{image_ref}").as_bytes())]);
        if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(tag.as_str()) {
            return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, tag)]).into_response());
        }
        let (img, meta) = library::atlas(&picked, scale, &image_ref, max_side)
            .map_err(|e| ApiError(StatusCode::PAYLOAD_TOO_LARGE, json!({"error": e.to_string()})))?;
        if as_png {
            let png = img.png().map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e.to_string()})))?;
            Ok(cached(&headers, &tag, "image/png", png))
        } else {
            Ok(cached(&headers, &tag, "application/json", serde_json::to_vec_pretty(&meta).unwrap()))
        }
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e.to_string()})))?;
    res
}

/// POST /v1/render?format=png|gif|sheet|json&scale=N — render a spec without saving it.
async fn render_adhoc(Query(q): Query<HashMap<String, String>>, body: Bytes) -> Result<Response, ApiError> {
    let v: Value = serde_json::from_slice(&body).map_err(|e| ApiError(StatusCode::BAD_REQUEST, json!({"error": format!("body is not JSON: {e}")})))?;
    let format = q.get("format").cloned().unwrap_or_else(|| "png".into());
    let scale = qnum(&q, "scale", if format == "gif" { 4 } else { 1 }, 1, 16);
    let gopts = if format == "gif" { gif_opts(&q, scale)? } else { imgio::GifOpts { scale, ..Default::default() } };
    tokio::task::spawn_blocking(move || -> Result<Response, ApiError> {
        let pal = Palette::new();
        let spec = Spec::from_json(&v, &pal)
            .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, json!({"error": "invalid spec", "details": e})))?;
        let frames = render_anim(&spec, &pal, &RenderOpts::default());
        if format == "json" {
            return Ok(Json(json!({"lint": lint(&spec, &frames)})).into_response());
        }
        let kind = match format.as_str() {
            "gif" => "gif",
            "sheet" => "sheet",
            _ => "png",
        };
        let durations: Vec<u32> = (0..frames.len()).map(|i| spec.frame_ms(i)).collect();
        let (bytes, ct) = png_of(&frames, kind, scale, 0, &durations, &gopts)?;
        Ok(([(header::CONTENT_TYPE, ct)], bytes).into_response())
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": e.to_string()})))?
}

async fn not_found_fallback() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, json!({"error": "not found", "routes": [
        "GET /healthz", "POST|GET|DELETE /mcp", "GET /v1/sprites", "GET /v1/sprites/{name}.{json|png|gif|sheet.png}",
        "GET /v1/atlas.{png|json}", "POST /v1/render"
    ]}))
}

// ───────────────────────── server ─────────────────────────

pub fn serve(o: ServeOpts) -> Result<()> {
    let tokens = load_tokens(o.tokens_file.as_ref())?;
    if tokens.is_empty() {
        bail!("no tokens configured: set SPRITE_TOKENS or --tokens-file (generate one with `openssl rand -hex 32`)");
    }
    let unscoped: Vec<&str> = tokens.iter().filter(|t| !t.explicit).map(|t| t.label.as_str()).collect();
    if !unscoped.is_empty() {
        eprintln!(
            "sprite serve: token(s) {} have no scope and can edit and delete via MCP; write them as \
             label:read:token (prototypes, ?key= use) or label:write:token",
            unscoped.join(", ")
        );
    }
    std::fs::create_dir_all(&o.library).with_context(|| format!("create library {}", o.library.display()))?;
    let state = AppState { lib: Arc::new(o.library.clone()), tokens: Arc::new(tokens), cache: Arc::default() };
    let state_for_warmup = state.clone();

    let mut hosts: Vec<String> = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
    hosts.extend(o.hosts.iter().cloned());
    let reviews = Reviews::default();
    let lib = o.library.clone();
    let mcp = StreamableHttpService::new(
        move || Ok(SpriteServer::new(lib.clone(), reviews.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default().with_allowed_hosts(hosts).with_max_request_body_bytes(2 << 20),
    );

    let mut app = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/sprites", get(list))
        .route("/v1/sprites/{file}", get(sprite_file))
        .route("/v1/{file}", get(atlas))
        .route("/v1/render", post(render_adhoc).layer(RequestBodyLimitLayer::new(256 * 1024)))
        .nest_service("/mcp", mcp)
        .fallback(not_found_fallback)
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state);
    if !o.cors.is_empty() {
        let origins: Vec<HeaderValue> = o.cors.iter().filter_map(|c| HeaderValue::from_str(c).ok()).collect();
        app = app.layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(origins))
                .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE, header::IF_NONE_MATCH])
                .expose_headers([header::ETAG]),
        );
    }

    // warm the render cache in the background so the first atlas request is quick
    let warm = state_for_warmup;
    std::thread::spawn(move || {
        let names = warm.all_names();
        library::par_map(&names, |n| warm.frames(n).ok());
    });

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(o.bind).await.with_context(|| format!("bind {}", o.bind))?;
        let addr = listener.local_addr()?;
        println!("sprite serve listening on http://{addr} (library {})", o.library.display());
        use std::io::Write;
        std::io::stdout().flush().ok();
        axum::serve(listener, app).with_graceful_shutdown(shutdown()).await?;
        Ok(())
    })
}

async fn shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
        tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    ctrl_c.await.ok();
}

