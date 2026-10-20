/**
 * End-to-end test: spawns the MCP server over stdio and drives every tool like a model would.
 * Sampling is stubbed (no model API in CI): the stub saves the review sheet it was sent and
 * returns a scripted answer, which checks the plumbing and grading — not real recognition.
 */
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { CreateMessageRequestSchema, type CallToolResult } from "@modelcontextprotocol/sdk/types.js";
import { mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import assert from "node:assert/strict";

const OUT = resolve("test/out");
mkdirSync(OUT, { recursive: true });
const library = mkdtempSync(join(tmpdir(), "sprite-lib-"));

// SPRITE_SERVER="/path/to/sprite mcp" runs the same test against another implementation (e.g. the Rust binary)
const [cmd, ...cmdArgs] = (process.env.SPRITE_SERVER ?? "node dist/server.js").split(" ");
const transport = new StdioClientTransport({
  command: cmd,
  args: cmdArgs,
  env: { ...process.env, SPRITE_LIBRARY: library } as Record<string, string>,
  stderr: "inherit",
});
const client = new Client({ name: "e2e", version: "0.0.0" }, { capabilities: { sampling: {} } });

let sampledSheet = false;
client.setRequestHandler(CreateMessageRequestSchema, async (req) => {
  // content may be a single block or an array of blocks, depending on the server
  const blocks = req.params.messages.flatMap((m) => (Array.isArray(m.content) ? m.content : [m.content]));
  const img = blocks.find((b) => b.type === "image") as { data: string } | undefined;
  if (img) {
    writeFileSync(join(OUT, "blind_sheet.png"), Buffer.from(img.data, "base64"));
    sampledSheet = true;
  }
  // scripted reviewer: calls everything "teapot" except sprite #2
  return {
    role: "assistant",
    model: "stub",
    content: { type: "text", text: '[{"n":1,"guess":"teapot","conf":90,"alts":["kettle"]},{"n":2,"guess":"jar","conf":60,"alts":[]}]' },
  };
});

await client.connect(transport);
const call = async (name: string, args: Record<string, unknown> = {}) =>
  (await client.callTool({ name, arguments: args })) as CallToolResult;
const textOf = (r: CallToolResult) => r.content.filter((c) => c.type === "text").map((c) => (c as { text: string }).text).join("\n");
const saveImages = (r: CallToolResult, base: string) =>
  r.content
    .filter((c) => c.type === "image")
    .forEach((c, i) => writeFileSync(join(OUT, `${base}_${i}.png`), Buffer.from((c as { data: string }).data, "base64")));

const step = (s: string) => console.log(`\n▶ ${s}`);

step("tools/list");
const tools = (await client.listTools()).tools.map((t) => t.name);
console.log(tools.join(", "));
assert.deepEqual(
  [...tools].sort(),
  ["blind_review", "create_sprite", "delete_sprite", "edit_sprite", "export_atlas", "get_sprite", "list_sprites", "render_sprite", "style_guide"],
);

step("style_guide");
const guide = await call("style_guide");
assert.ok(textOf(guide).includes("Primitives") && textOf(guide).includes("materials"));
console.log(`${textOf(guide).length} chars`);

step("create_sprite teapot (authored through the tool, first try)");
const teapot = {
  name: "teapot",
  category: "home",
  aka: ["tea pot", "kettle"],
  anim: "wobble",
  tracks: [{ g: "steam", prop: "dy", amp: -2, wave: "saw" }],
  prims: [
    { t: "line", pts: [[24, 17], [27.5, 13], [29, 9.5]], w: 2.6, m: "teal", lit: true },
    { t: "line", pts: [[7, 13], [3.5, 15], [3.5, 21], [7, 24]], w: 2.2, m: "teal", lit: true },
    { t: "ellipse", c: [15.5, 20], r: [10.5, 9], m: "teal", gloss: true },
    { t: "rect", p: [6, 11.5, 25, 14], round: 1.2, m: "teal", tone: -1, puff: 1.2, edge: true },
    { t: "ellipse", c: [15.5, 10], r: [6, 3], m: "teal", edge: true },
    { t: "ellipse", c: [15.5, 6.5], r: 2, m: "gold", edge: true },
    { t: "ellipse", c: [15.5, 21], r: [3.5, 3], m: "white", flat: true, shade: 3 },
    { t: "px", pts: [[29, 7], [28, 5], [29, 3]], m: "white", shade: 2, g: "steam" },
  ],
};
let r = await call("create_sprite", teapot);
console.log(textOf(r));
assert.ok(!r.isError);
assert.equal(r.content.filter((c) => c.type === "image").length, 2, "still + frames");
saveImages(r, "teapot_v1");

step("create_sprite duplicate is refused");
r = await call("create_sprite", teapot);
assert.ok(r.isError && textOf(r).includes("already exists"));
console.log(textOf(r));

step("create_sprite with a bad spec is rejected and not saved");
r = await call("create_sprite", { name: "bad", category: "misc", prims: [{ t: "ellipse", c: [16, 16], r: 8, m: "red", clip: 3 }] });
assert.ok(r.isError);
console.log(textOf(r));
assert.ok(!textOf(await call("list_sprites")).includes("bad"));

step("edit_sprite: merge op (make the lid knob bigger, swap the decal to a heart-ish pink)");
r = await call("edit_sprite", {
  name: "teapot",
  prim_ops: [
    { op: "merge", index: 5, prim: { r: 2.4 } },
    { op: "merge", index: 6, prim: { m: "pink" } },
  ],
});
assert.ok(!r.isError);
console.log(textOf(r).split("\n").slice(0, 3).join("\n"));
saveImages(r, "teapot_v2");

step("edit_sprite: out-of-range index is an error");
r = await call("edit_sprite", { name: "teapot", prim_ops: [{ op: "delete", index: 99 }] });
assert.ok(r.isError);
console.log(textOf(r));

step("second sprite + get_sprite round trip");
r = await call("create_sprite", {
  name: "jar",
  category: "home",
  prims: [{ t: "rect", p: [8, 8, 24, 29], round: 4, m: "glass" }, { t: "rect", p: [9, 4, 23, 9], round: 1, m: "red", edge: true }],
});
assert.ok(!r.isError);
const got = JSON.parse(textOf(await call("get_sprite", { name: "jar" })));
assert.equal(got.prims.length, 2);

step("list_sprites with sheet");
r = await call("list_sprites", { sheet: true });
console.log(textOf(r));
saveImages(r, "list");

step("blind_review (sampling stubbed)");
r = await call("blind_review", { names: ["teapot", "jar"] });
console.log(textOf(r));
assert.ok(sampledSheet, "server sent the sheet over sampling");
assert.match(textOf(r), /\d\/2 passed/);

step("export_atlas");
r = await call("export_atlas", { file: "home", scale: 2 });
console.log(textOf(r));
assert.ok(!r.isError);
saveImages(r, "atlas");

step("delete_sprite");
r = await call("delete_sprite", { name: "jar" });
assert.ok(!r.isError);

await client.close();
console.log(`\n✓ all checks passed (library: ${library})`);
