#!/usr/bin/env node
/**
 * Sprite Factory MCP server.
 *
 * Gives any MCP-capable model a sprite pipeline it can drive like Blender MCP:
 * describe shapes → see the rendered sprite → revise → blind-test → export an atlas.
 * The house style (palette, light, outline, 32×32 canvas) lives in the renderer, so
 * every model that uses these tools produces sprites from the same set.
 */
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";
import { z } from "zod";
import { mkdir, readFile, readdir, writeFile, unlink } from "node:fs/promises";
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { RendererBridge } from "./bridge.js";

const HERE = dirname(fileURLToPath(import.meta.url));
const FACTORY_DIR = resolve(process.env.SPRITE_FACTORY_DIR ?? join(HERE, "..", ".."));
const LIBRARY_DIR = resolve(process.env.SPRITE_LIBRARY ?? join(FACTORY_DIR, "library"));
const EXPORT_DIR = join(LIBRARY_DIR, "_export");

const bridge = new RendererBridge(FACTORY_DIR);

// ───────────────────────────── schemas ─────────────────────────────
const MATERIALS = [
  "red", "orange", "yellow", "gold", "lime", "green", "forest", "teal", "sky", "blue", "navy", "purple",
  "pink", "brown", "wood", "tan", "skin", "cream", "white", "gray", "steel", "charcoal", "black", "glass",
] as const;
const CATEGORIES = ["animals", "food", "home", "nature", "vehicles", "places", "people", "misc"] as const;
const ANIMS = ["none", "idle", "bob", "float", "hop", "wobble", "sway", "pulse", "spin", "drive"] as const;

const Pt = z.tuple([z.number(), z.number()]);
const Prim = z
  .object({
    t: z.enum(["ellipse", "rect", "poly", "arc", "star", "ngon", "line", "px"]).describe("primitive type"),
    m: z.enum(MATERIALS).optional().describe("material (colour ramp). Never pick raw colours."),
    c: Pt.optional().describe("centre [x,y] for ellipse/arc/star/ngon"),
    r: z.union([z.number(), Pt]).optional().describe("radius, or [rx,ry]"),
    r2: z.number().optional(),
    n: z.number().int().optional(),
    a: z.number().optional(),
    a0: z.number().optional(),
    a1: z.number().optional(),
    pie: z.boolean().optional(),
    p: z.tuple([z.number(), z.number(), z.number(), z.number()]).optional().describe("rect [x0,y0,x1,y1]"),
    round: z.number().optional(),
    pts: z.array(Pt).optional().describe("points for poly/line/px"),
    w: z.number().optional().describe("line width"),
    smooth: z.number().int().min(0).max(4).optional(),
    puff: z.number().optional(),
    height: z.number().optional(),
    flat: z.boolean().optional(),
    shade: z.number().int().min(0).max(4).optional(),
    tone: z.number().int().min(-2).max(2).optional(),
    gloss: z.boolean().optional(),
    edge: z.boolean().optional(),
    weight: z.number().optional(),
    rot: z.number().optional(),
    cut: z.boolean().optional(),
    clip: z.number().int().optional(),
    lit: z.boolean().optional(),
    cap: z.boolean().optional(),
    g: z.string().optional().describe("animation group"),
  })
  .passthrough();
const Track = z.object({
  g: z.string().default("*"),
  prop: z.enum(["dx", "dy", "rot", "sx", "sy"]),
  amp: z.number(),
  wave: z.enum(["sin", "abs", "abs2", "saw", "square"]).optional(),
  phase: z.number().optional(),
  pivot: z.union([z.enum(["center", "bottom", "top", "each"]), Pt]).optional(),
});
const Name = z
  .string()
  .regex(/^[a-z][a-z0-9_]{0,40}$/, "lowercase snake_case noun, e.g. teddy_bear")
  .describe("lowercase snake_case noun");

type Spec = {
  name: string;
  cat: string;
  aka?: string[];
  anim?: string;
  ms?: number;
  tracks?: z.infer<typeof Track>[];
  prims: z.infer<typeof Prim>[];
};

// ───────────────────────────── library ─────────────────────────────
const specPath = (name: string) => join(LIBRARY_DIR, `${name}.json`);

async function loadSpec(name: string): Promise<Spec> {
  const p = specPath(name);
  if (!existsSync(p)) throw new Error(`No sprite named "${name}". Use list_sprites to see what exists.`);
  return JSON.parse(await readFile(p, "utf8"));
}

async function saveSpec(spec: Spec): Promise<void> {
  await mkdir(LIBRARY_DIR, { recursive: true });
  await writeFile(specPath(spec.name), JSON.stringify(spec, null, 1) + "\n");
}

