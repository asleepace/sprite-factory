//! Canvas sizes: a spec's `size` (32, 64, 128) sets the canvas, every size-tuned limit and check
//! scales with it, and a shape drawn at 2× on a 64px canvas looks like a crisper copy of the
//! 32px one rather than a different style.

use serde_json::{json, Value};
use sprite_core::lint::lint;
use sprite_core::{render_anim, Frame, Palette, RenderOpts, Spec};
use std::collections::HashMap;
use std::path::PathBuf;

fn testdata() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

fn errors(v: Value) -> String {
    match Spec::from_json(&v, &Palette::new()) {
        Ok(_) => String::new(),
        Err(e) => e.join("\n"),
    }
}

fn render(v: &Value) -> (Spec, Vec<Frame>) {
    let pal = Palette::new();
    let s = Spec::from_json(v, &pal).unwrap_or_else(|e| panic!("{e:?}"));
    let f = render_anim(&s, &pal, &RenderOpts::default());
    (s, f)
}

fn rgb(f: &Frame, x: usize, y: usize) -> Option<[u8; 3]> {
    let k = (y * f.size() + x) * 4;
    (f.0[k + 3] > 0).then(|| [f.0[k], f.0[k + 1], f.0[k + 2]])
}

/// colour → shade 0–4 of its material, or 5 for an outline colour. A few outline blends land on
/// a ramp colour (black's is near black's own shades); those count as the ramp colour.
fn shade_table() -> HashMap<[u8; 3], usize> {
    let pal = Palette::new();
    let mut t = HashMap::new();
    for i in 0..pal.names().len() {
        t.insert(pal.outline(i), 5);
    }
    for (_, ramp) in pal.iter() {
        for (k, c) in ramp.iter().enumerate() {
            t.insert(*c, k);
        }
    }
    t
}

#[test]
fn size_is_32_64_or_128() {
    let pal = Palette::new();
    let base = |size: Value| json!({"name": "x", "size": size, "prims": [{"t": "ellipse", "c": [16, 16], "r": 8}]});
    assert!(errors(base(json!(48))).contains("size must be one of [32, 64, 128]"));
    assert!(errors(base(json!("big"))).contains("size must be one of"));
    for size in [32, 64, 128] {
        let s = Spec::from_json(&base(json!(size)), &pal).unwrap();
        assert_eq!(s.size, size);
        let f = render_anim(&s, &pal, &RenderOpts::default());
        assert_eq!(f[0].size(), size);
    }
    assert_eq!(Spec::from_json(&json!({"name": "x", "prims": [{"t": "ellipse", "c": [16, 16], "r": 8}]}), &pal).unwrap().size, 32);
}

#[test]
fn limits_scale_with_the_canvas() {
    let at = |size: usize, prim: Value| errors(json!({"name": "x", "size": size, "prims": [prim]}));
    // coordinates may sit a canvas off either side: −64…96 at 32, −128…192 at 64, −256…384 at 128
    let far = json!({"t": "ellipse", "c": [150, 32], "r": 4});
    assert!(at(32, far.clone()).contains("outside -64..96"));
    assert_eq!(at(64, far.clone()), "");
    assert!(at(64, json!({"t": "ellipse", "c": [200, 32], "r": 4})).contains("outside -128..192"));
    assert_eq!(at(128, json!({"t": "ellipse", "c": [380, 32], "r": 4})), "");
    // radii and widths: 64 at 32px, 128 at 64px
    assert!(at(32, json!({"t": "ellipse", "c": [16, 16], "r": 100})).contains("r must be between 0 and 64"));
    assert_eq!(at(64, json!({"t": "ellipse", "c": [16, 16], "r": 100})), "");
    // points per prim: 256 × size/32
    let pts: Vec<Value> = (0..400).map(|i| json!([i % 60, i % 13])).collect();
    assert!(at(32, json!({"t": "poly", "pts": pts})).contains("at most 256"));
    assert_eq!(at(64, json!({"t": "poly", "pts": pts})), "");
    // px points across the spec: 1024 × (size/32)²
    let px: Vec<Value> = (0..250).map(|i| json!([i % 60, i / 60])).collect();
    let five = |size: usize| errors(json!({"name": "x", "size": size, "prims": vec![json!({"t": "px", "pts": px}); 5]}));
    assert!(five(32).contains("px prims have 1250 points in total; at most 1024"));
    assert_eq!(five(64), "");
    // dx/dy track amplitude and pivots scale too
    let track = |size: usize, amp: f64| {
        errors(json!({"name": "x", "size": size, "prims": [{"t": "ellipse", "c": [16, 16], "r": 8, "g": "a"}],
            "tracks": [{"g": "a", "prop": "dx", "amp": amp, "pivot": [16, 0]}]}))
    };
    assert!(track(32, 100.0).contains("amp must be within ±64"));
    assert_eq!(track(64, 100.0), "");
}

