/**
 * NeuroStrata OpenCode Plugin — Client Shim
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

import { Plugin } from "@opencode/plugin";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

// ──────────────────────────────────────────────────────────────────────
// Configuration
// ──────────────────────────────────────────────────────────────────────

/** Where the daemon serves MCP. The setup default is 34343, not 8080. */
export const DEFAULT_ENDPOINT = "http://127.0.0.1:34343/mcp";

/** The daemon's bearer token, as the daemon itself lays it down. */
export const DEFAULT_TOKEN_FILE = "daemon.token";

/** The tools this shim needs from the core to do its job. */
export const REQUIRED_TOOLS = [
  "neurostrata_guard_validate",
  "neurostrata_task_create",
];

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

const DEFAULT_CONFIG: PluginConfig = {
  namespace: "NeuroStrata",
  endpoint: DEFAULT_ENDPOINT,
  bearerToken: null,
  tokenFile: null,
  strictMode: true,
  requestTimeoutMs: 5000,
  intentDetection: { enabled: true, injectReminder: true },
};

function asRecord(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : {};
}

/**
 * First argument-carrying object among the candidates.
 *
 * Hosts disagree on where payload args live (`input.input` vs `input.args`);
 * `asRecord` folds a miss to `{}`, so the `??` chain must compare the raw
 * values and skip misses outright.
 */
function firstRecord(...candidates: unknown[]): Record<string, unknown> {
  for (const candidate of candidates) {
    if (typeof candidate === "object" && candidate !== null) {
      return candidate as Record<string, unknown>;
    }
  }
  return {};
}

function asString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() !== "" ? value.trim() : undefined;
}

function asNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : undefined;
}

function asBoolean(value: unknown): boolean | undefined {
  return typeof value === "boolean" ? value : undefined;
}

/**
 * Merge the host config over the defaults.
 *
 * `guardEndpoint` is still honored: it is what older installs wrote, and
 * silently ignoring it would point the shim at the wrong port.
 */
export function resolveConfig(raw: unknown): PluginConfig {
  const input = asRecord(raw);
  const intent = asRecord(input.intentDetection);
  const endpoint =
    asString(input.endpoint) ??
    asString(input.guardEndpoint) ??
    DEFAULT_CONFIG.endpoint;

  return {
    namespace: asString(input.namespace) ?? DEFAULT_CONFIG.namespace,
    endpoint,
    bearerToken: asString(input.bearerToken) ?? null,
    tokenFile: asString(input.tokenFile) ?? null,
    strictMode: asBoolean(input.strictMode) ?? DEFAULT_CONFIG.strictMode,
    requestTimeoutMs:
      asNumber(input.requestTimeoutMs) ?? DEFAULT_CONFIG.requestTimeoutMs,
    intentDetection: {
      enabled: asBoolean(intent.enabled) ?? DEFAULT_CONFIG.intentDetection.enabled,
      injectReminder:
        asBoolean(intent.injectReminder) ??
        DEFAULT_CONFIG.intentDetection.injectReminder,
    },
  };
}

// ──────────────────────────────────────────────────────────────────────
// Credential
// ──────────────────────────────────────────────────────────────────────

/** The absolute path of the daemon token file. */
export function daemonTokenPath(tokenFile?: string | null): string {
  if (tokenFile && tokenFile.trim() !== "") return tokenFile.trim();
  return join(homedir(), ".config", "neurostrata", DEFAULT_TOKEN_FILE);
}

/**
 * Read the daemon's bearer token.
 *
 * Returns `null` when there is no readable token: the daemon generates one
 * on first start, so an absent file means "the daemon has never run", not
 * "authentication is optional". The caller must report that as degraded
 * rather than pretend the request is anonymous.
 */
export function loadDaemonToken(tokenFile?: string | null): string | null {
  try {
    const raw = readFileSync(daemonTokenPath(tokenFile), "utf8").trim();
    return raw.length > 0 ? raw : null;
  } catch {
    return null;
  }
}

/** The request headers, with the bearer attached when there is one. */
export function buildHeaders(token: string | null): Record<string, string> {
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  if (token) headers.Authorization = `Bearer ${token}`;
  return headers;
}

