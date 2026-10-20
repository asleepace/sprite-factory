//! Parity with the original Python renderer: exact palette, and per-sprite pixel mismatch
//! within the noise floor (the Python renderer itself changes ~2% of pixels for a 0.02px shift).

use serde_json::Value;
use sprite_core::{render_anim, Palette, RenderOpts, Spec, SIZE};
use std::path::PathBuf;

fn testdata() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

fn decode(path: &std::path::Path) -> (usize, usize, Vec<u8>) {
    let mut dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
    dec.set_transformations(png::Transformations::EXPAND);
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size().unwrap()];
    let info = r.next_frame(&mut buf).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    (info.width as usize, info.height as usize, buf)
}

#[test]
fn palette_matches_python_exactly() {
    let want: serde_json::Map<String, Value> =
        serde_json::from_str(&std::fs::read_to_string(testdata().join("golden/_palette.json")).unwrap()).unwrap();
    let pal = Palette::new();
    for (name, ramp) in pal.iter() {
        let w: Vec<[u8; 3]> = serde_json::from_value(want[name].clone()).unwrap();
        assert_eq!(&w[..], &ramp[..], "ramp {name}");
    }
}

#[test]
fn renders_match_python_within_noise() {
    let pal = Palette::new();
    let mut worst = (0.0f64, String::new());
    let mut total = 0.0;
    let mut n = 0;
    for f in ["animals", "food", "home", "world"] {
        let specs: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(testdata().join(format!("nouns/{f}.json"))).unwrap()).unwrap();
        for v in specs {
            let s = Spec::from_json(&v, &pal).unwrap();
            let frames = render_anim(&s, &pal, &RenderOpts::default());
            let (w, h, refpx) = decode(&testdata().join(format!("golden/{}.png", s.name)));
            assert_eq!((w, h), (SIZE * frames.len(), SIZE), "{}: frame count", s.name);
            let mut bad = 0;
            for (k, fr) in frames.iter().enumerate() {
                for y in 0..SIZE {
                    for x in 0..SIZE {
                        let a = &fr.0[(y * SIZE + x) * 4..(y * SIZE + x) * 4 + 4];
                        let i = (y * w + k * SIZE + x) * 4;
                        let b = &refpx[i..i + 4];
                        if !(a[3] == 0 && b[3] == 0) && a != b {
                            bad += 1;
                        }
                    }
                }
            }
            let pct = 100.0 * bad as f64 / (w * h) as f64;
            total += pct;
            n += 1;
            if pct > worst.0 {
                worst = (pct, s.name.clone());
            }
        }
    }
    let mean = total / n as f64;
    eprintln!("{n} sprites, mean mismatch {mean:.2}%, worst {} {:.2}%", worst.1, worst.0);
    assert_eq!(n, 100);
    assert!(mean < 3.0, "mean mismatch {mean:.2}%");
    assert!(worst.0 < 6.0, "worst sprite {} at {:.2}%", worst.1, worst.0);
}

#[test]
fn invalid_specs_report_every_problem() {
    let pal = Palette::new();
    let v = serde_json::json!({"name": "x", "anim": "dance", "prims": [
        {"t": "blob"}, {"t": "ellipse", "c": [16, 16], "r": 8, "m": "plaid"}, {"t": "rect", "p": [1, 1, 5, 5], "clip": 3}
    ]});
    let errs = Spec::from_json(&v, &pal).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("unknown anim")));
    assert!(errs.iter().any(|e| e.contains("unknown type")));
    assert!(errs.iter().any(|e| e.contains("unknown material")));
    assert!(errs.iter().any(|e| e.contains("EARLIER")));
}

#[test]
fn rendering_is_deterministic() {
    let pal = Palette::new();
    let v = serde_json::json!({"name": "s", "anim": "spin", "prims": [{"t": "star", "c": [16, 16], "r": 13, "m": "gold"}]});
    let s = Spec::from_json(&v, &pal).unwrap();
    let a = render_anim(&s, &pal, &RenderOpts::default());
    let b = render_anim(&s, &pal, &RenderOpts::default());
    assert!(a == b);
    assert_eq!(a.len(), 8);
}

#[test]
fn hd_renders_the_same_spec_at_higher_resolution() {
    let pal = Palette::new();
    let v = serde_json::json!({"name": "b", "anim": "bob", "prims": [
        {"t": "ellipse", "c": [16, 16], "r": 10, "m": "red", "edge": true},
        {"t": "px", "pts": [[14, 14]], "m": "black", "shade": 0}
    ]});
    let s = Spec::from_json(&v, &pal).unwrap();
    assert!(!sprite_core::hd_size_ok(100) && !sprite_core::hd_size_ok(32) && sprite_core::hd_size_ok(256));
    let frames = sprite_core::render_anim_hd(&s, &pal, &RenderOpts::default(), 128);
    assert_eq!(frames.len(), 8);
    let f = &frames[0];
    assert_eq!(f.px.len(), 128 * 128 * 4);
    // the ball covers roughly the same fraction of the canvas as at 32px (~31%, plus outline)
    let opaque = f.px.chunks(4).filter(|p| p[3] > 0).count() as f64 / (128.0 * 128.0);
    assert!((0.28..0.40).contains(&opaque), "coverage {opaque}");
    // edges stay hard: every pixel is fully opaque or fully transparent
    assert!(f.px.chunks(4).all(|p| p[3] == 0 || p[3] == 255));
}
