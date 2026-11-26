/**
 * @neurostrata/plugin-core — Shared ESM implementation for all NeuroStrata harness adapters.
 *
 * Zero `require()` calls and zero external npm runtime dependencies. Only Node built-ins
 * (node:crypto, node:fs, node:os, node:path) are imported. Every harness adapter
 * (opencode, claude, codex, github-cli, openclaw, hermes) re-exports this module and
 * builds one plugin via `createHarnessPlugin`.
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
    /** Resolved project root directory (set in initialize from ctx.location / cwd). */
    projectRoot?: string;
}
/** Alias kept for readability in handler signatures (event, config: ResolvedConfig). */
export type ResolvedConfig = PluginConfig;
/** Allowlist namespace/task tokens before code-span interpolation (#316). */
export declare function sanitizeSubagentHeaderToken(value: string): string;
/**
 * Resolve `namespace` in strict priority order, the same order as
 * `neurostrata-mcp namespace resolve`:
 *  1. the project root marker (`.NeuroStrata/config.json`, findRootMarker(cwd))
 *  2. pluginConfig.namespace (the harness config; non-empty trimmed string)
 *  3. process.env.NEUROSTRATA_NAMESPACE (trimmed, one quote pair stripped and
 *     validated by `usableNamespace`, the CLI's `usable_env_namespace` rule)
 *  4. discoverProjectNamespace(cwd)
 * Never falls back to a hardcoded "NeuroStrata".
 */
export declare function resolveConfig(pluginConfig?: unknown, cwd?: string): PluginConfig;
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
 * Walk up from resolve(startDir) looking for `.git` (dir or file); return the
 * directory containing it. With no `.git` above (pass 2), the nearest folder
 * holding a valid project marker (`.NeuroStrata/config.json`), else the
 * nearest folder holding `.NeuroStrata` other than the home folder, else
 * resolve(startDir).
 */