/// A red ball of radius `r` resting on the floor, on a `size` canvas.
fn ball(size: usize, cx: f64, r: f64, cat: &str) -> Value {
    json!({"name": "b", "cat": cat, "size": size, "prims": [{"t": "ellipse", "c": [cx, size as f64 - 2.0 - r], "r": r, "m": "red"}]})
}

#[test]
fn lint_thresholds_scale_with_the_canvas() {
    for size in [32, 64, 128] {
        let u = (size / 32) as f64;
        let (s, f) = render(&ball(size, size as f64 / 2.0, 13.5 * u, "animals"));
        // (one material is always "only N colours"; that check doesn't depend on size)
        let w: Vec<String> = lint(&s, &f).warnings.into_iter().filter(|w| !w.contains("colours")).collect();
        assert!(w.is_empty(), "{size}: a canvas-filling ball resting on the floor: {w:?}");

        let (s, f) = render(&ball(size, size as f64 / 2.0, 7.0 * u, "food"));
        let w = lint(&s, &f).warnings.join("\n");
        assert!(w.contains(&format!("aim for {}–{}px so it fills the {size}px canvas", 26 * size / 32, 30 * size / 32)), "{size}: {w}");

        // floating above the floor by 4 sprite px (4u canvas px)
        let up = json!({"name": "b", "cat": "animals", "size": size,
            "prims": [{"t": "ellipse", "c": [size as f64 / 2.0, size as f64 - 6.0 * u - 13.5 * u], "r": 13.5 * u, "m": "red"}]});
        let (s, f) = render(&up);
        let w = lint(&s, &f).warnings.join("\n");
        assert!(w.contains(&format!("should rest near y={}", size - 2)), "{size}: {w}");

        let (s, f) = render(&ball(size, 13.0 * u, 13.5 * u, "food"));
        let w = lint(&s, &f).warnings.join("\n");
        assert!(w.contains(&format!("keep shapes inside 1.5–{}.5", size - 2)), "{size}: {w}");
    }
}

#[test]
fn preset_motion_scales_with_the_canvas() {
    // the top edge of a bobbing ball travels 2× as many canvas px at 64 as at 32
    let travel = |size: usize| {
        let u = (size / 32) as f64;
        let mut v = ball(size, size as f64 / 2.0, 10.0 * u, "food");
        v["anim"] = json!("hop");
        let (_, f) = render(&v);
        let tops: Vec<usize> = f.iter().map(|fr| (0..fr.size()).find(|&y| (0..fr.size()).any(|x| fr.alpha(x, y) > 0)).unwrap()).collect();
        tops.iter().max().unwrap() - tops.iter().min().unwrap()
    };
    let (t32, t64, t128) = (travel(32), travel(64), travel(128));
    assert!(t32 >= 2, "hop moves at 32px: {t32}");
    assert!(t64.abs_diff(2 * t32) <= 1 && t128.abs_diff(4 * t32) <= 2, "hop travel {t32} / {t64} / {t128}");
}

#[test]
fn outline_is_one_pixel_at_every_size() {
    let table = shade_table();
    for size in [32, 64, 128] {
        let u = (size / 32) as f64;
        let v = json!({"name": "b", "size": size, "prims": [
            {"t": "ellipse", "c": [size as f64 / 2.0, size as f64 / 2.0], "r": 12.0 * u, "m": "red"},
            {"t": "rect", "p": [6.0 * u, 6.0 * u, 12.0 * u, 26.0 * u], "m": "blue"}
        ]});
        let (_, f) = render(&v);
        let f = &f[0];
        let n = f.size();
        let (ring, outline) = outline_stats(f, &table);
        assert_eq!(ring, outline, "{size}: every silhouette pixel is outline, and outline is nothing else");
        assert!(outline > 2 * n, "{size}: {outline} outline px");
    }
}

