# Sprite Factory — authoring guide

You describe WHAT to draw; the renderer owns HOW it looks (palette, light, outline, size).
Never hand-pick RGB colours or draw shading yourself — pick a material and let geometry + the
fixed top-left light produce the shading.

## Canvas & style rules
- Canvas is **32×32**, coordinates are floats, origin top-left, y grows DOWN.
- Keep everything inside **x,y ∈ [1.5, 30.5]** — the renderer adds a 1px outline around the silhouette.
- **Fill the canvas**: the subject should span ~26–29px on its longest axis. Small sprites look sad.
- Upright things (animals, people-ish, furniture, buildings, vehicles) rest on the bottom: lowest point ≈ y 29.5–30.
- Pick the most iconic, instantly-readable view for a 4–6 year old:
  side view for most animals & vehicles (facing **right**), front view where the face is the point (owl, cat).
- Faces (eyes) ONLY on animals. No faces on food or objects.
- Eyes: single `px` pixels (`"m":"black","shade":0`), usually 1×1 or 1×2. A white glint is optional.
- Minimum readable feature ≈ 1.5px wide. Thinner things vanish unless you give them `"weight": 3`
  or draw them as `px` pixels.
- Fewer, bigger shapes beat many tiny ones. Typical sprite: 4–14 primitives.

## Spec format (one JSON array per file, one object per sprite)
```json
{"name": "apple", "cat": "food", "anim": "bob", "tracks": [], "prims": [ ... ]}
```
- `name`: lowercase noun, snake_case (`ice_cream`, `teddy_bear`).
- `cat`: animals | food | home | nature | vehicles | places
- `anim`: preset (see below). `tracks`: optional extra per-part animation.
- `prims`: drawn in order — later prims are on top.

## Primitives
| t | required fields | notes |
|---|---|---|
| `ellipse` | `c:[x,y]`, `r: n` or `[rx,ry]` | dome-shaded by default |
| `rect` | `p:[x0,y0,x1,y1]`, opt `round` | |
| `poly` | `pts:[[x,y],...]`, opt `smooth: 1-3` | `smooth` rounds corners (Chaikin) — great for leaves, organic shapes |
| `arc` | `c`, `r`, `a0`, `a1` (deg, 0=right, 90=down), opt `pie:false` | pie slice / half-disc; `pie:false` = chord segment |
| `star` | `c`, `r`, opt `r2`, `n`, `a` | |
| `ngon` | `c`, `r`, `n`, opt `a` | |
| `line` | `pts:[[x,y],...]`, `w` | thick stroke with round caps; flat-shaded unless `"lit":true` |
| `px` | `pts:[[x,y],...]`, `m`, `shade` | exact pixels painted after shading (eyes, glints, tiny marks). `[12,12]` paints pixel x=12,y=12 |

## Common fields
- `m`: material (below). Default gray.
- `puff`: how rounded the shape is, in px. Default = half the shape's smaller side (fully domed/pillowy).
  Small values (1–2) = flat face with bevelled edges (walls, books, boxes). Large = round (fruit, bodies).
- `height`: multiplies dome steepness (default 1). <1 flatter shading, >1 stronger.
- `flat: true` + `shade: 0–4`: no lighting, single tone (windows, stripes, decals, patterns).
- `tone: -1 | +1`: darken/lighten the whole primitive one step (far-side legs = -1).
- `gloss: true`: bigger specular highlight (fruit, glass, metal, plastic).
- `edge: true`: draws a dark separating line where this prim overlaps EARLIER prims.
  Use when same-coloured parts overlap (head over body, overlapping leaves).
- `weight`: >1 makes thin details survive downsampling (wheel spokes, strings, whiskers).
- `rot`: rotate this prim (degrees, clockwise) about its own centre.
- `cut: true`: erases earlier prims inside this shape (donut hole, mug handle hole). Holes get a 1px outline
  on their inside edge, so make them ≥ 9px across or they turn into squares/dots.
- `clip: k`: only draw where earlier prim index `k` (0-based) is visible (stripes on a fish, spots on a ladybug).
- `g`: group name, for animation tracks.

Shades: 0 = outline/darkest, 1 = shadow, 2 = base, 3 = light, 4 = highlight.

## Materials
red, orange, yellow, gold, lime, green, forest, teal, sky, blue, navy, purple, pink,
brown, wood, tan, skin, cream, white, gray, steel, charcoal, black, glass

## Animation
Presets (`anim`): `none`, `idle` (breathing squash), `bob`, `float`, `hop`, `wobble`, `sway`, `pulse`, `spin`, `drive`.
Pick what suits the noun: animals → idle/hop, food → bob, vehicles → drive, plants → sway, toys → wobble/hop.

Extra `tracks` animate groups (`g`) — this is where personality comes from:
```json
"tracks": [
  {"g":"tail", "prop":"rot", "amp":12, "pivot":[22,26]},          // wag around tail base
  {"g":"wheel","prop":"rot", "amp":360,"wave":"saw","pivot":"each"},// each wheel spins on its own centre
  {"g":"wing", "prop":"sy",  "amp":0.4, "pivot":[16,14]}            // flap
]
```
`prop`: dx | dy | rot | sx | sy. `amp`: amount (px, degrees, or scale delta). `wave`: sin (default) | abs | abs2 | saw | square.
`phase`: 0–1 offset. `pivot`: center | bottom | top | [x,y] | each.

**`px` pixels are rigid stamps.** Touching px pixels in the same group form one stamp, which travels
with the animation by whole pixels but never stretches, squashes or turns, so eyes and little patterns
(hearts, sparkles) stay crisp in every frame. Two eyes are two stamps, so each follows its own spot on a
turning or squashing head. A heart and the glint drawn on it touch, so they move as one. Scaling or
rotating a px group moves its stamps around the pivot but doesn't resize or rotate them; draw the part
with shapes if it should grow or turn.

Timing (optional, top level):
- `frames`: frames per loop (default 8, at most 64). `ms`: ms per frame (default 110, at least 16).
- `hold`: per-frame multiplier of `ms`, e.g. `[3,1,1,1,3,1,1,1]` to linger on the poses. Missing entries
  are 1; ignored on a sprite with no animation.
- `snap: true`: rounds each part's total `dx`/`dy` to whole pixels, so motion is a clean pixel-art step
  instead of a sub-pixel re-draw. Pair it with `"wave":"square"` or a small `amp` for a crisp bounce.

`export_gif` turns a sprite into an animated GIF using the renderer's exact palette: transparent by
default, timed from `ms`/`hold`, with optional `bg`, `pingpong` and `repeat`.

## Review loop (do this — it's the whole point)
```bash
python3 factory.py sheet  nouns/FILE.json --out out/FILE_sheet.png --scale 6 --cols 5
python3 factory.py strips nouns/FILE.json --out out/FILE_strips.png --scale 3
```
Then `Read` the PNG and critique each sprite honestly:
1. Would a 5-year-old name it instantly at small size? (silhouette first, colour second)
2. Does it fill the canvas and sit on the floor if upright?
3. Any shapes lost to downsampling, mushy blobs, or same-colour parts merging (add `edge`)?
4. Does the animation read and loop cleanly?
Revise and re-render. Use `--only name1 name2` to iterate on a few quickly.

See `nouns/_test.json` for worked examples (apple, cat, house, car, tree, star).
