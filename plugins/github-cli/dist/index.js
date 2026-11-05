/**
 * @neurostrata/github-cli-plugin — Thin harness adapter over @neurostrata/plugin-core.
 *
 * Re-exports the shared core implementation (compiled at ../../core/dist/index.js)
 * and builds one GitHub CLI plugin via `createHarnessPlugin`. All behavior lives
 * in core; this file contains zero logic.
 */
export * from "../../core/dist/index.js";
import { createHarnessPlugin } from "../../core/dist/index.js";
export const plugin = createHarnessPlugin("neurostrata-github-cli", "GitHub CLI");
export default plugin;
//# sourceMappingURL=index.js.map