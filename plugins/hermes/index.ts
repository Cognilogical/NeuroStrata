/**
 * NeuroStrata Hermes Agent Plugin — Client Shim
 *
 * Adapted from the OpenCode plugin for the Hermes Agent CLI.
 * Provides Zero-Action Start guard, file-touch recall, and subagent
 * prompt auto-injection without requiring @opencode/plugin import.
 */

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

function asString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() !== "" ? value.trim() : undefined;
}

function asNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : undefined;
}

function asBoolean(value: unknown): boolean | undefined {
  return typeof value === "boolean" ? value : undefined;
}

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
        asBoolean(intent.injectReminder) ?? DEFAULT_CONFIG.intentDetection.injectReminder,
    },
  };
}

// ──────────────────────────────────────────────────────────────────────
// Credential
// ──────────────────────────────────────────────────────────────────────

export function daemonTokenPath(tokenFile?: string | null): string {
  if (tokenFile && tokenFile.trim() !== "") return tokenFile.trim();
  return join(homedir(), ".config", "neurostrata", DEFAULT_TOKEN_FILE);
}

export function loadDaemonToken(tokenFile?: string | null): string | null {
  try {
    const raw = readFileSync(daemonTokenPath(tokenFile), "utf8").trim();
    return raw.length > 0 ? raw : null;
  } catch {
    return null;
  }
}

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

export function buildToolsListRequest(id: number): JsonRpcRequest {
  return { jsonrpc: "2.0", id, method: "tools/list", params: {} };
}

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
  bucket: string;
  reasons: string[];
  constraints: string[];
  notes: string[];
  rule_ids: string[];
}

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
  authenticated: boolean;
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

let activeSessionId: string | null = null;
let activeTaskId: string | null = null;
let mutationsSinceNote = 0;

export function resetState(): void {
  activeSessionId = null;
  activeTaskId = null;
  mutationsSinceNote = 0;
}

export function adapterStatus(): AdapterStatus {
  return { ...status, missingTools: [...status.missingTools] };
}

function setStatus(patch: Partial<AdapterStatus>): void {
  status = { ...status, ...patch, updatedAt: new Date().toISOString() };
}

// ──────────────────────────────────────────────────────────────────────
// Intent Detection
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
  ["need to follow up ", "need to follow up"],
  ["still need to ", "still need to"],
  ["must also fix ", "must also fix"],
  ["filed follow-up ", "filed follow-up"],
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

    const clauseStart = text.slice(0, idx).lastIndexOf("\n") + 1;
    let start = clauseStart;
    const sentPos = text.slice(0, idx).lastIndexOf(". ");
    if (sentPos !== -1 && sentPos + 2 > start) start = sentPos + 2;
    const clausePrefix = text.slice(start, idx).trimStart().toLowerCase();
    if (NEGATION_PREFIXES.some((p) => clausePrefix.startsWith(p))) continue;

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
// File-touch prefetch
// ──────────────────────────────────────────────────────────────────────

export const PREFETCH_LRU_CAP = 128;
export const PREFETCH_MAX_CHARS = 2048;
export const prefetchedPaths = new Map<string, number>();
export const injectedMemoryIds = new Set<string>();

function recordPrefetchedPath(key: string): void {
  prefetchedPaths.set(key, Date.now());
  if (prefetchedPaths.size > PREFETCH_LRU_CAP) {
    const oldest = prefetchedPaths.keys().next().value;
    if (oldest !== undefined) prefetchedPaths.delete(oldest);
  }
}

function touchPrefetchedPath(key: string): void {
  prefetchedPaths.delete(key);
  recordPrefetchedPath(key);
}

export interface FileMemoryHit {
  id: string;
  valid_from: string;
  memory_type: string;
  content: string;
}

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
    if (!id) continue;
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

function capSnippets(snippets: string[]): string[] {
  let total = 0;
  const out: string[] = [];
  for (const snippet of snippets) {
    if (total >= PREFETCH_MAX_CHARS) break;
    const room = PREFETCH_MAX_CHARS - total;
    out.push(snippet.length > room ? snippet.slice(0, room) : snippet);
    total += Math.min(snippet.length, room) + 1;
  }
  return out;
}

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

export const SUBAGENT_CONTEXT_MARKER = "### NeuroStrata Task & Graph Context";