// ──────────────────────────────────────────────────────────────────────
// MCP wire format
// ──────────────────────────────────────────────────────────────────────

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
  error?: { code?: number; message?: string };
}

export interface McpToolResult {
  content?: Array<{ type?: string; text?: string }>;
  isError?: boolean;
}

/** Build the `tools/call` request the core actually answers. */
export function buildToolCallRequest(
  id: number,
  name: string,
  args: unknown,
): JsonRpcRequest {
  return {
    jsonrpc: "2.0",
    id,
    method: "tools/call",
    params: { name, arguments: args },
  };
}

/** Build the `tools/list` request used for capability negotiation. */
export function buildToolsListRequest(id: number): JsonRpcRequest {
  return { jsonrpc: "2.0", id, method: "tools/list", params: {} };
}

/**
 * Unwrap `result.content[].text`.
 *
 * The core answers a tool call with a content array, not with the typed
 * result a tool would return. An answer with no text is a broken answer:
 * it is reported, never treated as an empty success.
 */
export function extractToolText(result: unknown): string {
  const content = (result as McpToolResult | undefined)?.content;
  if (!Array.isArray(content)) {
    throw new Error("tool result carried no content array");
  }
  const text = content
    .map((part) => (typeof part?.text === "string" ? part.text : ""))
    .filter((part) => part.length > 0)
    .join("\n");
  if (text.length === 0) {
    throw new Error("tool result carried no text content");
  }
  return text;
}

/** The tool names from a `tools/list` answer. */
export function extractToolNames(result: unknown): string[] {
  const tools = (result as { tools?: Array<{ name?: string }> } | undefined)?.tools;
  if (!Array.isArray(tools)) {
    throw new Error("tools/list returned no tools array");
  }
  return tools
    .map((tool) => (typeof tool?.name === "string" ? tool.name : ""))
    .filter((name) => name.length > 0);
}

// ──────────────────────────────────────────────────────────────────────
// MCP client
// ──────────────────────────────────────────────────────────────────────

export class NeuroStrataMCPClient {
  private readonly endpoint: string;
  private readonly token: string | null;
  private readonly timeoutMs: number;
  private requestId = 0;

  constructor(options: {
    endpoint: string;
    token: string | null;
    timeoutMs: number;
  }) {
    this.endpoint = options.endpoint;
    this.token = options.token;
    this.timeoutMs = options.timeoutMs;
  }

  /** Call one tool and return its text content. */
  async callTool(name: string, args: unknown): Promise<string> {
    const id = ++this.requestId;
    const response = await this.post(buildToolCallRequest(id, name, args));
    if (response.error) {
      throw new Error(
        `[${response.error.code ?? "?"}] ${response.error.message ?? "tool call failed"}`,
      );
    }
    if (response.result === undefined || response.result === null) {
      throw new Error(`${name} returned no result`);
    }
    const text = extractToolText(response.result);
    const isError = (response.result as McpToolResult).isError;
    if (isError) {
      throw new Error(`${name} failed: ${text}`);
    }
    return text;
  }

  /** The names of the tools the core exposes. */
  async listToolNames(): Promise<string[]> {
    const id = ++this.requestId;
    const response = await this.post(buildToolsListRequest(id));
    if (response.error) {
      throw new Error(
        `[${response.error.code ?? "?"}] ${response.error.message ?? "tools/list failed"}`,
      );
    }
    return extractToolNames(response.result);
  }

  private async post(body: JsonRpcRequest): Promise<JsonRpcResponse> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeoutMs);
    try {
      const response = await fetch(this.endpoint, {
        method: "POST",
        headers: buildHeaders(this.token),
        body: JSON.stringify(body),
        signal: controller.signal,
      });
      if (!response.ok) {
        throw new Error(`HTTP ${response.status} ${response.statusText}`);
      }
      return (await response.json()) as JsonRpcResponse;
    } catch (err) {
      if (err instanceof Error && err.name === "AbortError") {
        throw new Error(`request timed out after ${this.timeoutMs}ms`);
      }
      throw err;
    } finally {
      clearTimeout(timer);
    }
  }
}

