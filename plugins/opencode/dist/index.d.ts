/**
 * @neurostrata/plugin-core — Shared ESM implementation for all NeuroStrata harness adapters.
 *
 * Zero `require()` calls and zero external npm runtime dependencies. Only Node built-ins
 * (node:fs, node:os, node:path) are imported. Every harness adapter (opencode, claude,
 * codex, github-cli) is a thin wrapper that re-exports this module and builds one plugin
 * via `createHarnessPlugin`.
 *
 * Implements:
 *  - Bug 2: findProjectRoot / discoverProjectNamespace / resolveConfig (never hardcodes "NeuroStrata")
 *  - Bug 3: canonical TOOL_CLASSIFICATION + is*Tool predicates + OpenCode V2 Code Mode inspector
 *  - Bug 4: handleHookEvent / handleClaudeHookEvent (PreToolUse guard + subagent + recall)
 *  - Bug 5: prefetchMemoriesForFile + formatFileTouchRecallBlock + recordLocalSessionQuery + execute.after recall delivery
 *  - Enhancement 1: resolveActiveTaskFromDaemon (daemon-backed active task/session state)
 *  - Enhancement 2 & 3: buildSubagentContextHeader / injectSubagentPrompt / extractRepoFilePaths / enrichAndInjectSubagentPrompt
 *  - Enhancement 5: shared core architecture across all harness plugins
 */
export declare const DEFAULT_ENDPOINT = "http://127.0.0.1:34343/mcp";
export declare const DEFAULT_TOKEN_FILE = "daemon.token";
export declare const REQUIRED_TOOLS: string[];
export interface PluginConfig {
    namespace: string;
    endpoint: string;
    bearerToken: string | null;
    tokenFile: string | null;
    strictMode: boolean;
    requestTimeoutMs: number;
    intentDetection: {
        enabled: boolean;
        injectReminder: boolean;
    };
}
/**
 * Resolve `namespace` in strict priority order:
 *  1. pluginConfig.namespace (non-empty trimmed string)
 *  2. process.env.NEUROSTRATA_NAMESPACE (non-empty trimmed string)
 *  3. discoverProjectNamespace(cwd)
 * Never falls back to a hardcoded "NeuroStrata".
 */
export declare function resolveConfig(pluginConfig?: unknown, cwd?: string): PluginConfig;
/**
 * Walk up from resolve(startDir) to the filesystem root looking for `.git`
 * (dir or file) or `.NeuroStrata`; return the directory containing it, or
 * resolve(startDir) if none found.
 */
export declare function findProjectRoot(startDir?: string): string;
/**
 * 1. Walk up to findProjectRoot(startDir) checking <dir>/.NeuroStrata/session_queries.json;
 *    if it parses as JSON with a non-empty string .namespace, return it trimmed.
 * 2. Otherwise return basename(findProjectRoot(startDir)) || "default".
 */
export declare function discoverProjectNamespace(startDir?: string): string;
export declare function daemonTokenPath(tokenFile?: string | null): string;
export declare function loadDaemonToken(tokenFile?: string | null): string | null;
export declare function buildHeaders(token: string | null): Record<string, string>;
export interface JsonRpcRequest {
    jsonrpc: "2.0";
    id: number;
    method: string;
    params?: unknown;
}
export interface JsonRpcResponse {
    jsonrpc?: string;
    id?: number | string | null;
    result?: unknown;
    error?: {
        code?: number;
        message?: string;
    };
}
export interface McpToolResult {
    content?: Array<{
        type?: string;
        text?: string;
    }>;
    isError?: boolean;
}
export declare function buildToolCallRequest(id: number, name: string, args: unknown): JsonRpcRequest;
export declare function buildToolsListRequest(id: number): JsonRpcRequest;
export declare function extractToolText(result: unknown): string;
export declare function extractToolNames(result: unknown): string[];
export declare class NeuroStrataMCPClient {
    private readonly endpoint;
    private readonly token;
    private readonly timeoutMs;
    private requestId;
    constructor(options: {
        endpoint: string;
        token: string | null;
        timeoutMs: number;
    });
    callTool(name: string, args: unknown): Promise<string>;
    listToolNames(): Promise<string[]>;
    private post;
}
export interface GuardValidateResult {
    verdict: "pass" | "reject";
    bucket: string;
    reasons: string[];
    constraints: string[];
    notes: string[];
    rule_ids: string[];
}
export declare function parseGuardValidate(text: string): GuardValidateResult;
export type AdapterState = "uninitialized" | "ready" | "degraded" | "unavailable";
export interface AdapterStatus {
    state: AdapterState;
    endpoint: string;
    namespace: string;
    authenticated: boolean;
    missingTools: string[];
    lastError?: string;
    updatedAt: string;
}
export declare function adapterStatus(): AdapterStatus;
export interface TaskIntentSignal {
    marker: string;
    excerpt: string;
}
export declare const MARKERS: readonly [string, string][];
export declare const NEGATION_PREFIXES: string[];
export declare function detectTaskIntentsLocal(text: string): TaskIntentSignal[];
export declare const READ_FILE_TOOLS: Set<string>;
export declare const MUTATING_FILE_TOOLS: Set<string>;
export declare const SHELL_TOOLS: Set<string>;
export declare const MUTATING_TOOLS: Set<string>;
export declare const SUBAGENT_TOOLS: Set<string>;
export declare const CODE_EXEC_TOOLS: Set<string>;
export declare const TOOL_CLASSIFICATION: {
    READ_FILE_TOOLS: Set<string>;
    MUTATING_FILE_TOOLS: Set<string>;
    SHELL_TOOLS: Set<string>;
    MUTATING_TOOLS: Set<string>;
    SUBAGENT_TOOLS: Set<string>;
    CODE_EXEC_TOOLS: Set<string>;
};
export declare function isFileTouchTool(name: string): boolean;
export declare function isMutatingTool(name: string): boolean;
export declare function isSubagentTool(name: string): boolean;
export declare function isCodeExecTool(name: string): boolean;
export interface CodeModeInspection {
    id?: string;
    sessionId?: string;
    note?: string;
    taskClaimed?: boolean;
    taskUpdated?: boolean;
    taskCompleted?: boolean;
    memoryMutated?: boolean;
    queriedFiles: string[];
}
/**
 * Inspect JS code strings passed to OpenCode V2 `execute` (Code Mode).
 * Detects neurostrata_task_claim / _update / _complete / _memories_for_file
 * calls in order and extracts id / session_id / note / path arguments from
 * each matched call's own argument slice.
 */
