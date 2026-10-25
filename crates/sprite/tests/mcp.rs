//! Drives `sprite mcp` as a real MCP client over stdio and exercises each blind-review path:
//! MRTR (protocol 2026-07-28), classic server→client sampling (2025-11-25), the direct API call
//! (against a local mock of the Messages API) and the no-reviewer fallback.
#![allow(deprecated)]

use rmcp::model::*;
use rmcp::service::{RequestContext, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::{ClientHandler, ErrorData as McpError, RoleClient, ServiceExt};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::process::Command;

#[derive(Clone, Default)]
struct Reviewer {
    sampling: bool,
    version: Option<ProtocolVersion>,
    calls: Arc<AtomicUsize>,
}

impl ClientHandler for Reviewer {
    fn get_info(&self) -> ClientConfig {
        let caps = if self.sampling { ClientCapabilities::builder().enable_sampling().build() } else { ClientCapabilities::default() };
        let mut info = ClientConfig::new(caps, Implementation::new("test-reviewer", "0"));
        if let Some(v) = &self.version {
            info = info.with_protocol_version(v.clone());
        }
        info
    }

    async fn create_message(&self, params: CreateMessageRequestParams, _ctx: RequestContext<RoleClient>) -> Result<CreateMessageResult, McpError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let has_image = params.messages.iter().any(|m| m.content.iter().any(|c| matches!(c, SamplingMessageContentBlock::Image(_))));
        assert!(has_image, "review request must carry the sheet image");
        assert_eq!(params.include_context, Some(ContextInclusion::None), "reviewer must not see the conversation");
        // scripted answers; the sheet is shuffled so only the plumbing and grading are checked
        let reply = r#"[{"n":1,"guess":"teapot","conf":90,"alts":["kettle"]},{"n":2,"guess":"jar","conf":85,"alts":[]}]"#;
        Ok(CreateMessageResult::new(SamplingMessage::assistant_text(reply), "stub".into()))
    }
}

fn library() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("sprite-test-{}-{}", std::process::id(), rand_suffix()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn rand_suffix() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
}

async fn connect(r: Reviewer, lib: &std::path::Path, env: &[(&str, &str)]) -> RunningService<RoleClient, Reviewer> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sprite"));
    cmd.arg("mcp").arg("--library").arg(lib).env_remove("ANTHROPIC_API_KEY").env_remove("SPRITE_PUBLIC_URL");
    for (k, v) in env {
        cmd.env(k, v);
    }
    r.serve(TokioChildProcess::new(cmd).unwrap()).await.expect("connect")
}

async fn call(c: &RunningService<RoleClient, Reviewer>, name: &'static str, args: Value) -> CallToolResult {
    let mut p = CallToolRequestParams::new(name);
    p.arguments = args.as_object().cloned();
    c.call_tool(p).await.expect("call_tool")
}

fn text_of(r: &CallToolResult) -> String {
    r.content.iter().filter_map(|c| if let ContentBlock::Text(t) = c { Some(t.text.clone()) } else { None }).collect::<Vec<_>>().join("\n")
}

async fn seed(c: &RunningService<RoleClient, Reviewer>) {
    for (name, prims) in [
        ("teapot", json!([{"t":"ellipse","c":[15.5,20],"r":[10.5,9],"m":"teal","gloss":true},{"t":"ellipse","c":[15.5,8],"r":2.4,"m":"gold"}])),
        ("jar", json!([{"t":"rect","p":[8,8,24,29],"round":4,"m":"glass"},{"t":"rect","p":[9,4,23,9],"round":1,"m":"red","edge":true}])),
    ] {
        let r = call(c, "create_sprite", json!({"name": name, "category": "home", "prims": prims})).await;
        assert_ne!(r.is_error, Some(true), "{}", text_of(&r));
        assert!(r.content.iter().any(|c| matches!(c, ContentBlock::Image(_))), "create returns the rendered sprite");
    }
}