async function allSpecs(): Promise<Spec[]> {
  if (!existsSync(LIBRARY_DIR)) return [];
  const files = (await readdir(LIBRARY_DIR)).filter((f) => f.endsWith(".json")).sort();
  return Promise.all(files.map(async (f) => JSON.parse(await readFile(join(LIBRARY_DIR, f), "utf8")) as Spec));
}

// ───────────────────────────── helpers ─────────────────────────────
const text = (t: string) => ({ type: "text" as const, text: t });
const image = (b64: string) => ({ type: "image" as const, data: b64, mimeType: "image/png" });
const fail = (msg: string): CallToolResult => ({ isError: true, content: [text(msg)] });

async function renderResult(spec: Spec, headline: string, scale = 8): Promise<CallToolResult> {
  const r = await bridge.call("render", { spec, scale });
  if (!r.ok) {
    return fail(`Not saved — the spec failed to render: ${r.error}${r.details ? "\n- " + (r.details as string[]).join("\n- ") : ""}`);
  }
  const lint = r.lint as { warnings: string[]; stats: Record<string, unknown> };
  const lines = [
    headline,
    `stats: ${JSON.stringify(lint.stats)}`,
    lint.warnings.length ? `style warnings:\n- ${lint.warnings.join("\n- ")}` : "style warnings: none",
    "Look at the image: would a 5-year-old name it instantly? Revise with edit_sprite if not.",
  ];
  const content: CallToolResult["content"] = [text(lines.join("\n")), image(r.still as string)];
  if (r.strip) content.push(text(`animation frames (${spec.anim ?? "none"}):`), image(r.strip as string));
  return { content };
}

const norm = (s: string) =>
  s.toLowerCase().replace(/[_-]/g, " ").replace(/[^a-z ]/g, "").trim().replace(/ies$/, "y").replace(/s$/, "");

// ───────────────────────────── server ─────────────────────────────
const server = new McpServer(
  { name: "sprite-factory", version: "0.1.0" },
  {
    instructions:
      "Sprite Factory makes 32×32 pixel-art sprites in one locked house style. Call style_guide once before " +
      "authoring. You describe shapes and materials; the renderer handles colour, light, outline and animation. " +
      "Always look at the returned image and revise. Before calling a sprite done, run blind_review on it.",
  },
);