// ──────────────────────────────────────────────────────────────────────
// Guard verdict
// ──────────────────────────────────────────────────────────────────────

export interface GuardValidateResult {
  verdict: "pass" | "reject";
  /** The core's own verdict bucket, kept so nothing is flattened away. */
  bucket: string;
  reasons: string[];
  constraints: string[];
  notes: string[];
  rule_ids: string[];
}

/**
 * Read the guard's answer.
 *
 * The core prefixes the body with `namespace:` and `trace_id:` lines, so the
 * JSON body is what lies between the first `{` and the last `}`. The verdict
 * is tagged with `bucket`, not `type`.
 */
export function parseGuardValidate(text: string): GuardValidateResult {
  const start = text.indexOf("{");
  const end = text.lastIndexOf("}");
  if (start === -1 || end <= start) {
    throw new Error(
      `guard_validate returned no JSON body: ${text.slice(0, 200)}`,
    );
  }

  let body: Record<string, unknown>;
  try {
    body = JSON.parse(text.slice(start, end + 1)) as Record<string, unknown>;
  } catch (err) {
    throw new Error(`guard_validate returned an unreadable body: ${String(err)}`);
  }

  const verdict = asRecord(body.verdict);
  const bucket = asString(verdict.bucket) ?? "Unknown";
  const reasons = Array.isArray(verdict.reasons)
    ? verdict.reasons.map((reason) => String(reason))
    : [];
  const constraints = Array.isArray(verdict.constraints)
    ? verdict.constraints.map((constraint) => String(constraint))
    : [];
  const notes = Array.isArray(verdict.notes)
    ? verdict.notes.map((note) => String(note))
    : [];
  const rule_ids = Array.isArray(body.rule_ids_triggered)
    ? body.rule_ids_triggered.map((id) => String(id))
    : [];

  const rejecting =
    bucket === "DeterministicReject" || bucket === "SandboxReject";

  return {
    verdict: rejecting ? "reject" : "pass",
    bucket,
    reasons,
    constraints,
    notes,
    rule_ids,
  };
}

// ──────────────────────────────────────────────────────────────────────
// Adapter state
// ──────────────────────────────────────────────────────────────────────

export type AdapterState =
  | "uninitialized"
  | "ready"
  | "degraded"
  | "unavailable";

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

function initialStatus(): AdapterStatus {
  return {
    state: "uninitialized",
    endpoint: DEFAULT_ENDPOINT,
    namespace: "NeuroStrata",
    authenticated: false,
    missingTools: [],
    updatedAt: new Date(0).toISOString(),
  };
}

let status: AdapterStatus = initialStatus();

/**
 * Session/task tracking between host tool calls and guard validation.
 *
 * The guard needs `active_task_id`, but a host passes `write`/`edit` args
 * that carry none of it. Task mutations (`claim`/`update`/`complete`) whose
 * arguments carry `id` / `session_id` are the only reliable source.
 */
let activeSessionId: string | null = null;
let activeTaskId: string | null = null;

/**
 * Mutations since the heavyweight channels were last touched (a note on the
 * active task, a full claim, or completion). The guard consumes this through
 * `neurostrata_guard_validate`'s `mutations_since_note` argument to pin the
 * Breath prompt.
 */
let mutationsSinceNote = 0;

/** Reset the tracked session/task state (test hook). */
export function resetState(): void {
  activeSessionId = null;
  activeTaskId = null;
  mutationsSinceNote = 0;
}

/** The adapter's current state. A degraded core is visible here. */
export function adapterStatus(): AdapterStatus {
  return { ...status, missingTools: [...status.missingTools] };
}

function setStatus(patch: Partial<AdapterStatus>): void {
  status = { ...status, ...patch, updatedAt: new Date().toISOString() };
}

// ──────────────────────────────────────────────────────────────────────
// Intent Detection (local — mirrors src/task/intent.rs)
// ──────────────────────────────────────────────────────────────────────

export interface TaskIntentSignal {
  marker: string;
  excerpt: string;
}