export function buildSubagentContextHeader(
  namespace: string,
  taskId: string | null,
): string {
  const taskLabel = taskId ?? "unclaimed (claim a task first)";
  return (
    `${SUBAGENT_CONTEXT_MARKER} (AUTO-INJECTED BY NEUROSTRATA PLUGIN)\n` +
    `- Namespace: \`${namespace}\` | Active Task ID: \`${taskLabel}\`\n` +
    `- Mandatory Graph-First Lookup Rule: Before blind \`grep\`/\`glob\` or editing files, query \`neurostrata_memories_for_file\`, \`neurostrata_related\`, \`neurostrata_search_memory\` (\`include_related: true\`), or \`neurostrata_sparql_query\` (or \`neurostrata-mcp\` CLI) to inspect governing rules (\`GOVERNS\`) and AST call/import edges (\`DEPENDS_ON\`, \`calls\`, \`imports\`).\n\n`
  );
}

export function injectSubagentPrompt(
  target: Record<string, unknown>,
  namespace: string,
  taskId: string | null,
): boolean {
  const raw = typeof target.prompt === "string" ? target.prompt : null;
  if (raw === null || raw.includes(SUBAGENT_CONTEXT_MARKER)) return false;
  target.prompt = buildSubagentContextHeader(namespace, taskId) + raw;
  return true;
}

// ──────────────────────────────────────────────────────────────────────
// Plugin Entry Point
// ──────────────────────────────────────────────────────────────────────

let config: PluginConfig = DEFAULT_CONFIG;
let mcpClient!: NeuroStrataMCPClient;

export function resetClientForTesting(): void {
  mcpClient = undefined as any;
  resetState();
}

let pluginCtx: any | null = null;

function requireCtx(): any {
  if (!pluginCtx) {
    throw new Error("NeuroStrata plugin used before initialize()");
  }
  return pluginCtx;
}

function firstRecord(...candidates: unknown[]): Record<string, unknown> {
  for (const candidate of candidates) {
    if (typeof candidate === "object" && candidate !== null) {
      return candidate as Record<string, unknown>;
    }
  }
  return {};
}