export declare function inspectCodeModeExecution(code: string): CodeModeInspection;
export declare const PREFETCH_LRU_CAP = 128;
export declare const PREFETCH_MAX_CHARS = 2048;
export declare const prefetchedPaths: Map<string, number>;
export declare const injectedMemoryIds: Set<string>;
export interface FileMemoryHit {
    id: string;
    valid_from: string;
    memory_type: string;
    content: string;
}
export declare function parseFileMemoryHits(text: string): FileMemoryHit[];
/**
 * Recall governing memories for a touched file. LRU path dedup (no re-query within the
 * window) and injectedMemoryIds dedup (each memory injected once per session).
 */
export declare function prefetchMemoriesForFile(namespace: string, filePath: string, opts?: {
    recordLedgerDir?: string;
}): Promise<string[]>;
/**
 * Mark a prefetch as delivered: its memories were handed to the caller, so the
 * dedup state stays and a later touch of the same file does not re-inject
 * them.
 */
export declare function confirmPrefetchDelivery(namespace: string, filePath: string): void;
/**
 * Roll back the dedup state of a prefetch whose recall was never delivered
 * (the guard denied the edit). The path is un-marked and the memory ids the
 * prefetch consumed are returned, so the allowed retry still sees the
 * governing rules.
 */
export declare function restorePrefetchDelivery(namespace: string, filePath: string): void;
export declare function formatFileTouchRecallBlock(filePath: string, snippets: string[]): string;
export declare const SUBAGENT_CONTEXT_MARKER = "### NeuroStrata Task & Graph Context";
export declare function buildSubagentContextHeader(namespace: string, taskId: string | null, extraContext?: string): string;
export declare function injectSubagentPrompt(target: Record<string, unknown>, namespace: string, taskId: string | null, extraContext?: string): boolean;
export declare function extractRepoFilePaths(promptText: string): string[];
export declare const cachedTaskInjection: Map<string, string | null>;
export declare const cachedSubagentPromptByTask: Map<string, string | null>;
/**
 * Resolve the active task from the daemon (fail-soft). Caches activeTaskId,
 * activeSessionId and cachedTaskInjection.
 */
export declare function resolveActiveTaskFromDaemon(namespace: string, preferredTaskId?: string | null): Promise<{
    taskId: string | null;
    sessionId: string | null;
    promptInjection: string | null;
}>;
/**
 * Prefetch governing memories for repo file paths mentioned in a subagent prompt and
 * inject the daemon's subagent_prompt_injection plus any recalled snippets into the
 * target prompt. Idempotent.
 */
export declare function enrichAndInjectSubagentPrompt(target: Record<string, unknown>, namespace: string, taskId?: string | null): Promise<boolean>;
export declare let activeSessionId: string | null;
export declare let activeTaskId: string | null;
export declare let mutationsSinceNote: number;
export declare function resetState(): void;
export declare function resetClientForTesting(): void;
export declare function getTrackingState(): {
    activeTaskId: string | null;
    activeSessionId: string | null;
    mutationsSinceNote: number;
};
export declare const pendingRecallBlocks: string[];
export declare const pendingRecallByCall: Map<string, string>;
export declare function initialize(ctx: any): Promise<void>;
export declare function handleHookEvent(event: Record<string, unknown>): Promise<Record<string, unknown>>;
export declare const handleClaudeHookEvent: typeof handleHookEvent;
export declare function onTurnEnd(turn: {
    role: "user" | "assistant";
    content: string;
}): Promise<void>;
export declare function fetchContextPlan(opts: {
    namespace: string;
    query?: string;
    file_path?: string;
    max_items?: number;
    max_bytes?: number;
    format?: "json" | "text";
}): Promise<string>;
export declare function createHarnessPlugin(pluginId?: string, harnessName?: string): {
    id: string;
    name: string;
    setup: typeof initialize;
    initialize: typeof initialize;
    handleHookEvent: typeof handleHookEvent;
    handleClaudeHookEvent: typeof handleHookEvent;
    onTurnEnd: typeof onTurnEnd;
    fetchContextPlan: typeof fetchContextPlan;
    adapterStatus: typeof adapterStatus;
};
export declare const plugin: ReturnType<typeof createHarnessPlugin>;
export default plugin;