export const MARKERS: readonly [string, string][] = [
  ["todo:", "todo:"],
  ["fixme:", "fixme:"],
  ["follow-up:", "follow-up:"],
  ["followup:", "followup:"],
  ["next step:", "next step:"],
  ["action item:", "action item:"],
  ["remaining work:", "remaining work:"],
  ["we should also ", "we should also"],
  ["later we need to ", "later we need to"],
  ["need to follow up", "need to follow up"],
  ["still need to ", "still need to"],
  ["must also fix ", "must also fix"],
  ["filed follow-up", "filed follow-up"],
  ["defer ", "defer"],
  ["deferred ", "deferred"],
];

export const NEGATION_PREFIXES = ["never ", "do not ", "don't ", "do not, "];

export function detectTaskIntentsLocal(text: string): TaskIntentSignal[] {
  const lower = text.toLowerCase();
  const out: TaskIntentSignal[] = [];
  const seen = new Set<string>();

  for (const [matchStr, canonical] of MARKERS) {
    const idx = lower.indexOf(matchStr);
    if (idx === -1) continue;
    if (seen.has(canonical)) continue;

    // negation guard
    const clauseStart = text.slice(0, idx).lastIndexOf("\n") + 1;
    let start = clauseStart;
    const sentPos = text.slice(0, idx).lastIndexOf(". ");
    if (sentPos !== -1 && sentPos + 2 > start) start = sentPos + 2;
    const clausePrefix = text.slice(start, idx).trimStart().toLowerCase();
    if (NEGATION_PREFIXES.some((p) => clausePrefix.startsWith(p))) continue;

    // excerpt
    const from = idx;
    const rest = text.slice(from);
    const end = Math.min(rest.indexOf("\n") === -1 ? rest.length : rest.indexOf("\n"), 160);
    let excerpt = rest.slice(0, end).trimEnd();
    if (excerpt.length > 160) excerpt = excerpt.slice(0, 160);

    seen.add(canonical);
    out.push({ marker: canonical, excerpt });
  }
  return out;
}

// ──────────────────────────────────────────────────────────────────────
// File-touch prefetch (design §4.5): LRU dedup + injected-memory dedup
// ──────────────────────────────────────────────────────────────────────

/** LRU cap: how many recently touched (`namespace:path`) keys are kept. */
export const PREFETCH_LRU_CAP = 128;

/** Output budget: one file-touch recall injects at most this many chars. */
export const PREFETCH_MAX_CHARS = 2048;

/**
 * Recently touched file paths, keyed `namespace:path`, in LRU order
 * (oldest first). Bounded so a long session cannot grow without bound.
 */
export const prefetchedPaths = new Map<string, number>();

/** `(memory_id:valid_from)` keys already injected this session. */
export const injectedMemoryIds = new Set<string>();

function recordPrefetchedPath(key: string): void {
  prefetchedPaths.set(key, Date.now());
  if (prefetchedPaths.size > PREFETCH_LRU_CAP) {
    const oldest = prefetchedPaths.keys().next().value;
    if (oldest !== undefined) prefetchedPaths.delete(oldest);
  }
}

function touchPrefetchedPath(key: string): void {
  // Re-insertion moves the key to the newest end of the LRU order.
  prefetchedPaths.delete(key);
  recordPrefetchedPath(key);
}

/** One governing memory as `memories_for_file` returns it. */
export interface FileMemoryHit {
  id: string;
  valid_from: string;
  memory_type: string;
  content: string;
}

/**
 * Read the `memories_for_file` answer.
 *
 * The core wraps the body in a content-array answer like every other tool,
 * and the array of governing memories arrives as `hits` or `memories`.
 */