#[tokio::test]
async fn tools_and_editing() {
    let lib = library();
    let c = connect(Reviewer::default(), &lib, &[]).await;
    let tools: Vec<String> = c.list_all_tools().await.unwrap().into_iter().map(|t| t.name.to_string()).collect();
    for t in ["style_guide", "list_sprites", "get_sprite", "create_sprite", "edit_sprite", "render_sprite", "delete_sprite", "sprite_history", "restore_sprite", "blind_review", "export_gif", "export_atlas"] {
        assert!(tools.contains(&t.to_string()), "missing tool {t}");
    }
    seed(&c).await;
    let dup = call(&c, "create_sprite", json!({"name": "jar", "category": "home", "prims": [{"t":"ellipse","c":[16,16],"r":8}]})).await;
    assert_eq!(dup.is_error, Some(true));
    let bad = call(&c, "create_sprite", json!({"name": "bad", "category": "misc", "prims": [{"t":"ellipse","c":[16,16],"r":8,"m":"plaid"}]})).await;
    assert!(text_of(&bad).contains("unknown material"), "{}", text_of(&bad));
    assert!(!lib.join("bad.json").exists(), "invalid specs are not saved");
    let ed = call(&c, "edit_sprite", json!({"name": "jar", "prim_ops": [{"op": "merge", "index": 1, "prim": {"m": "blue"}}]})).await;
    assert_ne!(ed.is_error, Some(true), "{}", text_of(&ed));
    let got: Value = serde_json::from_str(&text_of(&call(&c, "get_sprite", json!({"name": "jar"})).await)).unwrap();
    assert_eq!(got["prims"][1]["m"], "blue");
    let oob = call(&c, "edit_sprite", json!({"name": "jar", "prim_ops": [{"op": "delete", "index": 9}]})).await;
    assert_eq!(oob.is_error, Some(true));
    // timing fields round-trip through edit_sprite, and export_gif returns a real GIF inline
    let ed = call(&c, "edit_sprite", json!({"name": "jar", "anim": "bob", "snap": true, "ms": 90, "hold": [2, 1, 1, 1, 2, 1, 1, 1]})).await;
    assert_ne!(ed.is_error, Some(true), "{}", text_of(&ed));
    let got: Value = serde_json::from_str(&text_of(&call(&c, "get_sprite", json!({"name": "jar"})).await)).unwrap();
    assert_eq!((got["snap"].clone(), got["ms"].clone()), (json!(true), json!(90)));
    let g = call(&c, "export_gif", json!({"name": "jar", "scale": 4, "pingpong": true})).await;
    assert_ne!(g.is_error, Some(true), "{}", text_of(&g));
    assert!(text_of(&g).contains("128×128px") && text_of(&g).contains("ping-pong"), "{}", text_of(&g));
    let gif_b64 = g.content.iter().find_map(|c| match c {
        ContentBlock::Image(i) if i.mime_type == "image/gif" => Some(i.data.clone()),
        _ => None,
    });
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, gif_b64.expect("an image/gif block")).unwrap();
    assert_eq!(&bytes[..6], b"GIF89a");
    assert!(lib.join("_export/jar.gif").exists());
    let bad_bg = call(&c, "export_gif", json!({"name": "jar", "bg": "plaid"})).await;
    assert_eq!(bad_bg.is_error, Some(true));
    let big = call(&c, "export_gif", json!({"name": "jar", "scale": 99})).await;
    assert!(text_of(&big).contains("scale must be"), "{}", text_of(&big));
    // null puts a timing field back to its default
    call(&c, "edit_sprite", json!({"name": "jar", "hold": null})).await;
    let got: Value = serde_json::from_str(&text_of(&call(&c, "get_sprite", json!({"name": "jar"})).await)).unwrap();
    assert!(got.get("hold").is_none());
    assert!(!text_of(&g).contains("URL"), "no download link without SPRITE_PUBLIC_URL");
    assert!(!text_of(&g).contains(lib.to_str().unwrap()), "server paths stay private");

    let atlas = call(&c, "export_atlas", json!({"file": "home", "scale": 2})).await;
    assert!(text_of(&atlas).contains("Exported 2 sprites"), "{}", text_of(&atlas));
    assert!(lib.join("_export/home.json").exists());
    assert!(!text_of(&atlas).contains(lib.to_str().unwrap()), "server paths stay private");
    let huge = call(&c, "export_atlas", json!({"scale": 8, "max_side": 256})).await;
    assert!(text_of(&huge).contains("over the 256px limit"), "{}", text_of(&huge));
    let traversal = call(&c, "get_sprite", json!({"name": "../etc/passwd"})).await;
    assert_eq!(traversal.is_error, Some(true));
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn history_restore_and_conflicts() {
    let lib = library();
    let c = connect(Reviewer::default(), &lib, &[]).await;
    seed(&c).await;
    let rev = |v: &Value| v["rev"].as_u64().unwrap();
    let get = |n: &'static str| {
        let c = &c;
        async move { serde_json::from_str::<Value>(&text_of(&call(c, "get_sprite", json!({"name": n})).await)).unwrap() }
    };
    assert_eq!(rev(&get("jar").await), 1);

    // an edit based on a stale rev is refused and changes nothing
    let ok = call(&c, "edit_sprite", json!({"name": "jar", "if_rev": 1, "prim_ops": [{"op": "merge", "index": 0, "prim": {"m": "pink"}}]})).await;
    assert!(text_of(&ok).contains("(rev 2)"), "{}", text_of(&ok));
    let stale = call(&c, "edit_sprite", json!({"name": "jar", "if_rev": 1, "prim_ops": [{"op": "merge", "index": 0, "prim": {"m": "lime"}}]})).await;
    assert_eq!(stale.is_error, Some(true));
    assert!(text_of(&stale).contains("is at rev 2"), "{}", text_of(&stale));
    assert_eq!(get("jar").await["prims"][0]["m"], "pink");

    // delete goes to the trash and is undoable
    let del = call(&c, "delete_sprite", json!({"name": "jar"})).await;
    assert!(text_of(&del).contains("restore_sprite"), "{}", text_of(&del));
    assert!(!lib.join("jar.json").exists());
    let hist = text_of(&call(&c, "sprite_history", json!({"name": "jar"})).await);
    assert!(hist.contains("currently deleted") && hist.contains("rev   1  create") && hist.contains("local"), "{hist}");
    let back = call(&c, "restore_sprite", json!({"name": "jar", "rev": 1})).await;
    assert_ne!(back.is_error, Some(true), "{}", text_of(&back));
    assert!(back.content.iter().any(|c| matches!(c, ContentBlock::Image(_))));
    let restored = get("jar").await;
    assert_eq!((rev(&restored), restored["prims"][0]["m"].clone()), (3, json!("glass")));

    // a corrupt file is reported, not mistaken for an empty library
    std::fs::write(lib.join("broken.json"), "{ nope").unwrap();
    let list = text_of(&call(&c, "list_sprites", json!({})).await);
    assert!(list.starts_with("2 sprites") && list.contains("broken.json"), "{list}");

    // ...and create won't replace it without overwrite; with overwrite, the damaged bytes are kept
    let prims = json!([{"t": "rect", "p": [8, 8, 24, 28], "m": "glass"}]);
    let refused = call(&c, "create_sprite", json!({"name": "broken", "category": "home", "prims": prims})).await;
    assert_eq!(refused.is_error, Some(true));
    assert!(text_of(&refused).contains("not valid JSON"), "{}", text_of(&refused));
    assert_eq!(std::fs::read(lib.join("broken.json")).unwrap(), b"{ nope");
    let replaced = call(&c, "create_sprite", json!({"name": "broken", "category": "home", "prims": prims, "overwrite": true})).await;
    assert_ne!(replaced.is_error, Some(true), "{}", text_of(&replaced));
    assert_eq!(std::fs::read(lib.join("_history/broken/000000.json")).unwrap(), b"{ nope");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn blind_review_via_mrtr() {
    let lib = library();
    let r = Reviewer { sampling: true, version: Some(ProtocolVersion::V_2026_07_28), ..Default::default() };
    let calls = r.calls.clone();
    let c = connect(r, &lib, &[]).await;
    seed(&c).await;
    let out = call(&c, "blind_review", json!({"names": ["teapot", "jar"]})).await;
    assert!(text_of(&out).contains("/2 passed"), "{}", text_of(&out));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "client answered one input request");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn blind_review_via_classic_sampling() {
    let lib = library();
    let r = Reviewer { sampling: true, version: Some(ProtocolVersion::V_2025_11_25), ..Default::default() };
    let calls = r.calls.clone();
    let c = connect(r, &lib, &[]).await;
    seed(&c).await;
    let out = call(&c, "blind_review", json!({"names": ["teapot", "jar"]})).await;
    assert!(text_of(&out).contains("/2 passed"), "{}", text_of(&out));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn blind_review_fallback_without_reviewer() {
    let lib = library();
    let c = connect(Reviewer::default(), &lib, &[]).await;
    seed(&c).await;
    let out = call(&c, "blind_review", json!({"names": ["teapot", "jar"]})).await;
    let t = text_of(&out);
    assert!(t.contains("No reviewer is available") && t.contains("#1="), "{t}");
    assert!(out.content.iter().any(|c| matches!(c, ContentBlock::Image(_))));
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn export_gif_links_to_the_public_url() {
    let lib = library();
    let c = connect(Reviewer::default(), &lib, &[("SPRITE_PUBLIC_URL", "https://sprites.example.com/")]).await;
    seed(&c).await;
    let g = call(&c, "export_gif", json!({"name": "teapot", "scale": 2, "bg": "#ffffff", "repeat": 3})).await;
    let t = text_of(&g);
    assert!(t.contains("https://sprites.example.com/v1/sprites/teapot.gif?scale=2&bg=ffffff&repeat=3"), "{t}");
    assert!(t.contains("single still frame"), "an unanimated sprite says so: {t}");
    c.cancel().await.unwrap();
}

/// Minimal mock of POST /v1/messages that checks the request shape and returns a canned reply.
fn mock_messages_api() -> (String, std::thread::JoinHandle<Value>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let h = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut rd = BufReader::new(s.try_clone().unwrap());
        let mut len = 0usize;
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            rd.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap();
            }
            headers.push_str(&line);
        }
        let mut body = vec![0u8; len];
        rd.read_exact(&mut body).unwrap();
        let req: Value = serde_json::from_slice(&body).unwrap();
        assert!(headers.to_ascii_lowercase().contains("x-api-key: test-key"));
        let reply = json!({"content": [{"type": "text", "text": "[{\"n\":1,\"guess\":\"jar\",\"conf\":95},{\"n\":2,\"guess\":\"teapot\",\"conf\":95}]"}]}).to_string();
        write!(s, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", reply.len(), reply).unwrap();
        req
    });
    (url, h)
}

#[tokio::test]
async fn blind_review_via_direct_api() {
    let lib = library();
    let (url, server) = mock_messages_api();
    let c = connect(Reviewer { sampling: true, ..Default::default() }, &lib, &[("ANTHROPIC_API_KEY", "test-key"), ("ANTHROPIC_BASE_URL", &url), ("SPRITE_REVIEW_MODEL", "claude-test")]).await;
    seed(&c).await;
    let out = call(&c, "blind_review", json!({"names": ["teapot", "jar"]})).await;
    assert!(text_of(&out).contains("/2 passed"), "{}", text_of(&out));
    let req = server.join().unwrap();
    assert_eq!(req["model"], "claude-test");
    assert_eq!(req["messages"][0]["content"][0]["type"], "image");
    c.cancel().await.unwrap();
}
