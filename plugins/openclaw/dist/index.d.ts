/**
 * NeuroStrata OpenClaw Plugin — Client Shim
 *
 * Phase 0 of the agent-adoption plan repaired the wire contract this shim
 * speaks:
 *
 * - The core dispatches on `tools/call` — never on a bare tool name — so
 *   every call goes through the real JSON-RPC method and the answer is
 *   unwrapped from `result.content[].text`.
 * - The daemon listens on `:34343` and requires the bearer token from
 *   `~/.config/neurostrata/daemon.token`; both are used on every request.
 * - `neurostrata_detect_task_intents` was a phantom: the core answers
 *   "Unknown tool". The call is gone and the lexical detector runs locally,
 *   mirroring `src/task/intent.rs`. Phase 0 writes nothing.
 * - The plugin context is captured inside `initialize`, so no handler
 *   reaches for a `ctx` that is not in scope.
 * - A degraded core is reported through `adapterStatus()` instead of being
 *   masked by a silent local fallback.
 */
export interface GuardValidateParams {
    action_type: string;
    payload: string;
    cwd: string;
    use_sandbox?: boolean;
}
export interface GuardValidateResult {
    verdict: "pass" | "reject";
    /** The core's own verdict bucket, kept so nothing is flattened away. */
    bucket: string;
    reasons: string[];
    constraints: string[];
    notes: string[];
    rule_ids: string[];
}
export interface TaskIntentSignal {
    marker: string;
    excerpt: string;
}
export interface TaskCreateParams {
    namespace: string;
    title: string;
    description?: string;
    priority?: number;
    labels?: string[];
    blocked_by?: string[];
    parent_id?: string;
}
export interface OpenClawPluginContext {
    config: Record<string, unknown>;
    logger: {
        info: (msg: string, meta?: Record<string, unknown>) => void;
        warn: (msg: string, meta?: Record<string, unknown>) => void;
        error: (msg: string, meta?: Record<string, unknown>) => void;
    };
    hooks: {
        beforeFileOp: (handler: FileOpHandler) => void;
        onTurnEnd: (handler: TurnEndHandler) => void;
    };
    conversation: {
        injectSystemMessage: (message: string) => void;
    };
}
export type FileOpHandler = (op: FileOperation) => Promise<FileOpResult>;
export type TurnEndHandler = (turn: ConversationTurn) => Promise<void>;
export interface FileOperation {
    type: "write" | "edit" | "delete" | "mkdir";
    path: string;
    content?: string;
    oldContent?: string;
    newContent?: string;
}
export interface FileOpResult {
    allowed: boolean;
    reason?: string;
    guardVerdict?: GuardValidateResult;
    /** True when the core could not answer and the shim is running degraded. */
    degraded?: boolean;
}
export interface ConversationTurn {
    role: "user" | "assistant" | "system";
    content: string;
}
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
/**
 * Merge the host config over the defaults.
 *
 * `guardEndpoint` is still honored: it is what older installs wrote, and
 * silently ignoring it would point the shim at the wrong port.
 */
export declare function resolveConfig(raw: unknown): PluginConfig;
/** The absolute path of the daemon token file. */
export declare function daemonTokenPath(tokenFile?: string | null): string;
/**
 * Read the daemon's bearer token.
 *
 * Returns `null` when there is no readable token: the daemon generates one
 * on first start, so an absent file means "the daemon has never run", not
 * "authentication is optional". The caller must report that as degraded
 * rather than pretend the request is anonymous.
 */
export declare function loadDaemonToken(tokenFile?: string | null): string | null;
/** The request headers, with the bearer attached when there is one. */
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
/** Build the `tools/call` request the core actually answers. */
export declare function buildToolCallRequest(id: number, name: string, args: unknown): JsonRpcRequest;
/** Build the `tools/list` request used for capability negotiation. */
export declare function buildToolsListRequest(id: number): JsonRpcRequest;
/**
 * Unwrap `result.content[].text`.
 *
 * The core answers a tool call with a content array, not with the typed
 * result a tool would return. An answer with no text is a broken answer:
 * it is reported, never treated as an empty success.
 */
export declare function extractToolText(result: unknown): string;
/** The tool names from a `tools/list` answer. */
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
    /** Call one tool and return its text content. */
    callTool(name: string, args: unknown): Promise<string>;
    /** The names of the tools the core exposes. */
    listToolNames(): Promise<string[]>;
    private post;
}
/**
 * Read the guard's answer.
 *
 * The core prefixes the body with `namespace:` and `trace_id:` lines, so the
 * JSON body is what lies between the first `{` and the last `}`. The verdict
 * is tagged with `bucket`, not `type`.
 */
export declare function parseGuardValidate(text: string): GuardValidateResult;
export type AdapterState = "uninitialized" | "ready" | "degraded" | "unavailable";
export interface AdapterStatus {
    state: AdapterState;
    endpoint: string;
    namespace: string;
    /** Whether a bearer token was found and attached. */
    authenticated: boolean;
    /** Tools this shim needs that the core does not expose. */
    missingTools: string[];
    lastError?: string;
    updatedAt: string;
}
/** Reset the tracked session/task state (test hook). */
export declare function resetState(): void;
/** The adapter's current state. A degraded core is visible here. */
export declare function adapterStatus(): AdapterStatus;
export declare const MARKERS: readonly [string, string][];
export declare const NEGATION_PREFIXES: string[];
export declare function detectTaskIntentsLocal(text: string): TaskIntentSignal[];
/** LRU cap: how many recently touched (`namespace:path`) keys are kept. */
export declare const PREFETCH_LRU_CAP = 128;
/** Output budget: one file-touch recall injects at most this many chars. */
export declare const PREFETCH_MAX_CHARS = 2048;
/**
 * Recently touched file paths, keyed `namespace:path`, in LRU order
 * (oldest first). Bounded so a long session cannot grow without bound.
 */
export declare const prefetchedPaths: Map<string, number>;
/** `(memory_id:valid_from)` keys already injected this session. */
export declare const injectedMemoryIds: Set<string>;
/** One governing memory as `memories_for_file` returns it. */
export interface FileMemoryHit {
    id: string;
    valid_from: string;
    memory_type: string;
    content: string;
}
/**
 * Read the `memories_for_file` answer: the array arrives as `hits` or
 * `memories` between the first `{` and the last `}` of the content text.
 */
export declare function parseFileMemoryHits(text: string): FileMemoryHit[];
/**
 * Recall governing memories when a file is touched (design §4.5).
 *
 * Each `namespace:path` key is prefetched at most once per LRU window;
 * each memory is injected at most once per `(id, valid_from)` key. Never
 * increments `access_count` — this is a read-only recall.
 */
export declare function prefetchMemoriesForFile(namespace: string, filePath: string): Promise<string[]>;
export declare const SUBAGENT_CONTEXT_MARKER = "### NeuroStrata Task & Graph Context";
export declare function buildSubagentContextHeader(namespace: string, taskId: string | null): string;
export declare function injectSubagentPrompt(target: Record<string, unknown>, namespace: string, taskId: string | null): boolean;
export declare function initialize(ctx: OpenClawPluginContext): Promise<void>;
//# sourceMappingURL=index.d.ts.map