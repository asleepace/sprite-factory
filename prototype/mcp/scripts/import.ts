/** Splits batch files (nouns/*.json) into the one-file-per-sprite library the MCP server uses. */
import { mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

const factory = resolve(process.argv[2] ?? "..");
const library = resolve(process.env.SPRITE_LIBRARY ?? join(factory, "library"));
mkdirSync(library, { recursive: true });

let n = 0;
for (const f of readdirSync(join(factory, "nouns")).filter((f) => f.endsWith(".json") && !f.startsWith("_"))) {
  const specs = JSON.parse(readFileSync(join(factory, "nouns", f), "utf8"));
  for (const s of Array.isArray(specs) ? specs : [specs]) {
    writeFileSync(join(library, `${s.name}.json`), JSON.stringify(s, null, 1) + "\n");
    n++;
  }
}
console.log(`imported ${n} sprites → ${library}`);