export function parseFileMemoryHits(text: string): FileMemoryHit[] {
  const start = text.indexOf("{");
  const end = text.lastIndexOf("}");
  if (start === -1 || end <= start) {
    throw new Error(
      `memories_for_file returned no JSON body: ${text.slice(0, 200)}`,
    );
  }

  let body: Record<string, unknown>;
  try {
    body = JSON.parse(text.slice(start, end + 1)) as Record<string, unknown>;
  } catch (err) {
    throw new Error(
      `memories_for_file returned an unreadable body: ${String(err)}`,
    );
  }

  const raw = Array.isArray(body.hits)
    ? body.hits
    : Array.isArray(body.memories)
      ? body.memories
      : [];

  const out: FileMemoryHit[] = [];
  for (const item of raw) {
    if (typeof item !== "object" || item === null) continue;
    const hit = item as Record<string, unknown>;
    const id = asString(hit.id) ?? asString(hit.memory_id);
    const valid_from = asString(hit.valid_from) ?? "";
    if (!id) continue; // a hit without an id cannot be deduped — never inject
    out.push({
      id,
      valid_from,
      memory_type: asString(hit.memory_type) ?? asString(hit.type) ?? "memory",
      content:
        asString(hit.content) ??
        asString(hit.text) ??
        asString(hit.summary) ??
        "",
    });
  }
  return out;
}

function formatFileMemorySnippet(hit: FileMemoryHit): string {
  const where = hit.valid_from ? ` (valid_from ${hit.valid_from})` : "";
  return `[NeuroStrata ${hit.memory_type} ${hit.id}${where}] ${hit.content}`;
}

/** Cap the recalled snippets so one file touch can never flood context. */
function capSnippets(snippets: string[]): string[] {
  let total = 0;
  const out: string[] = [];
  for (const snippet of snippets) {
    if (total >= PREFETCH_MAX_CHARS) break;
    const room = PREFETCH_MAX_CHARS - total;
    out.push(snippet.length > room ? snippet.slice(0, room) : snippet);
    total += Math.min(snippet.length, room) + 1; // + join newline
  }
  return out;
}

/**
 * Recall governing memories when a file is touched (design §4.5).
 *
 * Each `namespace:path` key is prefetched at most once per LRU window;
 * each memory is injected at most once per `(id, valid_from)` key. Never
 * increments `access_count` — this is a read-only recall.
 */
export async function prefetchMemoriesForFile(
  namespace: string,
  filePath: string,
): Promise<string[]> {
  const path = filePath.trim();
  if (path === "") return [];

  const key = `${namespace}:${path}`;
  if (prefetchedPaths.has(key)) {
    touchPrefetchedPath(key);
    return [];
  }

  // Record the LRU entry only after the recall actually succeeded: a
  // disconnected or failed call must never permanently suppress prefetch
  // for this file within the LRU window.
  if (!mcpClient) return [];

  const text = await mcpClient.callTool("neurostrata_memories_for_file", {
    namespace,
    path,
    limit: 5,
  });
  recordPrefetchedPath(key);

  const out: string[] = [];
  for (const hit of parseFileMemoryHits(text)) {
    const dedupKey = `${hit.id}:${hit.valid_from}`;
    if (injectedMemoryIds.has(dedupKey)) continue;
    injectedMemoryIds.add(dedupKey);
    out.push(formatFileMemorySnippet(hit));
  }
  return capSnippets(out);
}

// ──────────────────────────────────────────────────────────────────────
// Plugin Entry Point
// ──────────────────────────────────────────────────────────────────────

let config: PluginConfig = DEFAULT_CONFIG;
let mcpClient: NeuroStrataMCPClient;

/**
 * Test hook: drop the cached MCP client so un-initialized code paths (like
 * `fetchContextPlan` before `initialize` ran) can be exercised.
 */
export function resetClientForTesting(): void {
  mcpClient = undefined as any;
  resetState();
}

/**
 * The plugin context, captured on initialize.
 *
 * The handlers below are module-level functions, so they cannot close over
 * `initialize`'s parameter. Capturing it here is what makes `ctx` reachable
 * from them at all.
 */
let pluginCtx: any | null = null;

function requireCtx(): any {
  if (!pluginCtx) {
    throw new Error("NeuroStrata plugin used before initialize()");
  }
  return pluginCtx;
}

