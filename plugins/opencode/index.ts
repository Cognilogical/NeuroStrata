/**
 * @neurostrata/opencode-plugin — Thin harness adapter over @neurostrata/plugin-core.
 *
 * Re-exports the shared @neurostrata/plugin-core implementation (bundled into dist/index.js)
 * and builds one OpenCode plugin via `createHarnessPlugin`. All behavior (Bugs 2-5,
 * Enhancements 1-3 & 5) lives in core; this file contains zero logic.
 */

export * from "@neurostrata/plugin-core";
import { createHarnessPlugin } from "@neurostrata/plugin-core";

export const plugin = createHarnessPlugin("neurostrata", "OpenCode");
export default plugin;