export declare function findProjectRoot(startDir?: string, home?: string): string;
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
 * The project folder a CLI hook runner resolves its namespace from. Claude
 * Code starts a new hook process per event, and the event's `cwd` follows
 * every `cd`, so for Claude this is the session's project folder:
 * `CLAUDE_PROJECT_DIR`, which Claude Code exports to hook processes as the
 * project root where the session started. Without it, the folder recorded
 * for this session at SessionStart (`claude_session_dirs` in
 * `.NeuroStrata/active_session.json`; never the home folder's), else the
 * event's `cwd`. Other harnesses use the event's `cwd`, else the process's.
 *
 * SessionStart also fires on resume, clear and compact, when the event's
 * `cwd` may have moved: a folder already recorded for the session is kept.
 * The file is replaced whole (temp + rename), and never when it exists but
 * cannot be parsed, so the other fields it holds are not lost.
 */
export declare function resolveHookProjectDir(harness: string, event?: Record<string, unknown>, env?: Record<string, string | undefined>, home?: string): string;
/**
 * Whether `toolName` is one of [SESSION_SCOPED_TOOLS], bare or as a harness
 * spells an MCP tool (`mcp__<server>__neurostrata_task_claim`,
 * `<server>_neurostrata_task_claim`, `<server>.neurostrata_task_claim`).
 */
export declare function isSessionScopedTool(toolName: string): boolean;
/**
 * Claim scope from a linked worktree (design 6.5): `neurostrata_task_claim`
 * and `neurostrata_get_snapshot` without `worktree`, `project_root` or `cwd`
 * are scoped by the daemon to the namespace's registry root, the main
 * checkout when a marker applies. This passes the session's project folder
 * as `worktree` instead. Changes `args` in place; true when it did.
 */
export declare function addSessionWorktree(toolName: string, args: Record<string, unknown>, projectDir: string | undefined): boolean;
/**
 * 1. Walk up to findProjectRoot(startDir) checking <dir>/.NeuroStrata/session_queries.json;
 *    if it parses as JSON with a non-empty string .namespace, return it trimmed.
 * 2. Otherwise return basename(findProjectRoot(startDir)) || "default".
 */
export declare function discoverProjectNamespace(startDir?: string): string;
/**
 * Resolve the project root from an OpenCode V2 host context, falling back
 * through ctx fields to process.cwd(). Priority:
 *   1. ctx.location.project.canonical
 *   2. ctx.location.project.root
 *   3. ctx.location.directory
 *   4. ctx.projectDir
 *   5. ctx.directory
 *   6. ctx.cwd
 *   7. process.cwd()
 */
export declare function resolveProjectRoot(ctx?: any): string;
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
    /** Daemon memory advisory captured on the last errored tool call, drained by
     *  the plugin's execute.after handler so the agent sees it as a suffix line
     *  rather than an opaque throw. Cleared on every callTool. */
    private pendingDaemonAdvisory;
    constructor(options: {
        endpoint: string;
        token: string | null;
        timeoutMs: number;
    });
    /** Drain the advisory captured by the last errored callTool, if any. */
    flushPendingAdvisory(): string | null;
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
export declare function adapterStatus(): AdapterStatus;
export interface TaskIntentSignal {
    marker: string;
    excerpt: string;
}
export declare const MARKERS: readonly [string, string][];
export declare const NEGATION_PREFIXES: string[];
export declare function detectTaskIntentsLocal(text: string): TaskIntentSignal[];
export interface ProvenanceSpan {
    turn_id?: string;
    role?: string;
    source_tool?: string;
    channel?: string;
    evidence_quote?: string;
    quote?: string;
    char_range?: [number, number];
    parent_task_id?: string;
    confidence?: number;
}
export interface Turn {
    role: string;
    content: string;
    turn_id?: string;
    source_tool?: string;
    channel?: string;
    parent_task_id?: string;
}
export interface TurnCandidate {
    kind: 'task_obligation' | 'memory_candidate';
    category?: 'fact' | 'decision' | 'rule_candidate';
    title: string;
    description?: string;
    confidence: number;
    auto_commit: boolean;
    provenance: ProvenanceSpan;
}
export interface ConceptualBridge {
    source_id?: string;
    via_id?: string;
    target_id?: string;
    target_domain?: string;
    target_type?: string;
    target_summary?: string;
    path?: string;
}
/**
 * Extract task-obligation candidates from a turn's content. Detects local
 * task-intent markers (TODO:, FIXME:, follow-up:, ...) via
 * `detectTaskIntentsLocal` and attaches a provenance span
 * (turn_id, role, source_tool, channel, evidence_quote, quote, char_range,
 * confidence) to each. Pure: no daemon interaction. Rule candidates are
 * never produced here (rule candidates never auto-promote).
 */
export declare function extractTurnCandidates(turn: Turn): TurnCandidate[];
/**
 * Post turn candidates fail-soft to the intake ledger via
 * `neurostrata_task_candidates` (`action: "record"`). Never throws: a missing
 * MCP client or a daemon error is swallowed so a turn is never blocked by
 * intake-ledger submission.
 */
export declare function postTurnCandidates(candidates: TurnCandidate[], namespace: string): Promise<void>;
/**
 * Extract de-duplicated conceptual bridges from `memories_for_file` hits.
 * Each hit may carry a `conceptual_bridges` array (source -> via -> target
 * 2-hop cross-domain paths). Bridges are de-duplicated by `path`.
 */
export declare function extractConceptualBridges(hits: unknown[]): ConceptualBridge[];
/**
 * Persist conceptual bridges into `.NeuroStrata/active_session.json` under
 * the `conceptual_bridges` key, merging with any existing fields so the
 * `sessionContextHandler` can inject them as `[NeuroStrata Conceptual Bridges]`.
 */
export declare function persistConceptualBridges(projectRoot: string, bridges: ConceptualBridge[]): void;
export declare const WORKING_SET_MAX_FRAME_BYTES = 4096;
export declare const WORKING_SET_MAX_IDLE_TURNS = 3;
export declare const WORKING_SET_STUB_MAX_BYTES = 192;
export declare const WORKING_SET_MAX_ROOTS = 32;
export declare const WORKING_SET_MAX_YOUNG_GEN = 64;
export declare const WORKING_SET_MAX_STUBS = 128;
export declare const WORKING_SET_MAX_TOOL_STUBS = 128;
export declare const WORKING_SET_MAX_TOOL_DIGESTS = 128;
export declare const WORKING_SET_MAX_EVENT_IDS = 128;
export declare const WORKING_SET_MAX_INJECTION_KEYS = 128;
export declare const WORKING_SET_MAX_ACTIVE_SESSION_BYTES = 1048576;
export declare const TOOL_GC_INLINE_THRESHOLD_BYTES = 3072;
export declare const TOOL_GC_HEAD_BYTES = 512;
export declare const TOOL_GC_TAIL_BYTES = 512;
export declare const TOOL_GC_STUB_MAX_BYTES = 512;
export declare const WORKING_SET_SUMMARY_MAX_BYTES = 512;
export declare const WORKING_SET_FRAME_OPEN = "[NeuroStrata Active Context]";
export declare const WORKING_SET_FRAME_CLOSE = "[NeuroStrata End Active Context]";
export declare const WORKING_SET_TIER0_MAX_BYTES = 2048;
export declare const WORKING_SET_MAX_EDGES = 128;
export declare const WORKING_SET_STRIDE_MESSAGES = 8;
export declare const WORKING_SET_HOT_TAIL_MESSAGES = 12;
export declare const WORKING_SET_TIER0_OPEN = "[NeuroStrata Golden Rules \u2014 IMMORTAL PREFIX]";
export declare const WORKING_SET_TIER0_CLOSE = "[NeuroStrata End Immortal Prefix]";
export declare const WORKING_SET_STRIDE_MARKER = "[NeuroStrata Quantized History Stride ";
export type WorkingSetNodeKind = 'task_header' | 'golden_rule' | 'file' | 'symbol' | 'memory' | 'ast_summary' | 'conceptual_bridge' | 'recall_text';
export type WorkingSetTier = 'Immortal' | 'PinnedRoot' | 'YoungGen' | 'EvictedStub';
export type WorkingSetGcAction = 'retain_full' | 'promote_to_young_gen' | 'demote_to_stub' | 'retain_stub';
export type WorkingSetCacheTier = 'Tier0SystemPrefix' | 'Tier1QuantizedHistory' | 'Tier2TailDelta' | 'EvictedHeapStub';
export type WorkingSetCacheTierTag = 'tier0_system_prefix' | 'tier1_quantized_history' | 'tier2_tail_delta' | 'evicted_heap_stub';
export type WorkingSetStrideAction = 'retain_raw_message' | 'fold_into_quantized_stub' | 'invalid_message_index';
export type WorkingSetEdgeKind = 'GOVERNS' | 'DEPENDS_ON' | 'CALLS' | 'IMPORTS' | 'CONCEPTUAL_BRIDGE' | 'EXTRACTED_FROM';
export interface WorkingSetEdge {
    from: string;
    to: string;
    kind: WorkingSetEdgeKind;
    weight_bps: number;
    last_touched_turn: number;
}
export interface WorkingSetStrideInput {
    total_messages: number;
    message_index: number;
    hot_tail_count: number;
    stride: number;
    is_protected_system_or_pinned: boolean;
}
export interface WorkingSetScope {
    projectRoot: string;
    projectId: string;
    namespace: string;
    sessionId: string;
    epoch: number;
}
export interface WorkingSetTouch {
    id: string;
    kind: Exclude<WorkingSetNodeKind, 'recall_text'>;
    path?: string;
    symbol?: string;
    memoryId?: string;
}
export interface WorkingSetExpansionRequest {
    id: string;
    expandTool: string;
    input: Record<string, unknown>;
}
export interface WorkingSetNode {
    id: string;
    kind: WorkingSetNodeKind;
    label: string;
    summary: string;
    governing_ids: string[];
    byte_cost: number;
    last_touched_turn: number;
    expand_tool: string;
    path?: string;
    symbol?: string;
    memory_type?: string;
}
export interface WorkingSetStub {
    id: string;
    kind: WorkingSetNodeKind;
    pointer: string;
    byte_cost: number;
    last_touched_turn: number;
    governing_ids: string[];
    path?: string;
    memory_type?: string;
    sha256?: string;
}
export interface ToolOutputStub {
    id: string;
    tool_name: string;
    rel_path: string;
    sha256: string;
    bytes: number;
    epoch: number;
    last_touched_turn: number;
    governing_ids: string[];
    pointer: string;
    byte_cost: number;
}
export interface ToolOutputDigest {
    id: string;
    project_id: string;
    path_key: string;
    tool_name: string;
    sha256: string;
    bytes: number;
    epoch: number;
    last_touched_turn: number;
    read_count: number;
}
export interface WorkingSetState {
    version: 1;
    project_id: string;
    namespace: string;
    session_id: string;
    epoch: number;
    turn_seq: number;
    active_turn_id: string | null;
    recent_event_ids: string[];
    max_frame_bytes: 4096;
    max_idle_turns: number;
    used_frame_bytes: number;
    frame_fingerprint: string;
    last_injected_fingerprint: string;
    roots: WorkingSetNode[];
    young_gen: WorkingSetNode[];
    evicted_stubs: WorkingSetStub[];
    edges: WorkingSetEdge[];
    tool_output_stubs: ToolOutputStub[];
    tool_output_digests: ToolOutputDigest[];
}
export interface ActiveSessionState {
    namespace?: string;
    session_id?: string;
    task_id?: string;
    status?: string;
    golden_rules?: string[];
    conceptual_bridges?: unknown[];
    subagent_prompt_injection?: string;
    claude_session_dirs?: Record<string, string>;
    working_set?: WorkingSetState;
    [key: string]: unknown;
}
export type WorkingSetExpansion = (request: WorkingSetExpansionRequest) => Promise<WorkingSetNode | WorkingSetStub | null>;
/** Mirrors the first-match Rust/Lean working-set decision table exactly. */
export declare function evaluateWorkingSetEviction(input: {
    tier: WorkingSetTier;
    turn_age: number;
    max_idle_turns: number;
    node_bytes: number;
    projected_frame_bytes: number;
    max_frame_bytes: number;
    is_re_referenced: boolean;
}): WorkingSetGcAction;
/** Rust `WorkingSetCacheTier::as_tag` mirror. */
export declare function workingSetCacheTierAsTag(tier: WorkingSetCacheTier): WorkingSetCacheTierTag;
/** Mirrors `evaluate_working_set_cache_tier` exactly. Tier 1 is state-only. */
export declare function evaluateWorkingSetCacheTier(tier: WorkingSetTier, hasSystemChannel: boolean): WorkingSetCacheTier;
/** Mirrors `compute_working_set_quantized_frontier` exactly. */
export declare function computeWorkingSetQuantizedFrontier(totalMessages: number, hotTailCount: number, stride: number): number;
/** Mirrors `evaluate_working_set_stride_fold` exactly (first match wins). */
export declare function evaluateWorkingSetStrideFold(input: WorkingSetStrideInput): WorkingSetStrideAction;
/** Load and strictly validate the reconstructible local L1 cache. */
export declare function loadWorkingSet(scopeInput: WorkingSetScope): Promise<WorkingSetState>;
/** Merge only working_set into a valid parent session file, best effort. */
export declare function persistWorkingSet(scopeInput: WorkingSetScope, stateInput: WorkingSetState): Promise<void>;
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
/** Apply TTL and deterministic pressure demotion without changing immortal nodes. */
export declare function sweepWorkingSet(stateInput: WorkingSetState): WorkingSetState;
/** Record typed graph edges with deterministic dedup, ordering, and cap. */
export declare function recordWorkingSetEdges(stateInput: WorkingSetState, edges: Array<Partial<WorkingSetEdge> & Pick<WorkingSetEdge, 'from' | 'to' | 'kind'>>): WorkingSetState;
/** Advance one deterministic turn, mark touches, and retry fitting page faults. */
export declare function advanceWorkingSetTurn(stateInput: WorkingSetState, namespace: string, sessionId: string, projectRoot: string, touched: WorkingSetTouch[], expand: WorkingSetExpansion): Promise<WorkingSetState>;
/** Pure size-based dehydration; duplicate history belongs to the scoped helper. */
export declare function dehydrateToolOutput(rawText: string, toolName: string, filePath: string | null, governingIds: string[]): {
    text: string;
    dehydrated: boolean;
    digest?: ToolOutputDigest;
};
/** Scoped duplicate-aware read dehydration with digest/stub pair persistence. */
export declare function dehydrateRepeatedReadOutput(scopeInput: WorkingSetScope, rawText: string, toolName: string, filePath: string | null, governingIds: string[]): Promise<{
    text: string;
    dehydrated: boolean;
    digest?: ToolOutputDigest;
}>;
/** Render one bounded, marker-safe frame; state nodes are never demoted here. */
export declare function renderWorkingSetFrame(state: WorkingSetState, activeSession: Record<string, unknown>): {
    text: string;
    usedBytes: number;
    fingerprint: string;
};
/** Tier 0: immortal Golden Rules only; zero volatile counters or task fields. */
export declare function renderWorkingSetTier0Prefix(state: WorkingSetState, activeSession?: ActiveSessionState | null): {
    text: string;
    usedBytes: number;
    fingerprint: string;
};
/** Tier 2: volatile task/roots/young/edges/stubs; never invalidates tier 0. */
export declare function renderWorkingSetDeltaFrame(state: WorkingSetState, activeSession?: ActiveSessionState | null, options?: {
    omitImmortalRules?: boolean;
    maxBytes?: number;
}): {
    text: string;
    usedBytes: number;
    fingerprint: string;
};
/** Deterministic compaction checkpoint; replaces a lossy host LLM summary. */
export declare function renderWorkingSetCompactionSummary(state: WorkingSetState, activeSession?: ActiveSessionState | null): string;
/** Tier 1: fold pre-frontier history into stride-stable deterministic stubs. */
export declare function projectWorkingSetMessages(messages: unknown[], state: WorkingSetState, options?: {
    hotTailCount?: number;
    stride?: number;
}): {
    messages: unknown[];
    quantizedFrontier: number;
    foldedCount: number;
};
/** Replace or append the immutable-prefix system part in place. */
export declare function upsertWorkingSetSystemPrefix(system: unknown[], tier0Text: string): boolean;
/** Attach or update the volatile delta on the tail without touching prefix. */
export declare function upsertWorkingSetMessageTailDelta(messages: unknown[], deltaText: string): boolean;
/** Upsert one mutable frame using context[], content, then additionalContext. */
export declare function upsertWorkingSetFrame(sink: unknown, frame: string): boolean;
export declare function wsSyncBridges(state: WorkingSetState, parent: Record<string, unknown>): void;
/** Observe or advance one native turn, deduplicating native event/turn identity. */
export declare function runWorkingSetTurnBoundary(scopeInput: WorkingSetScope, eventId?: string, turnId?: string): Promise<{
    frame: string;
    fingerprint: string;
    changed: boolean;
    duplicateEvent?: boolean;
}>;
/** Render current state without advancing turns or marking touches. */
export declare function runWorkingSetContextCycle(scopeInput: WorkingSetScope, eventId?: string): Promise<{
    frame: string;
    fingerprint: string;
    changed: boolean;
}>;
/** Pre-turn activation: mark anchors, spread edges, and persist L1 state. */
export declare function runWorkingSetPromptCycle(input: {
    scope: WorkingSetScope;
    promptText: string;
    turnId?: string;
    eventId?: string;
}): Promise<{
    frame: string;
    fingerprint: string;
    changed: boolean;
    activatedIds: string[];
}>;
/** Mark deterministic file/symbol/memory touches without advancing the turn. */
export declare function markWorkingSetTouches(scopeInput: WorkingSetScope, touched: WorkingSetTouch[], eventId?: string): Promise<void>;
/** Shared tool cycle: observe touches, dehydrate text, and return one frame. */
export declare function runWorkingSetToolCycle(input: {
    scope: WorkingSetScope;
    turnId?: string;
    eventId?: string;
    toolName: string;
    toolInput: unknown;
    rawText: string | null;
    outputIsText: boolean;
    isError: boolean;
    advanceTurn: boolean;
}): Promise<{
    replacementText: string | null;
    frame: string;
    fingerprint: string;
    dehydrated: boolean;
    changed: boolean;
}>;
export declare function isReadOnlyInspectionTool(name: string): boolean;
export declare function isSubagentTool(name: string): boolean;
export declare function isCodeExecTool(name: string): boolean;
/**
 * True only for shell tools whose payload is provably read-only/verification.
 * JSON payloads are unwrapped via `command`/`cmd`; substitutions, redirection
 * and chains containing any mutating segment fail closed.
 */
export declare function isZeroActionExemptShellPayload(actionType: string, payloadOrCommand: string): boolean;
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
    projectRoot?: string;
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
export declare function isReadOnlySubagentAgent(agentId?: string | null): boolean;
/** Resolve the subagent agent id from the fields harnesses use to name it. */
export declare function resolveSubagentAgentId(target: Record<string, unknown>): string | undefined;
/** Code-Mode harnesses (OpenCode V2) expose MCP via `execute` + tools.neurostrata.*. */
export declare function detectCodeMode(ctx?: any, cfg?: PluginConfig): boolean;
export interface SubagentHeaderOptions {
    agentId?: string | null;
    codeMode?: boolean;
}
/**
 * Render a value inside a markdown backtick code span without allowing a
 * breakout: backticks close the span and newlines end its line, so both are
 * collapsed to single spaces. The namespace is not privileged input — it can
 * arrive from a committed `.NeuroStrata/session_queries.json`, plugin config,
 * or an environment variable — so every header interpolation goes through
 * here (issue #316).
 */
export declare function markdownCodeSpanValue(value: unknown): string;
/**
 * Public header builder: sanitizes the two caller-supplied values before
 * rendering (issue #316).
 */
export declare function buildSubagentContextHeader(namespace: string, taskId: string | null, extraContext?: string, options?: SubagentHeaderOptions): string;
export declare function injectSubagentPrompt(target: Record<string, unknown>, namespace: string, taskId: string | null, extraContext?: string, options?: SubagentHeaderOptions): boolean;
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
/** Structured marker the daemon writes on its error/diagnostic channel when
 *  it reports buffer-pool / RSS pressure. Memory payloads and tool-result
 *  rows never carry it, so matching it can never misfire on user content. */
export declare const DAEMON_MEMORY_ADVICE_MARKER = "NEUROSTRATA_ADVICE[daemon_memory]";
export declare function resetAdvisoryStateForTesting(): void;
/** Drain the live client's pending advisory (if any). Returns-and-clears. */
export declare function flushPendingDaemonAdvisory(): string | null;
/** Pull the daemon memory advisory out of the error/diagnostic channel.
 * Only text carrying the marker is inspected — memory payloads and result
 * rows are never matched, so a memory that merely discusses
 * `NEUROSTRATA_BUFFER_POOL_MB` cannot synthesize an advisory. Returns the
 * guidance string once per 10-minute window, or `null` when already shown
 * or absent. */
export declare function extractDaemonMemoryAdvisory(errorChannelText: string | null | undefined, nowMs?: number): string | null;
export declare function wsRecallMemoryIds(block: string): string[];
export interface SessionEpochState {
    epoch: number;
    baselineInjectedEpoch: number;
    seenFiles: Set<string>;
}
/** Current epoch for a session (starts at 0, incremented by compaction). */
export declare function currentSessionEpoch(sessionId: string | null | undefined): number;
/** Increment the epoch and clear seen files so the next context hook re-injects. */
export declare function incrementSessionEpoch(sessionId: string | null | undefined): void;
export declare function sessionSeenFiles(sessionId: string | null | undefined): Set<string>;
export declare function resetSessionEpoch(sessionId: string | null | undefined): void;
export declare function resetAllSessionEpochs(): void;
/**
 * `session.hook("context")` handler. Reads `.NeuroStrata/active_session.json`
 * from the project root; when an active task exists and its baseline has not
 * been injected this epoch, injects `[NeuroStrata Active Context] ...` once.
 * Same-epoch repeat calls leave the event untouched.
 */
export declare function sessionContextHandler(event: any, config: ResolvedConfig): Promise<void>;
/** `session.hook("prompt")` handler: activate anchors before the model call. */
export declare function sessionPromptHandler(event: any, config: ResolvedConfig): Promise<{
    changed: boolean;
    activatedIds: string[];
}>;
/**
 * `session.hook("compaction")` handler. Increments the session epoch and
 * clears seen files, so the very next context hook re-injects the active
 * task / rules baseline.
 */
export declare function sessionCompactionHandler(event: any, config: ResolvedConfig): Promise<void>;
export declare const pendingRecallBlocks: string[];
export declare const pendingRecallByCall: Map<string, string>;
/**
 * Tool-name extraction for execute.after events. OpenCode V2 passes
 * `event.name`; V1 / Claude / Hermes pass `event.tool` / `event.tool_name`.
 * `event.toolName` is checked first.
 */
export declare function extractToolName(event: any): string;
export declare function initialize(ctx: any): Promise<void>;
export declare function handleHookEvent(event: Record<string, unknown>): Promise<Record<string, unknown>>;
export declare const handleClaudeHookEvent: typeof handleHookEvent;
export declare function onTurnEnd(turn: {
    role: 'user' | 'assistant';
    content: string;
}): Promise<void>;
export declare function fetchContextPlan(opts: {
    namespace: string;
    query?: string;
    file_path?: string;
    max_items?: number;
    max_bytes?: number;
    format?: 'json' | 'text';
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
export interface HermesPluginContext {
    projectDir?: string;
    config?: Record<string, unknown>;
    pluginConfig?: Record<string, unknown>;
    tool?: {
        hook?: (name: string, handler: (input: any) => Promise<any>) => unknown;
    };
    hooks?: {
        beforeToolCall?: (handler: (input: any) => Promise<any>) => unknown;
        onTurnEnd?: (handler: HermesTurnEndHandler) => unknown;
    };
    logger?: {
        info?: (message: string, detail?: unknown) => void;
        warn?: (message: string, detail?: unknown) => void;
        error?: (message: string, detail?: unknown) => void;
    };
}
export interface HermesTurn {
    role: 'user' | 'assistant';
    content: string;
    turnId?: string;
    sessionId?: string;
}
export type HermesTurnEndHandler = (turn: HermesTurn) => Promise<void>;
declare const _default: {
    id: string;
    setup: (ctx: HermesPluginContext) => Promise<void>;
    initialize: (ctx: HermesPluginContext) => Promise<void>;
    handleHookEvent: (event: Record<string, unknown>) => Promise<Record<string, unknown>>;
    handleClaudeHookEvent: (event: Record<string, unknown>) => Promise<Record<string, unknown>>;
    onTurnEnd: (turn: HermesTurn) => Promise<void>;
    fetchContextPlan: typeof fetchContextPlan;
    resetState: () => void;
};
export default _default;
//# sourceMappingURL=index.d.ts.map
