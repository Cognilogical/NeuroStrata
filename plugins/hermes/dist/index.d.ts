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
export declare function resolveConfig(raw: unknown): PluginConfig;
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
export declare function buildSubagentContextHeader(namespace: string, taskId: string | null): string;
export declare function injectSubagentPrompt(target: Record<string, unknown>, namespace: string, taskId: string | null): boolean;
export declare function resetClientForTesting(): void;
export declare function initialize(ctx: any): Promise<void>;
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
declare const _default: {
    id: string;
    setup: typeof initialize;
    initialize: typeof initialize;
};
export default _default;
//# sourceMappingURL=index.d.ts.map