export async function initialize(ctx: any): Promise<void> {
  pluginCtx = ctx;
  config = resolveConfig(ctx?.config ?? ctx?.pluginConfig ?? ctx);

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

  // Hermes Agent tool names support (without @opencode/plugin import)
  // Register via ctx?.tool?.hook?.("execute.before", ...) for pre-tool hook
  await ctx?.tool?.hook?.("execute.before", async (input: any) => {
    const toolName = input?.tool;

    // File-touch recall (§4.5): on read/write/edit touching a file
    const hermesFileTools = ["read_file", "read", "write_file", "write", "patch_file", "edit"];
    if (hermesFileTools.includes(toolName)) {
      const args = asRecord(input?.input);
      const altArgs = asRecord(input?.args);
      const filePath =
        asString(args.filePath) ??
        asString(args.path) ??
        asString(altArgs.filePath) ??
        asString(altArgs.path);
      if (filePath) {
        try {
          const snippets = await prefetchMemoriesForFile(config.namespace, filePath);
          if (snippets.length > 0) {
            console.info(`[NeuroStrata] File-touch recall for ${filePath}\n${snippets.join("\n")}`);
          }
        } catch (err: any) {
          setStatus({ state: "degraded", lastError: String(err) });
          console.warn("[NeuroStrata] File-touch prefetch failed", { file: filePath, error: String(err) });
        }
      }
    }

    // Subagent prompt auto-injection for delegate_task/subagent/Task
    const subagentNames = ["delegate_task", "subagent", "Task", "task", "dispatch_agent"];
    if (subagentNames.includes(toolName)) {
      if (input?.input && typeof input.input === "object") {
        injectSubagentPrompt(
          input.input as Record<string, unknown>,
          config.namespace,
          activeTaskId,
        );
      }
      if (input?.args && typeof input.args === "object") {
        injectSubagentPrompt(
          input.args as Record<string, unknown>,
          config.namespace,
          activeTaskId,
        );
      }
    }

    // Session/task tracking
    const mutationTools = [
      "neurostrata_task_claim",
      "neurostrata_task_update",
      "neurostrata_task_complete",
    ];
    if (mutationTools.includes(toolName)) {
      const payloadArgs = firstRecord(input?.input, input?.args);
      const sessionId =
        asString(payloadArgs.session_id) ??
        asString(input?.session_id) ??
        asString(input?.sessionID);
      if (sessionId) activeSessionId = sessionId;
      const taskId = asString(payloadArgs.id);
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

    // Guard validation on mutating Hermes Agent tools
    const hermesMutatingTools = ["write_file", "write", "patch_file", "edit", "terminal", "bash"];
    if (hermesMutatingTools.includes(toolName)) {
      const call: any = {
        name: toolName,
        args: firstRecord(input?.input, input?.args),
        session_id:
          asString(input?.session_id) ??
          asString(input?.sessionID) ??
          activeSessionId ??
          undefined,
      };
      if (!config.strictMode) {
        mutationsSinceNote++;
        return;
      }
      const actionType = toolName;
      const payload = JSON.stringify(call.args);
      const cwd = process.cwd();
      const { session_id, active_task_id, worktree } = callState(call);
      try {
        const text = await mcpClient!.callTool("neurostrata_guard_validate", {
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
          const reason = result.reasons.join("; ") || result.constraints.join("; ") || result.bucket;
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
      } catch (err: any) {
        const message = String(err);
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
      mutationsSinceNote++;
    }
  });

  // Also register via ctx?.hooks?.beforeToolCall?.(function) if available
  await ctx?.hooks?.beforeToolCall?.(async (input: any) => {
    // Subagent injection via alternative hook interface
    const subagentNames = ["delegate_task", "subagent", "Task", "task", "dispatch_agent"];
    const toolName = input?.tool ?? input?.name ?? input?.action ?? input?.command ?? "";
    if (subagentNames.includes(toolName)) {
      if (input?.input && typeof input.input === "object") {
        injectSubagentPrompt(
          input.input as Record<string, unknown>,
          config.namespace,
          activeTaskId,
        );
      }
      if (input?.args && typeof input.args === "object") {
        injectSubagentPrompt(
          input.args as Record<string, unknown>,
          config.namespace,
          activeTaskId,
        );
      }
    }
  });

  // Register onTurnEnd hook via ctx?.hooks?.onTurnEnd?.(onTurnEnd)
  await ctx?.hooks?.onTurnEnd?.(onTurnEnd);

  await negotiateCapabilities();

  console.info("[NeuroStrata] Hermes plugin initialized", {
    namespace: config.namespace,
    endpoint: config.endpoint,
    authenticated: status.authenticated,
    state: status.state,
    missingTools: status.missingTools,
  });
}

async function negotiateCapabilities(): Promise<void> {
  try {
    const available = new Set(await mcpClient!.listToolNames());
    const missing = REQUIRED_TOOLS.filter((tool) => !available.has(tool));
    setStatus({
      state: missing.length > 0 ? "degraded" : "ready",
      missingTools: missing,
      lastError: undefined,
    });
  } catch (err: any) {
    setStatus({ state: "unavailable", lastError: String(err) });
  }
}

function callState(call: any): {
  session_id: string;
  active_task_id: string | null;
  worktree: string;
} {
  const args = asRecord(call.args);
  return {
    session_id:
      asString(call.session_id) ??
      asString(args.session_id) ??
      activeSessionId ??
      "unknown",
    active_task_id: asString(args.active_task_id) ?? activeTaskId,
    worktree: asString(args.worktree) ?? process.cwd(),
  };
}

export async function onTurnEnd(turn: {
  role: "user" | "assistant";
  content: string;
}): Promise<void> {
  if (!config.intentDetection.enabled) return;
  const text = turn.content;
  if (text.includes("neurostrata_task_create")) return;
  const signals = detectTaskIntentsLocal(text);
  if (signals.length === 0) return;
  if (!config.intentDetection.injectReminder) return;

  const markerList = signals.map((signal) => signal.marker).join(", ");
  const nudge = `[NeuroStrata] Task intent detected (${markerList}) but no neurostrata_task_create in this turn. Consider creating a task to track this work.`;
  console.info("[NeuroStrata] " + nudge);
}

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

export default {
  id: "neurostrata-hermes",
  setup: initialize,
  initialize,
};