export async function initialize(ctx: any): Promise<void> {
  pluginCtx = ctx;
  config = resolveConfig(ctx.config);

  const token = config.bearerToken ?? loadDaemonToken(config.tokenFile);
  mcpClient = new NeuroStrataMCPClient({
    endpoint: config.endpoint,
    token,
    timeoutMs: config.requestTimeoutMs,
  });

  setStatus({
    state: "uninitialized",
    endpoint: config.endpoint,
    namespace: config.namespace,
    authenticated: token !== null,
    missingTools: [],
    lastError: undefined,
  });

  // Register hooks before mutating tools (write, edit, bash).
  // V2 API: ctx.tool.hook("execute.before", ...) fires before each tool call.
  await ctx.tool.hook("execute.before", async (input: any) => {
    const toolName = input?.tool;

    // File-touch recall (§4.5): on read/write/edit touching a file, prefetch
    // the governing memories. Fail-soft: prefetch never blocks the tool, and
    // `read` is never guard-validated.
    if (toolName === "read" || toolName === "write" || toolName === "edit") {
      const args = asRecord(input?.input);
      const altArgs = asRecord(input?.args);
      const filePath =
        asString(args.filePath) ??
        asString(args.path) ??
        asString(altArgs.filePath) ??
        asString(altArgs.path);
      if (filePath) {
        try {
          const snippets = await prefetchMemoriesForFile(
            config.namespace,
            filePath,
          );
          if (snippets.length > 0) {
            console.info(
              `[NeuroStrata] File-touch recall for ${filePath}\n${snippets.join("\n")}`,
            );
          }
        } catch (err) {
          setStatus({ state: "degraded", lastError: String(err) });
          console.warn("[NeuroStrata] File-touch prefetch failed", {
            file: filePath,
            error: String(err),
          });
        }
      }
    }

    // Session/task tracking (F10): task mutations carry the `id` and
    // `session_id` guard validation needs for the mutating write/edit calls
    // that follow. Best-effort — a malformed payload never blocks the tool.
    if (
      toolName === "neurostrata_task_claim" ||
      toolName === "neurostrata_task_update" ||
      toolName === "neurostrata_task_complete"
    ) {
      const payloadArgs = firstRecord(input?.input, input?.args);
      const sessionId =
        asString(payloadArgs.session_id) ??
        asString(input?.session_id) ??
        asString(input?.sessionID);
      if (sessionId) activeSessionId = sessionId;
      const taskId = asString(payloadArgs.id);
      // Heavyweight turn: a note, claim, or completion satisfies the Breath
      // prompt, so the mutation counter starts over.
      if (
        toolName === "neurostrata_task_claim" ||
        toolName === "neurostrata_task_complete" ||
        payloadArgs.note !== undefined
      ) {
        mutationsSinceNote = 0;
      }
      if (toolName === "neurostrata_task_complete") {
        activeTaskId = null;
      } else if (taskId) {
        activeTaskId = taskId;
      }
    }

    // Guard validation stays on the mutating tools only.
    if (toolName !== "write" && toolName !== "edit" && toolName !== "bash") return;
    const call: any = {
      name: toolName,
      args: firstRecord(input?.input, input?.args),
      session_id:
        asString(input?.session_id) ??
        asString(input?.sessionID) ??
        activeSessionId ??
        undefined,
    };
    // Run guard validation (fail closed).
    if (!config.strictMode) {
      // Executed without validation: it is still a mutation.
      mutationsSinceNote++;
      return;
    }
    const actionType = toolName;
    const payload = JSON.stringify(call.args);
    const cwd = process.cwd();
    const { session_id, active_task_id, worktree } = callState(call);
    try {
      const text = await mcpClient.callTool("neurostrata_guard_validate", {
        namespace: config.namespace,
        session_id,
        active_task_id,
        worktree,
        action_type: actionType,
        payload,
        cwd,
        is_mutating: true,
        use_sandbox: false,
        mutations_since_note: mutationsSinceNote,
      });
      const result = parseGuardValidate(text);
      if (result.verdict === "reject") {
        const reason =
          result.reasons.join("; ") ||
          result.constraints.join("; ") ||
          result.bucket;
        console.warn("[NeuroStrata] Guard rejected mutating tool", {
          actionType,
          session_id,
          active_task_id,
          worktree,
          reason,
          rule_ids: result.rule_ids,
        });
        throw new Error(`Zero-Action Start violation: ${reason}`);
      }
    } catch (err) {
      const message = String(err);
      // Re-throw guard rejections as-is: `String(new Error(...))` prepends
      // "Error: ", so a startsWith check missed policy rejections and
      // flipped the adapter to "unavailable" on a plain policy answer.
      if (
        message.includes("Zero-Action Start violation:") ||
        message.includes("Guard rejected")
      ) {
        throw err;
      }
      setStatus({ state: "unavailable", lastError: message });
      console.error("[NeuroStrata] Guard validation failed", { error: message });
      throw new Error(
        `Guard validation unavailable (NeuroStrata core degraded): ${message}`,
      );
    }
    // Guard passed and the tool is about to execute: count the mutation.
    mutationsSinceNote++;
  });

  // Intent detection turn hook is best-effort in V2 (no direct conversation hook);
  // onTurnEnd remains exported for callers/tests.

  await negotiateCapabilities();

  console.info("[NeuroStrata] OpenCode plugin initialized", {
    namespace: config.namespace,
    endpoint: config.endpoint,
    authenticated: status.authenticated,
    state: status.state,
    missingTools: status.missingTools,
  });
}

