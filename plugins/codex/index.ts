/**
 * @neurostrata/codex-plugin — Thin harness adapter over @neurostrata/plugin-core.
 *
 * Re-exports the shared core implementation (compiled at ../core/dist/index.js)
 * and builds one Codex plugin via `createHarnessPlugin`. All behavior lives in
 * core; this file contains zero logic.
 */

export * from "../core/dist/index.js";
import { createHarnessPlugin } from "../core/dist/index.js";

export const plugin = createHarnessPlugin("neurostrata-codex", "Codex");
export default plugin;
