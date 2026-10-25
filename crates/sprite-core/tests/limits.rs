//! Specs come from models and over HTTP, so anything that would make the renderer allocate or
//! loop without bound is rejected while parsing, with a message the author can act on.

use serde_json::{json, Value};
use sprite_core::spec::limits;
use sprite_core::{Palette, Spec};

fn errors(v: Value) -> String {
    match Spec::from_json(&v, &Palette::new()) {
        Ok(_) => String::new(),
        Err(e) => e.join("\n"),
    }
}

fn one(prim: Value) -> Value {
    json!({"name": "x", "cat": "misc", "prims": [prim]})
}

#[test]
fn ordinary_specs_still_parse() {
    let ok = one(json!({"t": "poly", "pts": [[4, 4], [28, 4], [16, 28]], "smooth": 3, "weight": 3, "rot": 45}));
    assert_eq!(errors(ok), "");
    let star = one(json!({"t": "star", "c": [16, 16], "r": 12, "n": 12, "r2": 6}));
    assert_eq!(errors(star), "");
    // lines may start a little off-canvas
    let line = one(json!({"t": "line", "pts": [[-12, 4], [54, 30]], "w": 4}));
    assert_eq!(errors(line), "");
}

#[test]
fn smoothing_and_vertex_counts_are_bounded() {
    assert!(errors(one(json!({"t": "poly", "pts": [[4, 4], [28, 4], [16, 28]], "smooth": 24}))).contains("smooth must be between 0 and 4"));
    assert!(errors(one(json!({"t": "ngon", "c": [16, 16], "r": 10, "n": 50_000_000}))).contains("n must be between 3 and 64"));
    let many: Vec<Value> = (0..300).map(|i| json!([i % 30, i % 7])).collect();
    assert!(errors(one(json!({"t": "poly", "pts": many}))).contains("at most 256"));
}

#[test]
fn coordinates_and_sizes_are_bounded() {
    assert!(errors(one(json!({"t": "ellipse", "c": [1e300, 16], "r": 4}))).contains("outside"));
    assert!(errors(one(json!({"t": "ellipse", "c": [16, 16], "r": [4, 5000]}))).contains("r must be between"));
    assert!(errors(one(json!({"t": "line", "pts": [[1, 1], [2, 2]], "w": 900}))).contains("w must be between"));
    assert!(errors(one(json!({"t": "rect", "p": [0, 0, 32, 1e9]}))).contains("outside"));
}

#[test]
fn whole_spec_budgets() {
    let prim = json!({"t": "ellipse", "c": [16, 16], "r": 8});
    let too_many = json!({"name": "x", "prims": vec![prim.clone(); limits::MAX_PRIMS + 1]});
    assert!(errors(too_many).contains("at most 64 prims"));

    let px: Vec<Value> = (0..300).map(|i| json!([i % 32, i / 32])).collect();
    let stamps = json!({"name": "x", "prims": [
        {"t": "px", "pts": px}, {"t": "px", "pts": px}, {"t": "px", "pts": px}, {"t": "px", "pts": px}
    ]});
    assert!(errors(stamps).contains("px prims have 1200 points"));

    let heavy = json!({"name": "x", "anim": "bob", "frames": 64, "prims": vec![prim.clone(); 40]});
    assert!(errors(heavy).contains("prim-frames"));
    // the same prims are fine as a still, or with fewer frames
    assert_eq!(errors(json!({"name": "x", "frames": 64, "prims": vec![prim.clone(); 40]})), "");
    assert_eq!(errors(json!({"name": "x", "anim": "bob", "frames": 16, "prims": vec![prim; 40]})), "");
}

#[test]
fn track_values_are_bounded() {
    let base = |t: Value| json!({"name": "x", "prims": [{"t": "ellipse", "c": [16, 16], "r": 8, "g": "a"}], "tracks": [t]});
    assert!(errors(base(json!({"g": "a", "prop": "dx", "amp": 1e12}))).contains("amp must be within"));
    assert!(errors(base(json!({"g": "a", "prop": "rot", "amp": 30, "pivot": [1e9, 0]}))).contains("pivot"));
    assert_eq!(errors(base(json!({"g": "a", "prop": "rot", "amp": 360, "wave": "saw", "pivot": "each"}))), "");
}
