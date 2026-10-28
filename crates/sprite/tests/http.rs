//! `sprite serve`: auth, REST endpoints, caching, and MCP over Streamable HTTP sharing the
//! same library as the REST API.

use rmcp::model::{CallToolRequestParams, ContentBlock};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

const TOKEN: &str = "test-token-0123456789abcdef0123456789";
const READ_TOKEN: &str = "read-token-0123456789abcdef0123456789";

struct Server {
    child: Child,
    addr: String,
    lib: std::path::PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn start() -> Server {
    let lib = std::env::temp_dir().join(format!(
        "sprite-http-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(
        lib.join("apple.json"),
        json!({"name":"apple","cat":"food","anim":"bob","prims":[{"t":"ellipse","c":[16,17],"r":[11,10],"m":"red","gloss":true}]}).to_string(),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sprite"))
        .args(["serve", "--bind", "127.0.0.1:0", "--library"])
        .arg(&lib)
        .env("SPRITE_TOKENS", format!("tester:{TOKEN},proto:read:{READ_TOKEN}"))
        .env_remove("ANTHROPIC_API_KEY")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let addr = line.split("http://").nth(1).unwrap().split_whitespace().next().unwrap().to_string();
    Server { child, addr, lib }
}

/// Tiny HTTP/1.1 client: (status, headers, body).
fn req(addr: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> (u16, String, Vec<u8>) {
    let mut s = TcpStream::connect(addr).unwrap();
    let mut h = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in headers {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    h.push_str("\r\n");
    s.write_all(h.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut body = raw[split + 4..].to_vec();
    if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        body = dechunk(&body);
    }
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, head, body)
}

fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let nl = b.windows(2).position(|w| w == b"\r\n").unwrap();
        let n = usize::from_str_radix(std::str::from_utf8(&b[..nl]).unwrap().trim(), 16).unwrap();
        if n == 0 {
            return out;
        }
        out.extend_from_slice(&b[nl + 2..nl + 2 + n]);
        b = &b[nl + 4 + n..];
    }
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

const AUTH: (&str, &str) = ("Authorization", "Bearer test-token-0123456789abcdef0123456789");

#[test]
fn rest_auth_and_assets() {
    let s = start();
    assert_eq!(req(&s.addr, "GET", "/healthz", &[], b"").0, 200);
    // reads are public; a token that doesn't match is still an error, not a silent downgrade
    assert_eq!(req(&s.addr, "GET", "/v1/sprites", &[], b"").0, 200);
    let (st, head, gif) = req(&s.addr, "GET", "/v1/sprites/apple.gif?scale=8", &[], b"");
    assert_eq!((st, header(&head, "content-type")), (200, Some("image/gif")));
    assert_eq!(&gif[..6], b"GIF89a");
    assert_eq!(req(&s.addr, "GET", "/v1/atlas.json", &[], b"").0, 200);
    assert_eq!(req(&s.addr, "GET", "/v1/sprites", &[("Authorization", "Bearer wrong")], b"").0, 401);
    assert_eq!(req(&s.addr, "GET", "/v1/sprites/apple.png?key=wrong", &[], b"").0, 401);
    // a malformed Authorization header is a 401 too; the scheme is case-insensitive
    assert_eq!(req(&s.addr, "GET", "/v1/sprites", &[("Authorization", &format!("Basic {TOKEN}"))], b"").0, 401);
    assert_eq!(req(&s.addr, "GET", "/v1/sprites", &[("Authorization", TOKEN)], b"").0, 401);
    assert_eq!(req(&s.addr, "GET", "/v1/sprites", &[("Authorization", &format!("bearer {TOKEN}"))], b"").0, 200);

    let (st, _, body) = req(&s.addr, "GET", "/v1/sprites", &[AUTH], b"");
    assert_eq!(st, 200);
    let list: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(list["count"], 1);
    assert_eq!(list["sprites"][0]["frames"], 8);

    // ?key= works where headers can't be set
    let (st, head, png) = req(&s.addr, "GET", &format!("/v1/sprites/apple.png?scale=4&key={TOKEN}"), &[], b"");
    assert_eq!(st, 200);
    assert_eq!(header(&head, "content-type"), Some("image/png"));
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 128, "scale 4 → 128px wide");

    // ETag revalidation
    let tag = header(&head, "etag").unwrap().to_string();
    let (st, _, _) = req(&s.addr, "GET", "/v1/sprites/apple.png?scale=4", &[AUTH, ("If-None-Match", &tag)], b"");
    assert_eq!(st, 304);

    let (st, head, gif) = req(&s.addr, "GET", "/v1/sprites/apple.gif", &[AUTH], b"");
    assert_eq!((st, header(&head, "content-type")), (200, Some("image/gif")));
    assert_eq!(&gif[..6], b"GIF89a");
    let plain_tag = header(&head, "etag").unwrap().to_string();
    // GIF options change the bytes and the ETag; bad GIF options are a 400, not a silent default
    let (st, head, pp) = req(&s.addr, "GET", "/v1/sprites/apple.gif?loop=pingpong&bg=f6f5f0&repeat=2", &[AUTH], b"");
    assert_eq!(st, 200);
    assert_ne!(pp, gif);
    assert_ne!(header(&head, "etag").unwrap(), plain_tag);
    assert_eq!(req(&s.addr, "GET", "/v1/sprites/apple.gif?bg=plaid", &[AUTH], b"").0, 400);
    assert_eq!(req(&s.addr, "GET", "/v1/sprites/apple.gif?loop=sideways", &[AUTH], b"").0, 400);
    assert_eq!(req(&s.addr, "GET", "/v1/sprites/apple.gif?repeat=0", &[AUTH], b"").0, 400);
    // GIF options belong to .gif only; other formats ignore them, and ?frame doesn't fork a GIF's ETag
    assert_eq!(req(&s.addr, "GET", "/v1/sprites/apple.png?bg=plaid", &[AUTH], b"").0, 200);
    assert_eq!(req(&s.addr, "GET", "/v1/sprites/apple.sheet.png?loop=x", &[AUTH], b"").0, 200);
    let (_, head, _) = req(&s.addr, "GET", "/v1/sprites/apple.gif?frame=3", &[AUTH], b"");
    assert_eq!(header(&head, "etag").unwrap(), plain_tag);

    let (st, _, sheet) = req(&s.addr, "GET", "/v1/sprites/apple.sheet.png", &[AUTH], b"");
    assert_eq!(st, 200);
    assert_eq!(u32::from_be_bytes(sheet[16..20].try_into().unwrap()), 8 * 32, "8 frames side by side");

    let (st, _, body) = req(&s.addr, "GET", "/v1/atlas.json?scale=2", &[AUTH], b"");
    assert_eq!(st, 200);
    let atlas: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(atlas["meta"]["image"], "atlas.png?scale=2");
    // the token never ends up inside atlas.json
    let (_, _, body) = req(&s.addr, "GET", &format!("/v1/atlas.json?scale=2&key={TOKEN}"), &[], b"");
    let keyed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(keyed["meta"]["image"], "atlas.png?scale=2");
    assert_eq!(req(&s.addr, "GET", "/v1/atlas.png?scale=8&max=256", &[AUTH], b"").0, 413);
    // every frame key resolves to a 64px cell inside the image (identical bob poses may share one)
    let (w, h) = (atlas["meta"]["size"]["w"].as_u64().unwrap(), atlas["meta"]["size"]["h"].as_u64().unwrap());
    for k in 0..8 {
        let f = &atlas["frames"][format!("apple_{k}")]["frame"];
        assert!(f["x"].as_u64().unwrap() + 64 <= w && f["y"].as_u64().unwrap() + 64 <= h && f["w"] == 64, "{f}");
    }
    assert_eq!(req(&s.addr, "GET", "/v1/atlas.png", &[AUTH], b"").0, 200);

    // ad-hoc render: JSON bodies only (so cross-origin pages need a CORS preflight), errors are
    // specific, success returns an image, nothing is saved
    let star = br#"{"name":"tmp","anim":"spin","prims":[{"t":"star","c":[16,16],"r":12,"m":"gold"}]}"#;
    assert_eq!(req(&s.addr, "POST", "/v1/render?format=gif", &[], star).0, 415);
    assert_eq!(req(&s.addr, "POST", "/v1/render?format=gif", &[("Content-Type", "text/plain")], star).0, 415);
    let json_ct = ("Content-Type", "application/json");
    let (st, _, body) = req(&s.addr, "POST", "/v1/render?format=json", &[json_ct], br#"{"prims":[{"t":"blob"}]}"#);
    assert_eq!(st, 422);
    assert!(String::from_utf8_lossy(&body).contains("unknown type"));
    let (st, head, _) = req(&s.addr, "POST", "/v1/render?format=gif", &[("Content-Type", "application/json; charset=utf-8")], star);
    assert_eq!((st, header(&head, "content-type")), (200, Some("image/gif")));
    assert!(!s.lib.join("tmp.json").exists());

    // a 64px sprite: GIFs default to the same 128px as a 32px one at 4×, and cap at 512px
    std::fs::write(
        s.lib.join("ball64.json"),
        json!({"name":"ball64","cat":"toys","size":64,"anim":"bob","prims":[{"t":"ellipse","c":[32,32],"r":22,"m":"blue"}]}).to_string(),
    )
    .unwrap();
    let gif_w = |b: &[u8]| u16::from_le_bytes([b[6], b[7]]);
    let (st, _, gif) = req(&s.addr, "GET", "/v1/sprites/ball64.gif", &[], b"");
    assert_eq!((st, gif_w(&gif)), (200, 128));
    assert_eq!(gif_w(&req(&s.addr, "GET", "/v1/sprites/ball64.gif?scale=16", &[], b"").2), 512);
    let (_, _, png) = req(&s.addr, "GET", "/v1/sprites/ball64.png", &[], b"");
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 64);
    let (_, _, body) = req(&s.addr, "GET", "/v1/atlas.json?names=apple,ball64", &[], b"");
    let mixed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!((mixed["frames"]["ball64_0"]["frame"]["w"].as_u64(), mixed["frames"]["apple_0"]["frame"]["w"].as_u64()), (Some(64), Some(32)));
    let (st, _, gif) = req(&s.addr, "POST", "/v1/render?format=gif", &[json_ct], br#"{"size":128,"prims":[{"t":"star","c":[64,64],"r":50,"m":"gold"}]}"#);
    assert_eq!((st, gif_w(&gif)), (200, 128));

    assert_eq!(req(&s.addr, "GET", "/v1/sprites/nope.png", &[AUTH], b"").0, 404);
    assert_eq!(req(&s.addr, "GET", "/v1/sprites/..%2Fetc%2Fpasswd.json", &[AUTH], b"").0, 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_over_http_shares_the_library() {
    let s = start();
    let url = format!("http://{}/mcp", s.addr);

    // Without a write token in a header, MCP is open for reading and creating, but the keyed
    // tools refuse: anonymous, a read token, and a write token sent as ?key= all land here.
    let init = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;
    let mcp_hdrs = [("Content-Type", "application/json"), ("Accept", "application/json, text/event-stream")];
    assert_eq!(req(&s.addr, "POST", "/mcp", &[mcp_hdrs[0], mcp_hdrs[1], ("Authorization", "Bearer wrong")], init).0, 401);
    assert_eq!(req(&s.addr, "GET", &format!("/v1/sprites/apple.png?key={READ_TOKEN}"), &[], b"").0, 200);

    let call = |name: &'static str, args: Value| {
        let mut p = CallToolRequestParams::new(name);
        p.arguments = args.as_object().cloned();
        p
    };
    let text_of = |r: &rmcp::model::CallToolResult| -> String {
        r.content.iter().filter_map(|c| if let ContentBlock::Text(t) = c { Some(t.text.clone()) } else { None }).collect()
    };
    let star = json!([{"t":"star","c":[16,16],"r":12,"m":"gold"}]);
    let read_cfg = StreamableHttpClientTransportConfig::with_uri(url.clone()).auth_header(READ_TOKEN);
    let query_url = format!("{url}?key={TOKEN}");
    for (who, cfg) in [
        ("anon", StreamableHttpClientTransportConfig::with_uri(url.clone())),
        ("proto", read_cfg),
        ("tester", StreamableHttpClientTransportConfig::with_uri(query_url.clone())),
    ] {
        let c = ().serve(StreamableHttpClientTransport::from_config(cfg)).await.unwrap_or_else(|e| panic!("{who}: connect: {e}"));
        let r = c.call_tool(call("render_sprite", json!({"name": "apple"}))).await.unwrap();
        assert_ne!(r.is_error, Some(true), "{who}: render is public");
        let r = c.call_tool(call("export_gif", json!({"name": "apple"}))).await.unwrap();
        assert_ne!(r.is_error, Some(true), "{who}: export is public");

        let new_name = format!("star_{who}");
        let r = c.call_tool(call("create_sprite", json!({"name": new_name, "category": "nature", "prims": star}))).await.unwrap();
        assert_ne!(r.is_error, Some(true), "{who}: create is public: {}", text_of(&r));
        let hist = c.call_tool(call("sprite_history", json!({"name": new_name}))).await.unwrap();
        assert!(text_of(&hist).contains(who), "{who}: history records the caller: {}", text_of(&hist));

        for (tool, args) in [
            ("edit_sprite", json!({"name": "apple", "if_rev": 0, "prims": star})),
            ("delete_sprite", json!({"name": "apple"})),
            ("restore_sprite", json!({"name": "apple", "rev": 1})),
            ("blind_review", json!({"names": ["apple"]})),
            ("create_sprite", json!({"name": "apple", "category": "food", "overwrite": true, "prims": star})),
            ("export_atlas", json!({})),
        ] {
            let r = c.call_tool(call(tool, args)).await.unwrap();
            assert_eq!(r.is_error, Some(true), "{who}: {tool} must need a write token");
            assert!(text_of(&r).contains("write token"), "{who}: {tool}: {}", text_of(&r));
        }
        c.cancel().await.unwrap();
    }
    // none of the refused calls touched apple, and public export_gif didn't write files
    let apple: Value = serde_json::from_slice(&std::fs::read(s.lib.join("apple.json")).unwrap()).unwrap();
    assert_eq!(apple["prims"][0]["m"], "red");
    assert!(!s.lib.join("_export").exists());

    let cfg = StreamableHttpClientTransportConfig::with_uri(url.clone()).auth_header(TOKEN);
    let client = ().serve(StreamableHttpClientTransport::from_config(cfg)).await.expect("connect with token");
    let tools = client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 12);

    let mut p = CallToolRequestParams::new("create_sprite");
    p.arguments = json!({"name": "moon", "category": "nature", "prims": [{"t":"arc","c":[16,16],"r":12,"a0":-90,"a1":90,"m":"yellow"}]})
        .as_object()
        .cloned();
    let r = client.call_tool(p).await.unwrap();
    assert_ne!(r.is_error, Some(true));
    assert!(r.content.iter().any(|c| matches!(c, ContentBlock::Image(_))));
    // history records which token made the change
    let mut h = CallToolRequestParams::new("sprite_history");
    h.arguments = json!({"name": "moon"}).as_object().cloned();
    let hist = client.call_tool(h).await.unwrap();
    let txt: String = hist.content.iter().filter_map(|c| if let ContentBlock::Text(t) = c { Some(t.text.clone()) } else { None }).collect();
    assert!(txt.contains("create") && txt.contains("tester"), "{txt}");
    let mut e = CallToolRequestParams::new("export_gif");
    e.arguments = json!({"name": "moon"}).as_object().cloned();
    assert_ne!(client.call_tool(e).await.unwrap().is_error, Some(true));
    assert!(s.lib.join("_export/moon.gif").exists(), "a write token's export_gif is saved");

    // once the owner deletes a sprite, the public can't bring the name back with create_sprite
    let mut d = CallToolRequestParams::new("delete_sprite");
    d.arguments = json!({"name": "star_anon"}).as_object().cloned();
    assert_ne!(client.call_tool(d).await.unwrap().is_error, Some(true));
    client.cancel().await.unwrap();
    let anon = ().serve(StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(url.clone()))).await.unwrap();
    let r = anon.call_tool(call("create_sprite", json!({"name": "star_anon", "category": "nature", "prims": star}))).await.unwrap();
    assert_eq!(r.is_error, Some(true));
    assert!(text_of(&r).contains("write token"), "{}", text_of(&r));
    assert!(!s.lib.join("star_anon.json").exists());
    anon.cancel().await.unwrap();

    // what the model just made is immediately available to prototypes over REST
    let (st, _, png) = req(&s.addr, "GET", "/v1/sprites/moon.png", &[AUTH], b"");
    assert_eq!(st, 200);
    assert_eq!(&png[1..4], b"PNG");
}
