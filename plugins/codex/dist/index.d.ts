/**
 * @neurostrata/codex-plugin — Thin harness adapter over @neurostrata/plugin-core.
 *
 * Re-exports the shared core implementation (compiled at ../../core/dist/index.js)
 * and builds one Codex plugin via `createHarnessPlugin`. All behavior lives in
 * core; this file contains zero logic.
 */
export * from "../../core/dist/index.js";
export declare const plugin: {
    id: string;
    name: string;
    setup: typeof import("./index.js").initialize;
    initialize: typeof import("./index.js").initialize;
    handleHookEvent: typeof import("./index.js").handleHookEvent;
    handleClaudeHookEvent: typeof import("./index.js").handleHookEvent;
    onTurnEnd: typeof import("./index.js").onTurnEnd;
    fetchContextPlan: typeof import("./index.js").fetchContextPlan;
    adapterStatus: typeof import("./index.js").adapterStatus;
};
export default plugin;
//# sourceMappingURL=index.d.ts.map