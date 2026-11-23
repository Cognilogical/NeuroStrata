/**
 * NeuroStrata Hermes Agent Plugin — Client Shim
 *
 * Adapted from the OpenCode plugin for the Hermes Agent CLI.
 * Provides Zero-Action Start guard, file-touch recall, and subagent
 * prompt auto-injection without requiring @opencode/plugin import.
 */
/** Where the daemon serves MCP. The setup default is 34343, not 8080. */
export declare const DEFAULT_ENDPOINT = "http://127.0.0.1:34343/mcp";
/** The daemon's bearer token, as the daemon itself lays it down. */
export declare const DEFAULT_TOKEN_FILE = "daemon.token";
/** The tools this shim needs from the core to do its job. */
export declare const REQUIRED_TOOLS: string[];
export interface PluginConfig {
    namespace: string;
    /** The daemon's `/mcp` endpoint. */
    endpoint: string;
    /** The bearer token, when the host config carries one explicitly. */
    bearerToken: string | null;
    /** Where to read the daemon token from when the config carries none. */
    tokenFile: string | null;
    strictMode: boolean;
    /** Upper bound on one MCP round trip. */
    requestTimeoutMs: number;
    intentDetection: {
        enabled: boolean;
        injectReminder: boolean;
    };
}
export declare function resolveConfig(raw: unknown, cwd?: string): PluginConfig;
/** Which walk found a project root marker. */
export type RootMarkerVia = 'own' | 'main' | 'nogit';
export interface FoundRootMarker {
    /** The project root, in the start path's own checkout. */
    root: string;
    namespace: string;
    via: RootMarkerVia;
}
/**
 * A namespace value as written, or undefined when it is not one: empty, an
 * unexpanded variable, or anything but `[A-Za-z0-9_.:-]` starting with a
 * letter or digit. The Rust installer's `usable_namespace` rule.
 */
export declare function usableNamespace(raw: unknown): string | undefined;
/**
 * The marker that decides `startDir`'s project root, or null. `startDir`
 * must be absolute on this host; for a file the walk starts at its folder.
 *  1. Own checkout: from the start folder up to its checkout top, inclusive;
 *     the deepest marker wins.
 *  2. Linked worktree: the start folder mapped into the main checkout,
 *     walked from its nearest existing folder up to the main checkout's top,
 *     never entering a folder with its own `.git`. Steps 1 and 2 both run;
 *     the deeper marker wins, at equal depth the own checkout's.
 *  3. Outside git: the first marker above the start folder.
 * When the winning marker cannot be read the answer is null, with one
 * warning line naming the file: the caller falls back to the other namespace
 * sources, never to a marker further up.
 */
export declare function findRootMarker(startDir: string): FoundRootMarker | null;
/**
 * The namespace `harness`'s plugin config names for the project at
 * `projectDir`, read from its checkout's top, then (for a linked worktree,
 * whose untracked configs usually live only in the main checkout) the main
 * checkout's; outside git from `projectDir` itself. Empty when none names a
 * usable namespace.
 *
 * `runnerDir` is the folder of the hook runner the installer wrote,
 * `<root>/.NeuroStrata/plugins/<harness>/`. The session's checkout is read
 * FIRST, then the installer's root: a shared runner must not answer for
 * another project (#378 F5). A config that exists but cannot be read or
 * parsed is reported and fails closed, never falling through to env/folder
 * discovery (#378 F9).
 */
export declare function readHarnessConfig(harness: string, projectDir: string, runnerDir?: string): {
    namespace?: string;
};
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
    jsonrpc: '2.0';
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
    verdict: 'pass' | 'reject';
    bucket: string;
    reasons: string[];
    constraints: string[];
    notes: string[];
    rule_ids: string[];
}
export declare function parseGuardValidate(text: string): GuardValidateResult;
export type AdapterState = 'uninitialized' | 'ready' | 'degraded' | 'unavailable';
export interface AdapterStatus {
    state: AdapterState;
    endpoint: string;
    namespace: string;
    authenticated: boolean;
    missingTools: string[];
    lastError?: string;
    updatedAt: string;
}
export declare function resetState(): void;
export declare function adapterStatus(): AdapterStatus;
export interface TaskIntentSignal {
    marker: string;
    excerpt: string;
}
export declare const MARKERS: readonly [string, string][];
export declare const NEGATION_PREFIXES: string[];
export declare function detectTaskIntentsLocal(text: string): TaskIntentSignal[];
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
export declare function prefetchMemoriesForFile(namespace: string, filePath: string): Promise<string[]>;
export declare const SUBAGENT_CONTEXT_MARKER = "### NeuroStrata Task & Graph Context";
export declare function sanitizeSubagentHeaderToken(value: string): string;
export declare function buildSubagentContextHeader(namespace: string, taskId: string | null): string;
export declare function injectSubagentPrompt(target: Record<string, unknown>, namespace: string, taskId: string | null): boolean;
export declare function resetClientForTesting(): void;
/**
 * Whether `toolName` is one of SESSION_SCOPED_TOOLS, bare or as a host
 * spells an MCP tool (`mcp__<server>__neurostrata_task_claim`,
 * `<server>_neurostrata_task_claim`, `<server>.neurostrata_task_claim`).
 * A copy of core's `isSessionScopedTool`.
 */
export declare function isSessionScopedTool(toolName: string): boolean;
/**
 * Claim scope from a linked worktree (design 6.5): passes the session's
 * project folder as `worktree` on `neurostrata_task_claim` and
 * `neurostrata_get_snapshot` calls that carry no `worktree`, `project_root`
 * or `cwd`, which the daemon would otherwise scope to the namespace's
 * registry root (the main checkout when a marker applies). Changes `args`
 * in place; true when it did. A copy of core's `addSessionWorktree`.
 */
export declare function addSessionWorktree(toolName: string, args: Record<string, unknown>, projectDir: string | undefined): boolean;
export declare function initialize(ctx: any): Promise<void>;
export declare function onTurnEnd(turn: {
    role: 'user' | 'assistant';
    content: string;
}): Promise<void>;
/**
 * CLI hook runner entry point. The daemon installs hook-runner.mjs beside
 * dist/ and dispatches host hook events to this handler; a stateless CLI
 * invocation may arrive before any native initialize(ctx), so a default
 * client is built the same way core's handleHookEvent does. Mutating Hermes
 * tools run the same guard as the native execute.before hook, but a
 * rejection is returned as a deny decision instead of a thrown error.
 */
export declare function handleHookEvent(event: Record<string, unknown>): Promise<Record<string, unknown>>;
export declare function fetchContextPlan(opts: {
    namespace: string;
    query?: string;
    file_path?: string;
    max_items?: number;
    max_bytes?: number;
    format?: 'json' | 'text';
}): Promise<string>;
export declare const handleClaudeHookEvent: typeof handleHookEvent;
declare const _default: {
    id: string;
    setup: typeof initialize;
    initialize: typeof initialize;
    handleHookEvent: typeof handleHookEvent;
    handleClaudeHookEvent: typeof handleHookEvent;
};
export default _default;
//# sourceMappingURL=index.d.ts.map