//! Animation invariants: px stamps stay rigid, `snap` gives whole-pixel motion, timing parses,
//! and every colour the renderer emits is in the fixed output palette.

use serde_json::{json, Value};
use sprite_core::lint::lint;
use sprite_core::{render_anim, Frame, Palette, RenderOpts, Spec, SIZE};
use std::collections::BTreeSet;
use std::path::PathBuf;

fn render(v: &Value) -> (Spec, Vec<Frame>) {
    let pal = Palette::new();
    let s = Spec::from_json(v, &pal).unwrap();
    let f = render_anim(&s, &pal, &RenderOpts::default());
    (s, f)
}

fn pixels_of(f: &Frame, c: [u8; 3]) -> BTreeSet<(i32, i32)> {
    let mut out = BTreeSet::new();
    for y in 0..SIZE {
        for x in 0..SIZE {
            let k = (y * SIZE + x) * 4;
            if f.0[k + 3] > 0 && f.0[k..k + 3] == c {
                out.insert((x as i32, y as i32));
            }
        }
    }
    out
}

fn shifted(s: &BTreeSet<(i32, i32)>, dx: i32, dy: i32) -> BTreeSet<(i32, i32)> {
    s.iter().map(|&(x, y)| (x + dx, y + dy)).collect()
}

/// The "heart eyes hiccup": scaling a group of px pixels used to push each pixel through the
/// matrix on its own, so rows dropped or doubled. Now the whole stamp moves as one.
#[test]
fn px_stamps_stay_rigid_under_scale_and_squash() {
    let heart: Vec<[i32; 2]> = vec![
        [9, 9], [10, 9], [12, 9], [13, 9],
        [8, 10], [9, 10], [10, 10], [11, 10], [12, 10], [13, 10], [14, 10],
        [8, 11], [9, 11], [10, 11], [11, 11], [12, 11], [13, 11], [14, 11],
        [9, 12], [10, 12], [11, 12], [12, 12], [13, 12],
        [10, 13], [11, 13], [12, 13],
        [11, 14],
    ];
    let v = json!({"name": "t", "anim": "idle", "prims": [
        {"t": "ellipse", "c": [16, 16], "r": 13, "m": "brown"},
        {"t": "px", "m": "red", "shade": 2, "g": "heart", "pts": heart},
        {"t": "px", "m": "pink", "shade": 4, "g": "heart", "pts": [[9, 10]]}
    ], "tracks": [
        {"g": "heart", "prop": "sx", "amp": 0.3, "pivot": [11, 11.5], "wave": "abs"},
        {"g": "heart", "prop": "sy", "amp": 0.3, "pivot": [11, 11.5], "wave": "abs"},
        {"g": "*", "prop": "dy", "amp": 1.4}
    ]});
    let (s, frames) = render(&v);
    let pal = Palette::new();
    let red = pal.ramp(pal.index_of("red").unwrap())[2];
    let pink = pal.ramp(pal.index_of("pink").unwrap())[4];
    let red0 = pixels_of(&frames[0], red);
    let pink0 = pixels_of(&frames[0], pink);
    assert_eq!(red0.len() + pink0.len(), heart.len(), "frame 0 shows the whole heart (the glint covers one red px)");
    let glint0 = *pink0.iter().next().unwrap();
    for (k, f) in frames.iter().enumerate() {
        let pink_k = pixels_of(f, pink);
        assert_eq!(pink_k.len(), 1, "frame {k}: one glint pixel");
        let g = *pink_k.iter().next().unwrap();
        let (dx, dy) = (g.0 - glint0.0, g.1 - glint0.1);
        assert_eq!(pixels_of(f, red), shifted(&red0, dx, dy), "frame {k}: heart is frame 0's heart moved by ({dx},{dy})");
    }
    // and the author is told that scaling px does nothing useful
    let l = lint(&s, &frames);
    assert!(l.warnings.iter().any(|w| w.contains("rigid stamps")), "{:?}", l.warnings);
}

