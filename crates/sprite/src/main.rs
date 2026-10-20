//! `sprite` — Sprite Factory CLI and MCP server in one binary.

mod font;
mod http;
mod imgio;
mod library;
mod mcp;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use serde_json::Value;
use sprite_core::{render_anim, Frame, Palette, RenderOpts, Spec, SIZE};
use std::path::PathBuf;
use std::time::Instant;

use imgio::Rgba;

#[derive(Parser)]
#[command(name = "sprite", version, about = "Sprite Factory: shape specs → consistent pixel-art sprites")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Render specs to PNG (first frame), PNG strip and animated GIF
    Render {
        files: Vec<PathBuf>,
        #[arg(long, default_value = "out")]
        out: PathBuf,
        #[arg(long, num_args = 1..)]
        only: Vec<String>,
    },
    /// Labelled contact sheet of first frames (what a model looks at to critique)
    Sheet {
        files: Vec<PathBuf>,
        #[arg(long, default_value = "sheet.png")]
        out: PathBuf,
        #[arg(long, default_value_t = 4)]
        scale: usize,
        #[arg(long, default_value_t = 10)]
        cols: usize,
        #[arg(long, num_args = 1..)]
        only: Vec<String>,
        /// label with #1, #2… instead of names (for blind review)
        #[arg(long)]
        numbered: bool,
    },
    /// Every animation frame, one row per sprite
    Strips {
        files: Vec<PathBuf>,
        #[arg(long, default_value = "strips.png")]
        out: PathBuf,
        #[arg(long, default_value_t = 3)]
        scale: usize,
        #[arg(long, num_args = 1..)]
        only: Vec<String>,
    },
    /// Pack sprites into a PNG atlas + JSON frame map
    Atlas {
        files: Vec<PathBuf>,
        /// output base path (writes BASE.png and BASE.json)
        #[arg(long, default_value = "atlas")]
        out: PathBuf,
        #[arg(long, default_value_t = 1)]
        scale: usize,
    },
    /// Print the palette as JSON, or write a swatch image with --out
    Palette {
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Compare renders against reference PNG strips (golden-image test)
    Golden {
        /// folder of NAME.png strips
        dir: PathBuf,
        files: Vec<PathBuf>,
        /// fail if mean pixel mismatch exceeds this percentage
        #[arg(long, default_value_t = 3.0)]
        max_mean: f64,
        #[arg(long, default_value_t = 0.5)]
        poly_grow: f64,
        /// write a side-by-side diff image of the worst sprites
        #[arg(long)]
        diff: Option<PathBuf>,
    },
    /// Time rendering every frame of every spec
    Bench {
        files: Vec<PathBuf>,
        #[arg(long, default_value_t = 1)]
        repeat: usize,
    },
    /// Private HTTP server: MCP at /mcp + read-only REST for prototypes (bearer-token auth)
    Serve {
        #[arg(long, env = "SPRITE_BIND", default_value = "127.0.0.1:8787")]
        bind: std::net::SocketAddr,
        #[arg(long, env = "SPRITE_LIBRARY", default_value = "library")]
        library: PathBuf,
        /// file with one `label:token` per line (also reads SPRITE_TOKENS, comma-separated)
        #[arg(long, env = "SPRITE_TOKENS_FILE")]
        tokens_file: Option<PathBuf>,
        /// public hostname(s) the MCP endpoint should accept, e.g. sprites.asleepace.com
        #[arg(long = "host", env = "SPRITE_HOSTS", value_delimiter = ',')]
        hosts: Vec<String>,
        /// browser origins allowed to call the API (CORS), e.g. http://localhost:5173
        #[arg(long = "cors", env = "SPRITE_CORS", value_delimiter = ',')]
        cors: Vec<String>,
    },
    /// Run the MCP server over stdio
    Mcp {
        /// folder of one-JSON-per-sprite specs
        #[arg(long, env = "SPRITE_LIBRARY", default_value = "library")]
        library: PathBuf,
    },
}

fn filtered(files: &[PathBuf], only: &[String]) -> Result<Vec<Value>> {
    let mut specs = library::load_specs(files)?;
    if !only.is_empty() {
        specs.retain(|s| only.iter().any(|o| o == library::spec_name(s)));
    }
    if specs.is_empty() {
        bail!("no specs matched");
    }
    Ok(specs)
}