server.registerTool(
  "style_guide",
  {
    title: "Style guide",
    description: "Returns the authoring guide (shape DSL, materials, animation, review checklist). Read before creating sprites.",
    inputSchema: {},
  },
  async () => {
    const guide = await readFile(join(FACTORY_DIR, "AUTHORING.md"), "utf8");
    const pal = await bridge.call("palette");
    const guideForMcp = guide.replace(/## Review loop[\s\S]*$/, "## Review loop\nEvery create/edit returns the rendered sprite and its frames. Critique, then edit_sprite. Finish with blind_review.\n");
    return { content: [text(guideForMcp), text(`materials (shade 0→4): ${JSON.stringify(pal.materials)}`)] };
  },
);

server.registerTool(
  "list_sprites",
  {
    title: "List sprites",
    description: "Lists sprites in the library, optionally filtered by category. Set sheet=true to also get a labelled contact sheet image.",
    inputSchema: {
      category: z.enum(CATEGORIES).optional(),
      sheet: z.boolean().optional().describe("include a contact-sheet image"),
    },
  },
  async ({ category, sheet }) => {
    let specs = await allSpecs();
    if (category) specs = specs.filter((s) => s.cat === category);
    const byCat = new Map<string, string[]>();
    for (const s of specs) byCat.set(s.cat, [...(byCat.get(s.cat) ?? []), s.name]);
    const summary = [...byCat].map(([c, ns]) => `${c} (${ns.length}): ${ns.join(", ")}`).join("\n") || "library is empty";
    const content: CallToolResult["content"] = [text(`${specs.length} sprites\n${summary}`)];
    if (sheet && specs.length) {
      const r = await bridge.call("sheet", { specs, scale: 2, cols: 12 });
      if (r.ok) content.push(image(r.image as string));
    }
    return { content };
  },
);

server.registerTool(
  "get_sprite",
  {
    title: "Get sprite spec",
    description: "Returns a sprite's full JSON spec (useful as a reference when making a related sprite).",
    inputSchema: { name: Name },
  },
  async ({ name }) => {
    try {
      return { content: [text(JSON.stringify(await loadSpec(name), null, 1))] };
    } catch (e) {
      return fail((e as Error).message);
    }
  },
);

server.registerTool(
  "create_sprite",
  {
    title: "Create sprite",
    description:
      "Creates a sprite from shapes, renders it and saves it to the library. Returns the rendered sprite, its " +
      "animation frames and style warnings. Coordinates are on a 32×32 canvas (y down); keep shapes inside 1.5–30.5.",
    inputSchema: {
      name: Name,
      category: z.enum(CATEGORIES),
      aka: z.array(z.string()).optional().describe("other words a child might use (e.g. bunny for rabbit); used by blind_review"),
      anim: z.enum(ANIMS).optional(),
      tracks: z.array(Track).optional(),
      prims: z.array(Prim).min(1).max(40),
      overwrite: z.boolean().optional().describe("replace an existing sprite with this name"),
    },
  },
  async ({ name, category, aka, anim, tracks, prims, overwrite }) => {
    if (existsSync(specPath(name)) && !overwrite) {
      return fail(`"${name}" already exists. Use edit_sprite, or pass overwrite: true to replace it.`);
    }
    const spec: Spec = { name, cat: category, ...(aka ? { aka } : {}), anim: anim ?? "none", tracks: tracks ?? [], prims };
    const res = await renderResult(spec, `Created "${name}".`);
    if (!res.isError) await saveSpec(spec);
    return res;
  },
);

server.registerTool(
  "edit_sprite",
  {
    title: "Edit sprite",
    description:
      "Changes an existing sprite and re-renders it. Use prim_ops for surgical edits (indices refer to the current " +
      "prims list, applied in order), or prims to replace the whole shape list.",
    inputSchema: {
      name: Name,
      anim: z.enum(ANIMS).optional(),
      tracks: z.array(Track).optional(),
      aka: z.array(z.string()).optional(),
      prims: z.array(Prim).min(1).max(40).optional().describe("replace all prims"),
      prim_ops: z
        .array(
          z.object({
            op: z.enum(["set", "merge", "insert", "delete"]),
            index: z.number().int().min(0),
            prim: Prim.partial().optional().describe("for set/insert: the full prim (t required); for merge: just the fields to change"),
          }),
        )
        .optional(),
    },
  },
  async ({ name, anim, tracks, aka, prims, prim_ops }) => {
    let spec: Spec;
    try {
      spec = await loadSpec(name);
    } catch (e) {
      return fail((e as Error).message);
    }
    const next: Spec = structuredClone(spec);
    if (anim) next.anim = anim;
    if (tracks) next.tracks = tracks;
    if (aka) next.aka = aka;
    if (prims) next.prims = prims;
    for (const op of prim_ops ?? []) {
      const n = next.prims.length;
      if (op.op === "insert") {
        if (op.index > n || !op.prim) return fail(`insert at ${op.index} needs index ≤ ${n} and a prim`);
        if (!op.prim.t) return fail("insert needs a prim with a type (t)");
        next.prims.splice(op.index, 0, op.prim as z.infer<typeof Prim>);
        continue;
      }
      if (op.index >= n) return fail(`prim index ${op.index} out of range (sprite has ${n} prims)`);
      if (op.op === "delete") next.prims.splice(op.index, 1);
      else if (op.op === "set") {
        if (!op.prim?.t) return fail("set needs a full prim with a type (t)");
        next.prims[op.index] = op.prim as z.infer<typeof Prim>;
      } else if (op.op === "merge") next.prims[op.index] = { ...next.prims[op.index], ...(op.prim ?? {}) };
    }
    const res = await renderResult(next, `Updated "${name}".`);
    if (!res.isError) await saveSpec(next);
    return res;
  },
);

server.registerTool(
  "render_sprite",
  {
    title: "Render sprite",
    description: "Renders an existing sprite without changing it.",
    inputSchema: { name: Name, scale: z.number().int().min(1).max(16).optional() },
  },
  async ({ name, scale }) => {
    try {
      return await renderResult(await loadSpec(name), `"${name}"`, scale ?? 8);
    } catch (e) {
      return fail((e as Error).message);
    }
  },
);

server.registerTool(
  "delete_sprite",
  {
    title: "Delete sprite",
    description: "Removes a sprite from the library. Cannot be undone.",
    inputSchema: { name: Name },
  },
  async ({ name }) => {
    if (!existsSync(specPath(name))) return fail(`No sprite named "${name}".`);
    await unlink(specPath(name));
    return { content: [text(`Deleted "${name}".`)] };
  },
);

server.registerTool(
  "blind_review",
  {
    title: "Blind review",
    description:
      "Tests whether sprites are recognisable without their names. Sends an unlabeled, shuffled sheet to a fresh " +
      "model via MCP sampling and grades its guesses. Without sampling support, returns the unlabeled sheet for a " +
      "separate reviewer.",
    inputSchema: { names: z.array(Name).min(1).max(30) },
  },
  async ({ names }) => {
    let specs: Spec[];
    try {
      specs = await Promise.all(names.map(loadSpec));
    } catch (e) {
      return fail((e as Error).message);
    }
    // shuffle so order gives nothing away
    const order = specs.map((_, i) => i).sort(() => Math.random() - 0.5);
    const shuffled = order.map((i) => specs[i]);
    const sheet = await bridge.call("sheet", { specs: shuffled, scale: 5, cols: 5, numbered: true });
    if (!sheet.ok) return fail(`Could not build review sheet: ${sheet.error}`);

    const caps = server.server.getClientCapabilities();
    if (!caps?.sampling) {
      return {
        content: [
          text(
            "This client does not support sampling, so the review cannot run here. Give the image below to a reviewer " +
              "that has NOT seen the sprite names and ask what a young child would call each numbered sprite.\nKey: " +
              shuffled.map((s, i) => `#${i + 1}=${s.name}`).join(", "),
          ),
          image(sheet.image as string),
        ],
      };
    }

    const prompt =
      `This image shows ${shuffled.length} numbered pixel-art sprites from a picture-vocabulary game for 4–6 year olds. ` +
      `For each, give the word a young child would most likely say (1–2 words), your confidence 0–100, and up to two ` +
      `things it could be mistaken for. Reply with ONLY a JSON array: [{"n":1,"guess":"...","conf":90,"alts":["..."]}]`;
    let guesses: { n: number; guess: string; conf: number; alts?: string[] }[];
    try {
      const out = await server.server.createMessage({
        messages: [{ role: "user", content: image(sheet.image as string) }, { role: "user", content: text(prompt) }],
        systemPrompt: "You are a strict blind reviewer. Judge only from the picture.",
        maxTokens: 1500,
        includeContext: "none",
      });
      const body = out.content.type === "text" ? out.content.text : "";
      guesses = JSON.parse(body.slice(body.indexOf("["), body.lastIndexOf("]") + 1));
    } catch (e) {
      return fail(`Sampling failed: ${(e as Error).message}`);
    }

    const rows = shuffled.map((s, i) => {
      const g = guesses.find((x) => x.n === i + 1);
      const ok = !!g && [s.name, ...(s.aka ?? [])].map(norm).includes(norm(g.guess));
      const verdict = !g ? "NO ANSWER" : ok ? (g.conf >= 80 ? "PASS" : "WEAK") : "FAIL";
      return { name: s.name, verdict, guess: g?.guess, conf: g?.conf, alts: g?.alts ?? [] };
    });
    const passed = rows.filter((r) => r.verdict === "PASS").length;
    const lines = rows.map(
      (r) => `${r.verdict.padEnd(5)} ${r.name}: read as "${r.guess}" (${r.conf}%)${r.alts.length ? `; could be ${r.alts.join(", ")}` : ""}`,
    );
    return {
      content: [
        text(
          `${passed}/${rows.length} passed.\n${lines.join("\n")}\n\n` +
            "Fix FAIL and WEAK sprites so they cannot be confused with the alternatives, then run blind_review again.",
        ),
      ],
    };
  },
);

server.registerTool(
  "export_atlas",
  {
    title: "Export atlas",
    description:
      "Packs sprites (all, a category, or named ones) into one PNG atlas plus a JSON frame map with animation " +
      "timings (Phaser/Pixi JSON-hash style). Writes files to the library's _export folder.",
    inputSchema: {
      names: z.array(Name).optional(),
      category: z.enum(CATEGORIES).optional(),
      scale: z.number().int().min(1).max(8).optional().describe("pixel scale of the exported atlas, default 1"),
      file: z.string().regex(/^[a-z0-9_-]{1,40}$/).optional().describe("output base name, default 'atlas'"),
    },
  },
  async ({ names, category, scale, file }) => {
    let specs = await allSpecs();
    if (names?.length) specs = specs.filter((s) => names.includes(s.name));
    if (category) specs = specs.filter((s) => s.cat === category);
    if (!specs.length) return fail("No sprites matched.");
    await mkdir(EXPORT_DIR, { recursive: true });
    const base = file ?? "atlas";
    const r = await bridge.call(
      "atlas",
      {
        specs,
        scale: scale ?? 1,
        png_path: join(EXPORT_DIR, `${base}.png`),
        json_path: join(EXPORT_DIR, `${base}.json`),
        png_name: `${base}.png`,
      },
      600_000,
    );
    if (!r.ok) return fail(`Export failed: ${r.error}`);
    return {
      content: [
        text(
          `Exported ${specs.length} sprites, ${r.frames} frames, ${r.width}×${r.height}px.\n` +
            `${join(EXPORT_DIR, base + ".png")}\n${join(EXPORT_DIR, base + ".json")}`,
        ),
        image(r.preview as string),
      ],
    };
  },
);

const transport = new StdioServerTransport();
await server.connect(transport);
process.on("SIGINT", () => (bridge.close(), process.exit(0)));
process.stdin.on("close", () => bridge.close());