/// Every pixel of colour `c` sits on the body: each neighbour is a `body`-material pixel or `c` itself.
/// (Opaque isn't enough: the outline pass rings a stray eye with outline ink.)
fn all_on_body(f: &Frame, c: [u8; 3], body: &str) -> bool {
    let pal = Palette::new();
    let ramp = pal.ramp(pal.index_of(body).unwrap());
    let is_body = |x: i32, y: i32| {
        (0..SIZE as i32).contains(&x) && (0..SIZE as i32).contains(&y) && {
            let k = (y as usize * SIZE + x as usize) * 4;
            let px = [f.0[k], f.0[k + 1], f.0[k + 2]];
            f.0[k + 3] > 0 && (ramp.contains(&px) || px == c)
        }
    };
    pixels_of(f, c).iter().all(|&(x, y)| [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().all(|(dx, dy)| is_body(x + dx, y + dy)))
}

/// Eyes are separate stamps: through a squashing hop they keep their size and spacing.
#[test]
fn eyes_keep_their_size_and_spacing_through_hop() {
    let v = json!({"name": "t", "anim": "hop", "prims": [
        {"t": "ellipse", "c": [16, 18], "r": 11, "m": "orange"},
        {"t": "px", "m": "black", "shade": 0, "pts": [[12, 15], [12, 16], [19, 15], [19, 16]]}
    ]});
    let (_, frames) = render(&v);
    let pal = Palette::new();
    let ink = pal.ramp(pal.index_of("black").unwrap())[0];
    for (k, f) in frames.iter().enumerate() {
        let eyes = pixels_of(f, ink);
        assert_eq!(eyes.len(), 4, "frame {k}: all four eye pixels, got {eyes:?}");
        let xs: Vec<i32> = eyes.iter().map(|p| p.0).collect::<BTreeSet<_>>().into_iter().collect();
        assert_eq!(xs.len(), 2, "frame {k}: two eye columns, got {xs:?}");
        assert_eq!(xs[1] - xs[0], 7, "frame {k}: eyes stay 7px apart");
        assert!(all_on_body(f, ink, "orange"), "frame {k}: eyes stay on the body");
    }
}

/// Whole-sprite rotation carries each eye around with the face (review bug: one shared stamp for
/// every ungrouped px left the eyes level while the head turned, so an eye left the face).
#[test]
fn eyes_ride_a_spinning_face() {
    // a long bar with an eye near each end: if the eyes stayed level, they'd fall off it at 90°
    let v = json!({"name": "t", "anim": "spin", "prims": [
        {"t": "rect", "p": [4, 12, 28, 20], "m": "orange", "puff": 2},
        {"t": "px", "m": "black", "shade": 0, "pts": [[7, 15], [24, 15]]}
    ]});
    let (_, frames) = render(&v);
    let pal = Palette::new();
    let ink = pal.ramp(pal.index_of("black").unwrap())[0];
    for (k, f) in frames.iter().enumerate() {
        assert_eq!(pixels_of(f, ink).len(), 2, "frame {k}: both eyes drawn");
        assert!(all_on_body(f, ink, "orange"), "frame {k}: eyes stay on the face");
    }
}

/// pivot "each" gives every px prim its own transform; its pixels must neither vanish nor slide
/// (review bug: a shared group anchor under per-prim matrices threw them off-canvas).
#[test]
fn px_with_pivot_each_stay_put() {
    let v = json!({"name": "t", "anim": "none", "prims": [
        {"t": "ellipse", "c": [16, 16], "r": 13, "m": "navy"},
        {"t": "px", "m": "yellow", "shade": 4, "g": "spark", "pts": [[9, 9], [10, 9], [9, 10], [10, 10]]},
        {"t": "px", "m": "yellow", "shade": 4, "g": "spark", "pts": [[21, 20], [22, 20], [21, 21], [22, 21]]},
        {"t": "px", "m": "white", "shade": 4, "g": "dot", "pts": [[16, 8]]}
    ], "tracks": [
        {"g": "spark", "prop": "rot", "amp": 360, "wave": "saw", "pivot": "each"},
        {"g": "dot", "prop": "rot", "amp": 360, "wave": "saw", "pivot": "each"}
    ]});
    let (_, frames) = render(&v);
    let pal = Palette::new();
    let spark = pal.ramp(pal.index_of("yellow").unwrap())[4];
    let dot = pal.ramp(pal.index_of("white").unwrap())[4];
    let (s0, d0) = (pixels_of(&frames[0], spark), pixels_of(&frames[0], dot));
    assert_eq!((s0.len(), d0.len()), (8, 1));
    for (k, f) in frames.iter().enumerate() {
        assert_eq!(pixels_of(f, spark), s0, "frame {k}: sparks spin in place");
        assert_eq!(pixels_of(f, dot), d0, "frame {k}: a 1px stamp spinning on itself doesn't orbit");
    }
}

/// HD: px stamps move in output pixels, not whole sprite pixels, so they track smooth motion.
#[test]
fn hd_px_follow_motion_in_output_pixels() {
    let v = json!({"name": "t", "anim": "bob", "prims": [
        {"t": "ellipse", "c": [16, 16], "r": 11, "m": "green"},
        {"t": "px", "m": "black", "shade": 0, "pts": [[14, 14]]}
    ]});
    let pal = Palette::new();
    let s = Spec::from_json(&v, &pal).unwrap();
    let frames = sprite_core::render_anim_hd(&s, &pal, &RenderOpts::default(), 256);
    let ink = pal.ramp(pal.index_of("black").unwrap())[0];
    let tops: BTreeSet<usize> = frames
        .iter()
        .map(|f| (0..256 * 256).find(|&q| f.px[q * 4 + 3] > 0 && f.px[q * 4..q * 4 + 3] == ink).unwrap() / 256)
        .collect();
    // bob is ±1.2 sprite px = ±9.6 output px; whole-sprite-pixel rounding would allow only 8-px steps
    assert!(tops.iter().any(|y| y % 8 != tops.iter().next().unwrap() % 8), "dot rows {tops:?} only move in 8px steps");
}

/// With snap, a bob is a pure whole-pixel translation: every frame is frame 0 moved up or down.
#[test]
fn snap_turns_motion_into_whole_pixel_steps() {
    let v = json!({"name": "t", "anim": "bob", "snap": true, "prims": [
        {"t": "ellipse", "c": [16, 16], "r": 11, "m": "green", "edge": true},
        {"t": "rect", "p": [10, 12, 22, 20], "m": "yellow", "puff": 2},
        {"t": "px", "m": "black", "shade": 0, "pts": [[13, 14], [19, 14]]}
    ]});
    let (s, frames) = render(&v);
    assert!(s.snap);
    let opaque = |f: &Frame| -> BTreeSet<(i32, i32, [u8; 3])> {
        (0..SIZE * SIZE)
            .filter(|&q| f.0[q * 4 + 3] > 0)
            .map(|q| ((q % SIZE) as i32, (q / SIZE) as i32, [f.0[q * 4], f.0[q * 4 + 1], f.0[q * 4 + 2]]))
            .collect()
    };
    let base = opaque(&frames[0]);
    for (k, f) in frames.iter().enumerate() {
        let got = opaque(f);
        let ok = (-2..=2).any(|dy| base.iter().map(|&(x, y, c)| (x, y + dy, c)).collect::<BTreeSet<_>>() == got);
        assert!(ok, "frame {k} is not a whole-pixel shift of frame 0");
    }
}

#[test]
fn timing_fields_parse_and_validate() {
    let pal = Palette::new();
    let base = |extra: Value| {
        let mut v = json!({"name": "t", "anim": "bob", "prims": [{"t": "ellipse", "c": [16, 16], "r": 10}]});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    let s = Spec::from_json(&base(json!({"frames": 4, "ms": 90, "hold": [3, 1, 2]})), &pal).unwrap();
    assert_eq!((s.frames, s.ms), (4, 90));
    assert_eq!((0..4).map(|i| s.frame_ms(i)).collect::<Vec<_>>(), vec![270, 90, 180, 90], "missing holds are 1");
    assert_eq!(render_anim(&s, &pal, &RenderOpts::default()).len(), 4);

    let e = Spec::from_json(&base(json!({"frames": 2, "hold": [1, 1, 1]})), &pal).unwrap_err();
    assert!(e.iter().any(|m| m.contains("hold has 3 entries")), "{e:?}");
    let e = Spec::from_json(&base(json!({"hold": [0, 1.5]})), &pal).unwrap_err();
    assert_eq!(e.iter().filter(|m| m.contains("whole number")).count(), 2, "{e:?}");
    let e = Spec::from_json(&base(json!({"frames": 10_000})), &pal).unwrap_err();
    assert!(e.iter().any(|m| m.contains("at most 64")), "{e:?}");
    // a still sprite renders one frame; hold doesn't stretch it
    let still = Spec::from_json(&json!({"name": "s", "hold": [9], "prims": [{"t": "ellipse", "c": [16, 16], "r": 10}]}), &pal).unwrap();
    assert_eq!(still.frame_ms(0), 110);
}

/// snap rounds each prim's total shift, not each track: two 0.4px tracks add up to a 1px step.
#[test]
fn snap_rounds_the_summed_shift() {
    let v = json!({"name": "t", "snap": true, "frames": 4, "prims": [{"t": "ellipse", "c": [16, 16], "r": 9, "m": "red"}],
        "tracks": [{"prop": "dy", "amp": 0.4}, {"prop": "dy", "amp": 0.4}]});
    let (_, frames) = render(&v);
    let top = |f: &Frame| (0..SIZE * SIZE).find(|&q| f.0[q * 4 + 3] > 0).unwrap() / SIZE;
    // frame 1 is the sine peak: 0.8px total rounds to 1
    assert_eq!(top(&frames[1]), top(&frames[0]) + 1);
}

/// The GIF encoder relies on this: the renderer only ever emits colours from `output_colors`,
/// so one fixed palette encodes every sprite exactly, with room to spare for the background.
#[test]
fn every_rendered_colour_is_in_the_output_palette() {
    let pal = Palette::new();
    let allowed: BTreeSet<[u8; 3]> = pal.output_colors().into_iter().collect();
    assert!(allowed.len() <= 255, "{} colours won't fit a GIF palette with a transparent slot", allowed.len());
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/nouns");
    let mut checked = 0;
    for f in ["animals", "food", "home", "world"] {
        let specs: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(dir.join(format!("{f}.json"))).unwrap()).unwrap();
        for v in specs {
            let s = Spec::from_json(&v, &pal).unwrap();
            for fr in render_anim(&s, &pal, &RenderOpts::default()) {
                for p in fr.0.chunks(4).filter(|p| p[3] > 0) {
                    assert!(allowed.contains(&[p[0], p[1], p[2]]), "{}: colour {:?} is not in output_colors", s.name, &p[..3]);
                }
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 100);
}

/// Same guarantee for HD frames, which `encode_gif` also takes.
#[test]
fn hd_colours_are_in_the_output_palette() {
    let pal = Palette::new();
    let allowed: BTreeSet<[u8; 3]> = pal.output_colors().into_iter().collect();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/nouns");
    let specs: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(dir.join("animals.json")).unwrap()).unwrap();
    for v in specs.iter().take(12) {
        let s = Spec::from_json(v, &pal).unwrap();
        for fr in sprite_core::render_anim_hd(&s, &pal, &RenderOpts::default(), 128) {
            for p in fr.px.chunks(4).filter(|p| p[3] > 0) {
                assert!(allowed.contains(&[p[0], p[1], p[2]]), "{} (HD): colour {:?} is not in output_colors", s.name, &p[..3]);
            }
        }
    }
}
