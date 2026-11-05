/**
 * @neurostrata/opencode-plugin — Thin harness adapter over @neurostrata/plugin-core.
 *
 * Re-exports the shared core implementation (compiled at ../core/dist/index.js)
 * and builds one OpenCode plugin via `createHarnessPlugin`. All behavior (Bugs 2-5,
 * Enhancements 1-3 & 5) lives in core; this file contains zero logic.
 */

export * from "../../core/dist/index.js";
import { createHarnessPlugin } from "../../core/dist/index.js";

export const plugin = createHarnessPlugin("neurostrata", "OpenCode");
export default plugin;