/// (silhouette pixels: opaque with a transparent 4-neighbour, outline-coloured pixels), and
/// asserts the outline is 1px: every outline pixel borders the body (an opaque non-outline
/// pixel). In a 2px outline the outer layer would only touch outline and empty space.
fn outline_stats(f: &Frame, table: &HashMap<[u8; 3], usize>) -> (usize, usize) {
    let n = f.size() as i32;
    let at = |x: i32, y: i32| if x < 0 || y < 0 || x >= n || y >= n { None } else { rgb(f, x as usize, y as usize) };
    let (mut ring, mut outline) = (0, 0);
    for y in 0..n {
        for x in 0..n {
            let Some(c) = at(x, y) else { continue };
            let nb = [at(x - 1, y), at(x + 1, y), at(x, y - 1), at(x, y + 1)];
            let is_outline = table.get(&c) == Some(&5);
            if is_outline {
                let body = nb.iter().any(|q| q.is_some_and(|q| table.get(&q) != Some(&5)));
                assert!(body, "outline pixel ({x},{y}) doesn't border the body: thicker than 1px");
            }
            ring += nb.iter().any(Option::is_none) as usize;
            outline += is_outline as usize;
        }
    }
    (ring, outline)
}

/// Shading noise: a pixel at a lit shade (1–4) of the same material as its four neighbours, which
/// all share one other shade. px pixels (eyes, a smile) and 1px edge seams are drawn on purpose,
/// so a different material or shade 0 doesn't count.
fn speckles(f: &Frame) -> usize {
    let pal = Palette::new();
    let mut mat: HashMap<[u8; 3], (usize, usize)> = HashMap::new();
    for (i, (_, ramp)) in pal.iter().enumerate() {
        for (k, c) in ramp.iter().enumerate() {
            mat.insert(*c, (i, k));
        }
    }
    let n = f.size();
    let mut count = 0;
    for y in 1..n - 1 {
        for x in 1..n - 1 {
            let Some(&(m, k)) = rgb(f, x, y).and_then(|c| mat.get(&c)) else { continue };
            let nb = [rgb(f, x - 1, y), rgb(f, x + 1, y), rgb(f, x, y - 1), rgb(f, x, y + 1)];
            let Some(&(nm, nk)) = nb[0].and_then(|c| mat.get(&c)) else { continue };
            if k > 0 && nb.iter().all(|q| *q == nb[0]) && nm == m && nk != k {
                count += 1;
            }
        }
    }
    count
}

#[test]
fn large_domes_shade_without_speckle() {
    for size in [32, 64, 128] {
        let u = (size / 32) as f64;
        for gloss in [false, true] {
            let v = json!({"name": "b", "size": size, "prims": [
                {"t": "ellipse", "c": [size as f64 / 2.0, size as f64 / 2.0], "r": 14.0 * u, "m": "teal", "gloss": gloss},
                {"t": "rect", "p": [4.0 * u, 20.0 * u, 28.0 * u, 29.0 * u], "round": 3.0 * u, "m": "wood"}
            ]});
            let (_, f) = render(&v);
            assert_eq!(speckles(&f[0]), 0, "{size}px gloss={gloss}");
        }
    }
}

/// Fraction of a sprite's opaque pixels at each shade (0–4) and outline (5).
fn shade_mix(f: &Frame) -> [f64; 6] {
    let table = shade_table();
    let mut h = [0f64; 6];
    let n = f.size();
    for y in 0..n {
        for x in 0..n {
            if let Some(c) = rgb(f, x, y) {
                h[table[&c]] += 1.0;
            }
        }
    }
    // the outline and edge seams (shade 0) stay 1px at every size, so they're a smaller share
    // of a bigger sprite; compare the body's lit shades (1–4) on their own
    let body: f64 = h[1..5].iter().sum();
    let mut out = [0f64; 6];
    for k in 1..5 {
        out[k] = h[k] / body;
    }
    out
}