/**
 * Ask the core what it can do.
 *
 * The shim must know whether the tool it depends on exists before it
 * pretends to work: a missing tool is a degraded adapter, and a core that
 * cannot be reached is an unavailable one. Both are recorded, and neither is
 * hidden behind a local fallback.
 */
async function negotiateCapabilities(): Promise<void> {
  try {
    const available = new Set(await mcpClient.listToolNames());
    const missing = REQUIRED_TOOLS.filter((tool) => !available.has(tool));
    setStatus({
      state: missing.length > 0 ? "degraded" : "ready",
      missingTools: missing,
      lastError: undefined,
    });
    if (missing.length > 0) {
      console.warn(
        "NeuroStrata core does not expose the tools this shim needs",
        { missing },
      );
    }
  } catch (err) {
    setStatus({
      state: "unavailable",
      lastError: String(err),
    });
    console.warn(
      "NeuroStrata core is unreachable; the shim is degraded",
      { error: String(err) },
    );
  }
}

// ──────────────────────────────────────────────────────────────────────
// Tool Wrapper: guard_validate before write/edit
// ──────────────────────────────────────────────────────────────────────

async function wrapWriteEdit(
  call: any,
  next: (call: any) => Promise<any>,
): Promise<any> {
  if (!config.strictMode) {
    const result_ = await next(call);
    // Executed without validation: it is still a mutation.
    mutationsSinceNote++;
    return result_;
  }

  const actionType = call.name; // "write" or "edit"
  const payload = JSON.stringify(call.args);
  const cwd = process.cwd();
  const { session_id, active_task_id, worktree } = callState(call);

  try {
    const text = await mcpClient.callTool("neurostrata_guard_validate", {
      namespace: config.namespace,
      session_id,
      active_task_id,
      worktree,
      action_type: actionType,
      payload,
      cwd,
      is_mutating: true,
      use_sandbox: false, // semantic-only for speed; sandbox opt-in
      mutations_since_note: mutationsSinceNote,
    });

    const result = parseGuardValidate(text);
    if (result.verdict === "reject") {
      const reason =
        result.reasons.join("; ") ||
        result.constraints.join("; ") ||
        result.bucket;
      console.warn("Guard rejected write/edit", {
        actionType,
        session_id,
        active_task_id,
        worktree,
        reason,
        rule_ids: result.rule_ids,
      });
      return {
        success: false,
        error: `Zero-Action Start violation: ${reason}`,
        data: { guard_verdict: result, degraded: false },
      };
    }
  } catch (err) {
    const message = String(err);
    setStatus({ state: "unavailable", lastError: message });
    console.error("Guard validation failed", { error: message });
    // Fail closed: a guard that cannot answer is not a guard that passed.
    return {
      success: false,
      error: `Guard validation unavailable (NeuroStrata core degraded): ${message}`,
      data: { degraded: true, state: status.state },
    };
  }

  const result_ = await next(call);
  // Executed: count the mutation after the call goes through.
  mutationsSinceNote++;
  return result_;
}

/**
 * AC2 State-check wrapper: blocks mutating tools when session/task/worktree
 * state shows Unpreflighted/BreathRequired (no active task claim or note
 * missing after 5+ mutations).
 */