/// Parse every spec, then render them across all cores.
fn render_all(specs: &[Value], pal: &Palette, opts: &RenderOpts) -> Result<Vec<(Value, Vec<Frame>)>> {
    let parsed: Vec<Spec> = specs
        .iter()
        .map(|v| Spec::from_json(v, pal).map_err(|e| anyhow::anyhow!("{}: {}", library::spec_name(v), e.join("; "))))
        .collect::<Result<_>>()?;
    let frames = library::par_map(&parsed, |s| render_anim(s, pal, opts));
    Ok(specs.iter().cloned().zip(frames).collect())
}


fn main() -> Result<()> {
    let cli = Cli::parse();
    let pal = Palette::new();
    let opts = RenderOpts::default();
    match cli.cmd {
        Cmd::Render { files, out, only } => {
            let specs = filtered(&files, &only)?;
            for (v, frames) in render_all(&specs, &pal, &opts)? {
                let ms = v.get("ms").and_then(Value::as_u64).unwrap_or(110) as u32;
                library::save_sprite_outputs(&out, library::spec_name(&v), &frames, ms)?;
            }
            println!("rendered {} → {}", specs.len(), out.display());
        }
        Cmd::Sheet { files, out, scale, cols, only, numbered } => {
            let specs = filtered(&files, &only)?;
            let items: Vec<_> = render_all(&specs, &pal, &opts)?
                .into_iter()
                .enumerate()
                .map(|(i, (v, f))| (if numbered { format!("#{}", i + 1) } else { library::spec_name(&v).to_string() }, f[0].clone()))
                .collect();
            imgio::write(&out, &imgio::contact_sheet(&items, scale, cols).png()?)?;
            println!("sheet {} → {}", items.len(), out.display());
        }
        Cmd::Strips { files, out, scale, only } => {
            let specs = filtered(&files, &only)?;
            let rows: Vec<_> =
                render_all(&specs, &pal, &opts)?.into_iter().map(|(v, f)| (library::spec_name(&v).to_string(), f)).collect();
            imgio::write(&out, &imgio::strip_sheet(&rows, scale).png()?)?;
            println!("strips {} → {}", rows.len(), out.display());
        }
        Cmd::Atlas { files, out, scale } => {
            let specs = filtered(&files, &[])?;
            let rendered = render_all(&specs, &pal, &opts)?;
            let png_path = out.with_extension("png");
            let name = png_path.file_name().unwrap().to_string_lossy().to_string();
            let (img, meta) = library::atlas(&rendered, scale.max(1), &name)?;
            imgio::write(&png_path, &img.png()?)?;
            imgio::write(&out.with_extension("json"), serde_json::to_string_pretty(&meta)?.as_bytes())?;
            println!("atlas {}×{} ({} sprites) → {}", img.w, img.h, rendered.len(), png_path.display());
        }
        Cmd::Palette { out } => match out {
            Some(path) => {
                let k = 16;
                let names = pal.names();
                let mut img = imgio::Rgb::new(5 * k + 8 + font::text_width("charcoal", 1), names.len() * k, [255, 255, 255]);
                for (r, (name, ramp)) in pal.iter().enumerate() {
                    for (i, c) in ramp.iter().enumerate() {
                        img.fill(i * k, r * k, k - 1, k - 1, *c);
                    }
                    img.text(5 * k + 4, r * k + 4, name, 1, [0, 0, 0]);
                }
                imgio::write(&path, &img.png()?)?;
                println!("palette → {}", path.display());
            }
            None => {
                let m: serde_json::Map<String, Value> =
                    pal.iter().map(|(n, r)| (n.to_string(), serde_json::json!(r))).collect();
                println!("{}", serde_json::to_string_pretty(&m)?);
            }
        },
        Cmd::Golden { dir, files, max_mean, poly_grow, diff } => {
            let opts = RenderOpts { poly_grow };
            let specs = filtered(&files, &[])?;
            let mut rows = Vec::new();
            for (v, frames) in render_all(&specs, &pal, &opts)? {
                let name = library::spec_name(&v).to_string();
                let path = dir.join(format!("{name}.png"));
                if !path.exists() {
                    println!("skip {name}: no reference");
                    continue;
                }
                let refimg = imgio::decode_png(&path)?;
                let ours = Rgba::strip(&frames);
                if refimg.w != ours.w || refimg.h != ours.h {
                    println!("FRAMES {name}: reference {}×{}, ours {}×{}", refimg.w, refimg.h, ours.w, ours.h);
                    rows.push((name, 100.0, ours, refimg));
                    continue;
                }
                let n = ours.w * ours.h;
                let mut bad = 0usize;
                for i in 0..n {
                    let (a, b) = (&ours.px[i * 4..i * 4 + 4], &refimg.px[i * 4..i * 4 + 4]);
                    let same = if a[3] == 0 && b[3] == 0 { true } else { a == b };
                    if !same {
                        bad += 1;
                    }
                }
                rows.push((name, 100.0 * bad as f64 / n as f64, ours, refimg));
            }
            rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            let mean = rows.iter().map(|r| r.1).sum::<f64>() / rows.len().max(1) as f64;
            let exact = rows.iter().filter(|r| r.1 == 0.0).count();
            println!("worst:");
            for r in rows.iter().take(10) {
                println!("  {:>6.2}%  {}", r.1, r.0);
            }
            let mut sorted: Vec<f64> = rows.iter().map(|r| r.1).collect();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let median = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
            println!(
                "{} sprites · mean mismatch {:.2}% · median {:.2}% · pixel-identical {}",
                rows.len(),
                mean,
                median,
                exact
            );
            if let Some(path) = diff {
                // top 8: reference (left) vs ours (right), first frame, 4x
                let take: Vec<_> = rows.iter().take(8).collect();
                let s = 4;
                let cell = SIZE * s;
                let mut img = imgio::Rgb::new(cell * 2 + 24, take.len().max(1) * (cell + 18), [236, 232, 224]);
                for (k, (name, pct, ours, refimg)) in take.iter().enumerate() {
                    let y = k * (cell + 18);
                    let crop = |src: &Rgba| {
                        let mut c = Rgba::new(SIZE, SIZE);
                        for yy in 0..SIZE {
                            for xx in 0..SIZE.min(src.w) {
                                let si = (yy * src.w + xx) * 4;
                                c.px[(yy * SIZE + xx) * 4..(yy * SIZE + xx) * 4 + 4].copy_from_slice(&src.px[si..si + 4]);
                            }
                        }
                        c.scaled(s)
                    };
                    img.fill(4, y + 4, cell, cell, imgio::BACKDROP);
                    img.fill(cell + 16, y + 4, cell, cell, imgio::BACKDROP);
                    img.paste(&crop(refimg), 4, y + 4);
                    img.paste(&crop(ours), cell + 16, y + 4);
                    img.text(4, y + cell + 7, &format!("{name} {pct:.1}"), 1, [40, 40, 50]);
                }
                imgio::write(&path, &img.png()?)?;
            }
            if mean > max_mean {
                bail!("mean mismatch {mean:.2}% exceeds {max_mean}%");
            }
        }
        Cmd::Bench { files, repeat } => {
            let specs = filtered(&files, &[])?;
            let parsed: Vec<Spec> = specs.iter().map(|v| Spec::from_json(v, &pal).map_err(|e| anyhow::anyhow!(e.join("; ")))).collect::<Result<_>>()?;
            let t0 = Instant::now();
            let mut frames = 0;
            for _ in 0..repeat.max(1) {
                for s in &parsed {
                    frames += render_anim(s, &pal, &opts).len();
                }
            }
            let dt = t0.elapsed().as_secs_f64();
            let t1 = Instant::now();
            for _ in 0..repeat.max(1) {
                library::par_map(&parsed, |s| render_anim(s, &pal, &opts).len());
            }
            let dp = t1.elapsed().as_secs_f64();
            let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
            println!(
                "{} sprites × {} → {} frames\n  1 core:  {:.2}s ({:.2} ms/frame)\n  {} cores: {:.2}s ({:.0} frames/s)",
                parsed.len(),
                repeat.max(1),
                frames,
                dt,
                1000.0 * dt / frames as f64,
                cores,
                dp,
                frames as f64 / dp
            );
        }
        Cmd::Mcp { library } => mcp::serve(library)?,
        Cmd::Serve { bind, library, tokens_file, hosts, cors } => {
            http::serve(http::ServeOpts { bind, library, tokens_file, hosts, cors })?
        }
    }
    Ok(())
}
