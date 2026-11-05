/**
 * @neurostrata/github-cli-plugin — Thin harness adapter over @neurostrata/plugin-core.
 *
 * Re-exports the shared @neurostrata/plugin-core implementation (bundled into dist/index.js)
 * and builds one GitHub CLI plugin via `createHarnessPlugin`. All behavior lives
 * in core; this file contains zero logic.
 */

export * from "@neurostrata/plugin-core";
import { createHarnessPlugin } from "@neurostrata/plugin-core";

export const plugin = createHarnessPlugin("neurostrata-github-cli", "GitHub CLI");
export default plugin;