function callState(call: any): {
  session_id: string;
  active_task_id: string | null;
  worktree: string;
} {
  const args = asRecord(call.args);
  return {
    // Fall back to the session/task tracked from task mutations: a
    // write/edit payload rarely carries either field itself.
    session_id:
      asString(call.session_id) ??
      asString(args.session_id) ??
      activeSessionId ??
      "unknown",
    active_task_id: asString(args.active_task_id) ?? activeTaskId,
    worktree: asString(args.worktree) ?? process.cwd(),
  };
}

async function wrapMutatingTool(
  call: any,
  next: (call: any) => Promise<any>,
): Promise<any> {
  if (!config.strictMode) {
    const result_ = await next(call);
    // Executed without validation: it is still a mutation.
    mutationsSinceNote++;
    return result_;
  }

  const actionType = call.name;
  const payload = JSON.stringify(call.args);
  const cwd = process.cwd();
  const { session_id, active_task_id, worktree } = callState(call);

  try {
    const text = await mcpClient.callTool("neurostrata_guard_validate", {
      namespace: config.namespace,
      session_id,
      active_task_id,
      worktree,
      action_type: actionType,
      payload,
      cwd,
      is_mutating: true,
      use_sandbox: false,
      mutations_since_note: mutationsSinceNote,
    });

    const result = parseGuardValidate(text);
    if (result.verdict === "reject") {
      const reason =
        result.reasons.join("; ") || result.constraints.join("; ") || result.bucket;
      console.warn("Guard rejected mutating tool", {
        actionType,
        session_id,
        active_task_id,
        worktree,
        reason,
        rule_ids: result.rule_ids,
      });
      return {
        success: false,
        error: `Zero-Action Start violation: ${reason}`,
        data: { guard_verdict: result, degraded: false },
      };
    }
  } catch (err) {
    const message = String(err);
    setStatus({ state: "unavailable", lastError: message });
    console.error("Guard validation failed", { error: message });
    return {
      success: false,
      error: `Guard validation unavailable (NeuroStrata core degraded): ${message}`,
      data: { degraded: true, state: status.state },
    };
  }
  const result_ = await next(call);
  // Executed: count the mutation after the call goes through.
  mutationsSinceNote++;
  return result_;
}

// ──────────────────────────────────────────────────────────────────────
// Turn Hook: intent detection + nudge
// ──────────────────────────────────────────────────────────────────────

export async function onTurnEnd(turn: {
  role: "user" | "assistant";
  content: string;
}): Promise<void> {
  if (!config.intentDetection.enabled) return;

  const text = turn.content;
  if (text.includes("neurostrata_task_create")) return;

  // The core exposes no intent-detection tool: the earlier
  // `neurostrata_detect_task_intents` call was a phantom the core answered
  // with "Unknown tool". The lexical detector runs here instead, and
  // Phase 0 writes nothing — a detected intent is a nudge, never a task.
  const signals = detectTaskIntentsLocal(text);
  if (signals.length === 0) return;
  if (!config.intentDetection.injectReminder) return;

  const markerList = signals.map((signal) => signal.marker).join(", ");
  const nudge = `[NeuroStrata] Task intent detected (${markerList}) but no neurostrata_task_create in this turn. Consider creating a task to track this work.`;
  console.info("[NeuroStrata] " + nudge);
}

// ──────────────────────────────────────────────────────────────────────
// Everything above is exported at its declaration: the markers, the config
// resolver, the wire helpers, the verdict parser and the adapter status are
// the contract the adapter tests exercise.
// ──────────────────────────────────────────────────────────────────────

export async function fetchContextPlan(opts: {
  namespace: string;
  query?: string;
  file_path?: string;
  max_items?: number;
  max_bytes?: number;
  format?: "json" | "text";
}): Promise<string> {
  if (!mcpClient) throw new Error("NeuroStrata MCP client not initialized");
  return await mcpClient.callTool("neurostrata_context_plan", opts);
}

export default Plugin.define({
  id: "neurostrata",
  async setup(ctx: any) {
    await initialize(ctx);
    return;
  },
});