#[test]
fn two_x_on_a_64_canvas_shades_like_the_32px_sprite() {
    let base: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(testdata().join("../prototype/nouns/_test.json")).unwrap()).unwrap();
    let doubled: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(testdata().join("hd/test_x2.json")).unwrap()).unwrap();
    assert_eq!(base.len(), doubled.len());
    for (a, b) in base.iter().zip(&doubled) {
        let (sa, fa) = render(a);
        let (sb, fb) = render(b);
        assert_eq!((fa[0].size(), fb[0].size()), (32, 64));
        let (ma, mb) = (shade_mix(&fa[0]), shade_mix(&fb[0]));
        let dist: f64 = ma.iter().zip(&mb).map(|(x, y)| (x - y).abs()).sum();
        eprintln!("{}: shades 1–4 at 32 {:.2?} vs 64 {:.2?}, L1 {dist:.3}", sa.name, &ma[1..5], &mb[1..5]);
        assert!(dist < 0.2, "{} at 2× shades differently (L1 {dist:.3})", sb.name);
        // same frame count, and both fill their canvas the same way
        assert_eq!(fa.len(), fb.len());
        let (la, lb) = (lint(&sa, &fa), lint(&sb, &fb));
        // the 1px outline doesn't double, so 2× spans 2·span − 2
        assert!(lb.span_px.abs_diff(2 * la.span_px - 2) <= 2, "{}: span {} vs {}", sa.name, la.span_px, lb.span_px);
    }
}

fn decode(path: &std::path::Path) -> (usize, usize, Vec<u8>) {
    let mut dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
    dec.set_transformations(png::Transformations::EXPAND);
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size().unwrap()];
    let info = r.next_frame(&mut buf).unwrap();
    (info.width as usize, info.height as usize, buf)
}

#[test]
fn skater_hotdog_golden() {
    let v: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(testdata().join("hd/skater_hotdog.json")).unwrap()).unwrap();
    let (s, frames) = render(&v[0]);
    assert_eq!((s.size, frames.len()), (64, 8));

    // byte-identical to the reviewed render (every frame, side by side)
    let (w, h, want) = decode(&testdata().join("hd/skater_hotdog_sheet.png"));
    assert_eq!((w, h), (64 * 8, 64));
    for (k, f) in frames.iter().enumerate() {
        for y in 0..64 {
            let got = &f.0[y * 64 * 4..(y + 1) * 64 * 4];
            let row = &want[(y * w + k * 64) * 4..(y * w + k * 64 + 64) * 4];
            assert!(got == row, "frame {k} row {y} differs from testdata/hd/skater_hotdog_sheet.png");
        }
    }

    let l = lint(&s, &frames);
    assert!(!l.warnings.iter().any(|w| w.contains("edge") || w.contains("spans")), "{:?}", l.warnings);
    assert!(l.colors >= 20, "a 64px sprite this busy uses many shades: {}", l.colors);

    let pal = Palette::new();
    let table = shade_table();
    let red = pal.ramp(pal.index_of("red").unwrap());
    let tan = pal.ramp(pal.index_of("tan").unwrap());
    for f in &frames {
        // no speckle anywhere, even on the big glossy sausage
        assert_eq!(speckles(f), 0);
        // the outline is 1px (outline_stats asserts it) and goes all the way round
        let (ring, outline) = outline_stats(f, &table);
        assert!(outline >= ring, "{outline} outline px, {ring} silhouette px");
    }
    // edge seams: where the tan bun overlaps the red sausage, the sausage side gets its darkest red
    let f = &frames[0];
    let mut seams = 0;
    for y in 1..63 {
        for x in 1..63 {
            if rgb(f, x, y) == Some(red[0]) && [rgb(f, x - 1, y), rgb(f, x + 1, y)].iter().any(|c| c.is_some_and(|c| tan.contains(&c))) {
                seams += 1;
            }
        }
    }
    assert!(seams >= 20, "bun/sausage seam pixels: {seams}");
}
