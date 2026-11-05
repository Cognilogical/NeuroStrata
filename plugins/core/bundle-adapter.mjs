#!/usr/bin/env node
/**
 * bundle-adapter.mjs — Inlines @neurostrata/plugin-core into each harness adapter's dist/.
 *
 * Usage (from adapter directory, e.g., plugins/opencode):
 *   node ../core/bundle-adapter.mjs
 *
 * Reads:
 *   ../core/dist/index.js
 *   ../core/dist/index.d.ts
 *   ./index.ts
 *
 * Writes:
 *   ./dist/index.js   (core JS + plugin export)
 *   ./dist/index.d.ts (core .d.ts + plugin declare)
 */

import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const rootDir = resolve(__dirname, "..");

function stripCoreFooter(content) {
  return content
    .split("\n")
    .filter(
      (line) =>
        !line.trim().startsWith("//# sourceMappingURL=") &&
        line.trim() !== "export default createHarnessPlugin();" &&
        !line.trim().startsWith("declare const _default:") &&
        line.trim() !== "export default _default;",
    )
    .join("\n");
}

function extractPluginLines(indexTsPath) {
  const content = readFileSync(indexTsPath, "utf8");
  const lines = content.split("\n");
  let inCoreImportSection = true;
  const pluginLines = [];

  for (const line of lines) {
    const trimmed = line.trim();
    // Skip lines that are core imports or comments before the plugin definition
    if (inCoreImportSection) {
      if (trimmed.startsWith("export * from \"@neurostrata/plugin-core\";")) {
        continue;
      }
      if (trimmed.startsWith("import { createHarnessPlugin } from \"@neurostrata/plugin-core\";")) {
        continue;
      }
      // Blank lines and comments before the plugin export are skipped
      if (trimmed === "" || trimmed.startsWith("/**") || trimmed.startsWith("*") || trimmed.startsWith("*/")) {
        continue;
      }
      // Once we hit the actual plugin export, we're past the import section
      if (trimmed.startsWith("export const plugin = createHarnessPlugin(") || trimmed.startsWith("export default plugin;")) {
        inCoreImportSection = false;
        pluginLines.push(line);
      }
    } else {
      pluginLines.push(line);
    }
  }

  return pluginLines.join("\n").trim();
}

function main() {
  // Determine adapter directory: process.argv[2] or process.cwd()
  const adapterDir = resolve(process.argv[2] || process.cwd());

  // Paths
  const coreDistJs = resolve(rootDir, "core/dist/index.js");
  const coreDistDts = resolve(rootDir, "core/dist/index.d.ts");
  const adapterIndexTs = join(adapterDir, "index.ts");
  const adapterDistDir = join(adapterDir, "dist");
  const adapterDistJs = join(adapterDistDir, "index.js");
  const adapterDistDts = join(adapterDistDir, "index.d.ts");

  // Read core files
  const coreJs = stripCoreFooter(readFileSync(coreDistJs, "utf8"));
  const coreDts = stripCoreFooter(readFileSync(coreDistDts, "utf8"));

  // Extract plugin lines from adapter's index.ts
  const pluginLines = extractPluginLines(adapterIndexTs);

  // Ensure dist directory exists
  mkdirSync(adapterDistDir, { recursive: true });

  // Write bundled index.js
  const bundledJs = `${coreJs}\n${pluginLines}\n`;
  writeFileSync(adapterDistJs, bundledJs, "utf8");
  console.log(`[bundle] wrote ${adapterDistJs}`);

  // Write bundled index.d.ts
  const pluginDeclare = `export declare const plugin: ReturnType<typeof createHarnessPlugin>;
export default plugin;`;
  const bundledDts = `${coreDts}\n${pluginDeclare}\n`;
  writeFileSync(adapterDistDts, bundledDts, "utf8");
  console.log(`[bundle] wrote ${adapterDistDts}`);
}

main();