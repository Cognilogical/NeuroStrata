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
// ──────────────────────────────────────────────────────────────────────
// Node built-ins only (ESM). Zero require(), zero external runtime deps.
// ──────────────────────────────────────────────────────────────────────
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
// ──────────────────────────────────────────────────────────────────────
// Configuration
// ──────────────────────────────────────────────────────────────────────
export const DEFAULT_ENDPOINT = "http://127.0.0.1:34343/mcp";
export const DEFAULT_TOKEN_FILE = "daemon.token";
export const REQUIRED_TOOLS = ["neurostrata_guard_validate", "neurostrata_task_create"];
const DEFAULT_CONFIG = {
    namespace: "NeuroStrata",
    endpoint: DEFAULT_ENDPOINT,
    bearerToken: null,
    tokenFile: null,
    strictMode: true,
    requestTimeoutMs: 5000,
    intentDetection: { enabled: true, injectReminder: true },
};
function asRecord(value) {
    return typeof value === "object" && value !== null ? value : {};
}
function firstRecord(...candidates) {
    for (const candidate of candidates) {
        if (typeof candidate === "object" && candidate !== null) {
            return candidate;
        }
    }
    return {};
}
function asString(value) {
    return typeof value === "string" && value.trim() !== "" ? value.trim() : undefined;
}
function asNumber(value) {
    return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : undefined;
}
function asBoolean(value) {
    return typeof value === "boolean" ? value : undefined;
}
/**
 * Resolve `namespace` in strict priority order:
 *  1. pluginConfig.namespace (non-empty trimmed string)
 *  2. process.env.NEUROSTRATA_NAMESPACE (non-empty trimmed string)
 *  3. discoverProjectNamespace(cwd)
 * Never falls back to a hardcoded "NeuroStrata".
 */
export function resolveConfig(pluginConfig = {}, cwd = process.cwd()) {
    const input = asRecord(pluginConfig);
    const intent = asRecord(input.intentDetection);
    const endpoint = asString(input.endpoint) ?? asString(input.guardEndpoint) ?? DEFAULT_CONFIG.endpoint;
    const namespace = asString(input.namespace) ??
        asString(process.env.NEUROSTRATA_NAMESPACE) ??
        discoverProjectNamespace(cwd);
    return {
        namespace,
        endpoint,
        bearerToken: asString(input.bearerToken) ?? null,
        tokenFile: asString(input.tokenFile) ?? null,
        strictMode: asBoolean(input.strictMode) ?? DEFAULT_CONFIG.strictMode,
        requestTimeoutMs: asNumber(input.requestTimeoutMs) ?? DEFAULT_CONFIG.requestTimeoutMs,
        intentDetection: {
            enabled: asBoolean(intent.enabled) ?? DEFAULT_CONFIG.intentDetection.enabled,
            injectReminder: asBoolean(intent.injectReminder) ?? DEFAULT_CONFIG.intentDetection.injectReminder,
        },
    };
}
// ──────────────────────────────────────────────────────────────────────
// Bug 2: project-root / namespace discovery
// ──────────────────────────────────────────────────────────────────────
/**
 * Walk up from resolve(startDir) to the filesystem root looking for `.git`
 * (dir or file) or `.NeuroStrata`; return the directory containing it, or
 * resolve(startDir) if none found.
 */
export function findProjectRoot(startDir = process.cwd()) {
    // Pass 1: walk up looking for `.git` first; a `.git` anchors the project root.
    let current = resolve(startDir);
    while (true) {
        if (existsSync(join(current, ".git"))) {
            return current;
        }
        const next = dirname(current);
        if (next === current)
            break;
        current = next;
    }
    // Pass 2: only if no `.git` anywhere in the ancestor chain, walk up looking
    // for `.NeuroStrata`. Otherwise fall back to resolve(startDir).
    current = resolve(startDir);
    while (true) {
        if (existsSync(join(current, ".NeuroStrata"))) {
            return current;
        }
        const next = dirname(current);
        if (next === current)
            break;
        current = next;
    }
    return resolve(startDir);
}
/**
 * 1. Walk up to findProjectRoot(startDir) checking <dir>/.NeuroStrata/session_queries.json;
 *    if it parses as JSON with a non-empty string .namespace, return it trimmed.
 * 2. Otherwise return basename(findProjectRoot(startDir)) || "default".
 */
export function discoverProjectNamespace(startDir = process.cwd()) {
    const root = findProjectRoot(startDir);
    let current = resolve(startDir);
    while (true) {
        const sessionQueriesPath = join(current, ".NeuroStrata", "session_queries.json");
        try {
            if (existsSync(sessionQueriesPath)) {
                const content = readFileSync(sessionQueriesPath, "utf8");
                const parsed = JSON.parse(content);
                if (parsed !== null &&
                    typeof parsed === "object" &&
                    typeof parsed.namespace === "string" &&
                    parsed.namespace.trim() !== "") {
                    return parsed.namespace.trim();
                }
            }
        }
        catch {
            // fall through to next ancestor
        }
        if (current === root)
            break;
        const next = dirname(current);
        if (next === current)
            break;
        current = next;
    }
    return basename(root) || "default";
}
// ──────────────────────────────────────────────────────────────────────
// Credential
// ──────────────────────────────────────────────────────────────────────
export function daemonTokenPath(tokenFile) {
    if (tokenFile && tokenFile.trim() !== "")
        return tokenFile.trim();
    return join(homedir(), ".config", "neurostrata", DEFAULT_TOKEN_FILE);
}
export function loadDaemonToken(tokenFile) {
    try {
        const raw = readFileSync(daemonTokenPath(tokenFile), "utf8").trim();
        return raw.length > 0 ? raw : null;
    }
    catch {
        return null;
    }
}
export function buildHeaders(token) {
    const headers = { "Content-Type": "application/json" };
    if (token)
        headers.Authorization = `Bearer ${token}`;
    return headers;
}
export function buildToolCallRequest(id, name, args) {
    return { jsonrpc: "2.0", id, method: "tools/call", params: { name, arguments: args } };
}
export function buildToolsListRequest(id) {
    return { jsonrpc: "2.0", id, method: "tools/list", params: {} };
}
export function extractToolText(result) {
    const content = result?.content;
    if (!Array.isArray(content))
        throw new Error("tool result carried no content array");
    const text = content
        .map((part) => (typeof part?.text === "string" ? part.text : ""))
        .filter((part) => part.length > 0)
        .join("\n");
    if (text.length === 0)
        throw new Error("tool result carried no text content");
    return text;
}
export function extractToolNames(result) {
    const tools = result?.tools;
    if (!Array.isArray(tools))
        throw new Error("tools/list returned no tools array");
    return tools
        .map((tool) => (typeof tool?.name === "string" ? tool.name : ""))
        .filter((name) => name.length > 0);
}
// ──────────────────────────────────────────────────────────────────────
// MCP client
// ──────────────────────────────────────────────────────────────────────
export class NeuroStrataMCPClient {
    endpoint;
    token;
    timeoutMs;
    requestId = 0;
    constructor(options) {
        this.endpoint = options.endpoint;
        this.token = options.token;
        this.timeoutMs = options.timeoutMs;
    }
    async callTool(name, args) {
        const id = ++this.requestId;
        const response = await this.post(buildToolCallRequest(id, name, args));
        if (response.error)
            throw new Error(`[${response.error.code ?? "?"}] ${response.error.message ?? "tool call failed"}`);
        if (response.result === undefined || response.result === null)
            throw new Error(`${name} returned no result`);
        const text = extractToolText(response.result);
        if (response.result.isError)
            throw new Error(`${name} failed: ${text}`);
        return text;
    }
    async listToolNames() {
        const id = ++this.requestId;
        const response = await this.post(buildToolsListRequest(id));
        if (response.error)
            throw new Error(`[${response.error.code ?? "?"}] ${response.error.message ?? "tools/list failed"}`);
        return extractToolNames(response.result);
    }
    async post(body) {
        const controller = new AbortController();
        const timer = setTimeout(() => controller.abort(), this.timeoutMs);
        try {
            const response = await fetch(this.endpoint, {
                method: "POST",
                headers: buildHeaders(this.token),
                body: JSON.stringify(body),
                signal: controller.signal,
            });
            if (!response.ok)
                throw new Error(`HTTP ${response.status} ${response.statusText}`);
            return (await response.json());
        }
        catch (err) {
            if (err instanceof Error && err.name === "AbortError")
                throw new Error(`request timed out after ${this.timeoutMs}ms`);
            throw err;
        }
        finally {
            clearTimeout(timer);
        }
    }
}
export function parseGuardValidate(text) {
    const start = text.indexOf("{");
    const end = text.lastIndexOf("}");
    if (start === -1 || end <= start)
        throw new Error(`guard_validate returned no JSON body: ${text.slice(0, 200)}`);
    let body;
    try {
        body = JSON.parse(text.slice(start, end + 1));
    }
    catch (err) {
        throw new Error(`guard_validate returned an unreadable body: ${String(err)}`);
    }
    const verdict = asRecord(body.verdict);
    const bucket = asString(verdict.bucket) ?? "Unknown";
    const reasons = Array.isArray(verdict.reasons) ? verdict.reasons.map((r) => String(r)) : [];
    const constraints = Array.isArray(verdict.constraints) ? verdict.constraints.map((c) => String(c)) : [];
    const notes = Array.isArray(verdict.notes) ? verdict.notes.map((n) => String(n)) : [];
    const rule_ids = Array.isArray(body.rule_ids_triggered) ? body.rule_ids_triggered.map((id) => String(id)) : [];
    const rejecting = bucket === "DeterministicReject" || bucket === "SandboxReject";
    return { verdict: rejecting ? "reject" : "pass", bucket, reasons, constraints, notes, rule_ids };
}
function initialStatus() {
    return {
        state: "uninitialized",
        endpoint: DEFAULT_ENDPOINT,
        namespace: "NeuroStrata",
        authenticated: false,
        missingTools: [],
        updatedAt: new Date(0).toISOString(),
    };
}
let status = initialStatus();
export function adapterStatus() {
    return { ...status, missingTools: [...status.missingTools] };
}
function setStatus(patch) {
    status = { ...status, ...patch, updatedAt: new Date().toISOString() };
}
export const MARKERS = [
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
export function detectTaskIntentsLocal(text) {
    const lower = text.toLowerCase();
    const out = [];
    const seen = new Set();
    for (const [matchStr, canonical] of MARKERS) {
        const idx = lower.indexOf(matchStr);
        if (idx === -1)
            continue;
        if (seen.has(canonical))
            continue;
        const clauseStart = text.slice(0, idx).lastIndexOf("\n") + 1;
        let start = clauseStart;
        const sentPos = text.slice(0, idx).lastIndexOf(". ");
        if (sentPos !== -1 && sentPos + 2 > start)
            start = sentPos + 2;
        const clausePrefix = text.slice(start, idx).trimStart().toLowerCase();
        if (NEGATION_PREFIXES.some((p) => clausePrefix.startsWith(p)))
            continue;
        const rest = text.slice(idx);
        const end = Math.min(rest.indexOf("\n") === -1 ? rest.length : rest.indexOf("\n"), 160);
        let excerpt = rest.slice(0, end).trimEnd();
        if (excerpt.length > 160)
            excerpt = excerpt.slice(0, 160);
        seen.add(canonical);
        out.push({ marker: canonical, excerpt });
    }
    return out;
}
// ──────────────────────────────────────────────────────────────────────
// Bug 3: canonical tool classification
// ──────────────────────────────────────────────────────────────────────
export const READ_FILE_TOOLS = new Set([
    "read",
    "Read",
    "read_file",
    "view",
    "cat",
]);
export const MUTATING_FILE_TOOLS = new Set([
    "write",
    "Write",
    "write_file",
    "create_file",
    "create",
    "edit",
    "Edit",
    "MultiEdit",
    "edit_file",
    "str_replace_editor",
    "patch",
    "patch_file",
    "apply_patch",
    "replace_string_in_file",
    "insert_edit_into_file",
]);
export const SHELL_TOOLS = new Set([
    "bash",
    "Bash",
    "shell",
    "local_shell",
    "exec_command",
    "run_shell_command",
    "run_command",
    "execute_command",
    "terminal",
    "gh_exec",
]);
export const MUTATING_TOOLS = new Set([...MUTATING_FILE_TOOLS, ...SHELL_TOOLS]);
export const SUBAGENT_TOOLS = new Set([
    "subagent",
    "Task",
    "task",
    "dispatch_agent",
    "spawn_agent",
    "agent",
    "delegate_task",
]);
export const CODE_EXEC_TOOLS = new Set(["execute", "mcp_execute"]);
export const TOOL_CLASSIFICATION = {
    READ_FILE_TOOLS,
    MUTATING_FILE_TOOLS,
    SHELL_TOOLS,
    MUTATING_TOOLS,
    SUBAGENT_TOOLS,
    CODE_EXEC_TOOLS,
};
export function isFileTouchTool(name) {
    return READ_FILE_TOOLS.has(name) || MUTATING_FILE_TOOLS.has(name);
}
export function isMutatingTool(name) {
    return MUTATING_TOOLS.has(name);
}
export function isSubagentTool(name) {
    return SUBAGENT_TOOLS.has(name);
}
export function isCodeExecTool(name) {
    return CODE_EXEC_TOOLS.has(name);
}
function extractBalancedCallArgs(code, startAfterOpenParen) {
    let depth = 1;
    let quote = null;
    let i = startAfterOpenParen;
    while (i < code.length && depth > 0) {
        const ch = code[i];
        if (quote) {
            if (ch === "\\") {
                i += 2;
                continue;
            }
            if (ch === quote)
                quote = null;
            i++;
            continue;
        }
        if (ch === '"' || ch === "'" || ch === "`") {
            quote = ch;
        }
        else if (ch === "(") {
            depth++;
        }
        else if (ch === ")") {
            depth--;
            if (depth === 0)
                break;
        }
        i++;
    }
    return code.slice(startAfterOpenParen, i);
}
/**
 * Inspect JS code strings passed to OpenCode V2 `execute` (Code Mode).
 * Detects neurostrata_task_claim / _update / _complete / _memories_for_file
 * calls in order and extracts id / session_id / note / path arguments from
 * each matched call's own argument slice.
 */
export function inspectCodeModeExecution(code) {
    const result = { queriedFiles: [] };
    const callRe = /\b(neurostrata_(?:task_claim|task_update|task_complete|memories_for_file|archive_memory|add_memory|supersede_memory|update_memory))\b(?:\s*["']\s*\])?\s*\(/g;
    let match;
    while ((match = callRe.exec(code)) !== null) {
        const tool = match[1];
        const argSlice = extractBalancedCallArgs(code, match.index + match[0].length);
        // Keys may be bare identifiers (`id: "t1"`) or JSON-style quoted
        // (`{"id":"t1"}`); the quoted form puts a quote between the key and the
        // colon, so the key itself has to accept optional quotes.
        const idMatch = argSlice.match(/(?:^|[^\w$])["']?id["']?\s*:\s*["']([^"']+)["']/);
        const sessionMatch = argSlice.match(/(?:^|[^\w$])["']?session_id["']?\s*:\s*["']([^"']+)["']/);
        const noteMatch = argSlice.match(/(?:^|[^\w$])["']?note["']?\s*:\s*["']([^"']*)["']/);
        const pathMatch = argSlice.match(/(?:^|[^\w$])["']?path["']?\s*:\s*["']([^"']+)["']/);
        if (tool === "neurostrata_task_claim") {
            if (idMatch && sessionMatch) {
                result.id = idMatch[1];
                result.sessionId = sessionMatch[1];
                result.taskClaimed = true;
                result.taskCompleted = false;
            }
        }
        else if (tool === "neurostrata_task_update") {
            result.taskUpdated = true;
            if (idMatch)
                result.id = idMatch[1];
            if (sessionMatch)
                result.sessionId = sessionMatch[1];
            if (noteMatch)
                result.note = noteMatch[1];
        }
        else if (tool === "neurostrata_task_complete") {
            result.taskCompleted = true;
        }
        else if (tool === "neurostrata_memories_for_file") {
            if (pathMatch)
                result.queriedFiles.push(pathMatch[1]);
        }
        else if (tool === "neurostrata_archive_memory" || tool === "neurostrata_add_memory" || tool === "neurostrata_supersede_memory" || tool === "neurostrata_update_memory") {
            result.memoryMutated = true;
        }
    }
    return result;
}
// ──────────────────────────────────────────────────────────────────────
// File-touch prefetch (LRU dedup + injected-memory dedup)
// ──────────────────────────────────────────────────────────────────────
export const PREFETCH_LRU_CAP = 128;
export const PREFETCH_MAX_CHARS = 2048;
export const prefetchedPaths = new Map();
export const injectedMemoryIds = new Set();
/**
 * Memory ids injected by a prefetch that has not been delivered yet. A denied
 * edit never delivers its recall block, so the ids it consumed have to be
 * given back -- otherwise the allowed retry finds the path and the memories
 * already marked as delivered and returns no snippets at all.
 */
const pendingPrefetchDeliveries = new Map();
function recordPrefetchedPath(key) {
    prefetchedPaths.set(key, Date.now());
    if (prefetchedPaths.size > PREFETCH_LRU_CAP) {
        const oldest = prefetchedPaths.keys().next().value;
        if (oldest !== undefined)
            prefetchedPaths.delete(oldest);
    }
}
function touchPrefetchedPath(key) {
    prefetchedPaths.delete(key);
    recordPrefetchedPath(key);
}
export function parseFileMemoryHits(text) {
    const start = text.indexOf("{");
    const end = text.lastIndexOf("}");
    if (start === -1 || end <= start)
        throw new Error(`memories_for_file returned no JSON body: ${text.slice(0, 200)}`);
    let body;
    try {
        body = JSON.parse(text.slice(start, end + 1));
    }
    catch (err) {
        throw new Error(`memories_for_file returned an unreadable body: ${String(err)}`);
    }
    const raw = Array.isArray(body.hits) ? body.hits : Array.isArray(body.memories) ? body.memories : [];
    const out = [];
    for (const item of raw) {
        if (typeof item !== "object" || item === null)
            continue;
        const hit = item;
        const id = asString(hit.id) ?? asString(hit.memory_id);
        const valid_from = asString(hit.valid_from) ?? "";
        if (!id)
            continue;
        out.push({
            id,
            valid_from,
            memory_type: asString(hit.memory_type) ?? asString(hit.type) ?? "memory",
            content: asString(hit.content) ?? asString(hit.text) ?? asString(hit.summary) ?? "",
        });
    }
    return out;
}
function formatFileMemorySnippet(hit) {
    const where = hit.valid_from ? ` (valid_from ${hit.valid_from})` : "";
    return `[NeuroStrata ${hit.memory_type} ${hit.id}${where}] ${hit.content}`;
}
function capSnippets(snippets) {
    let total = 0;
    const out = [];
    for (const snippet of snippets) {
        if (total >= PREFETCH_MAX_CHARS)
            break;
        const room = PREFETCH_MAX_CHARS - total;
        out.push(snippet.length > room ? snippet.slice(0, room) : snippet);
        total += Math.min(snippet.length, room) + 1;
    }
    return out;
}
/**
 * Record a session-query ledger entry in <projectRoot>/.NeuroStrata/session_queries.json.
 * Only writes when opts.recordLedgerDir is provided OR <projectRoot>/.NeuroStrata already
 * exists on disk, so unit tests do not dirty random directories unless requested.
 */
function recordLocalSessionQuery(namespace, tool, query, recordLedgerDir) {
    const rootDir = recordLedgerDir ? findProjectRoot(recordLedgerDir) : findProjectRoot();
    const shouldRecord = recordLedgerDir !== undefined || existsSync(join(rootDir, ".NeuroStrata"));
    if (!shouldRecord)
        return;
    // The governed prefix is the queried path itself. Widening it to the first
    // path component would claim coverage of every sibling file under that
    // directory, so the pre-commit hook would silence warnings for files that
    // were never looked up.
    const entry = { ts: Math.floor(Date.now() / 1000), tool, query, governed_prefixes: [query] };
    const ledgerPath = join(rootDir, ".NeuroStrata", "session_queries.json");
    let parsed;
    try {
        parsed = existsSync(ledgerPath) ? JSON.parse(readFileSync(ledgerPath, "utf8")) : undefined;
    }
    catch {
        parsed = undefined;
    }
    let out;
    if (Array.isArray(parsed)) {
        // Legacy top-level array: wrap it into the object form.
        out = { namespace, queries: [...parsed, entry] };
    }
    else if (parsed !== null && typeof parsed === "object" && Array.isArray(parsed.queries)) {
        const obj = parsed;
        out = { ...obj, namespace: obj.namespace || namespace, queries: [...obj.queries, entry] };
    }
    else {
        out = { namespace, queries: [entry] };
    }
    try {
        mkdirSync(dirname(ledgerPath), { recursive: true });
        writeFileSync(ledgerPath, JSON.stringify(out, null, 2));
    }
    catch {
        // best-effort; never break a file touch for a ledger write
    }
}
const subagentFileRecallCache = new Map();
function cacheSubagentFileRecall(key, entries) {
    subagentFileRecallCache.delete(key);
    subagentFileRecallCache.set(key, entries);
    if (subagentFileRecallCache.size > PREFETCH_LRU_CAP) {
        const oldest = subagentFileRecallCache.keys().next().value;
        if (oldest !== undefined)
            subagentFileRecallCache.delete(oldest);
    }
}
function formatAstEntryList(val, maxItems = 5) {
    if (!Array.isArray(val))
        return "[]";
    const arr = val.map((item) => {
        if (typeof item === "string")
            return item;
        if (typeof item === "object" && item !== null) {
            const rec = item;
            const symbol_name = asString(rec.symbol_name);
            const name = asString(rec.name);
            const file_path = asString(rec.file_path) ?? asString(rec.path);
            const id = asString(rec.id);
            if (symbol_name && file_path)
                return `${symbol_name}@${file_path}`;
            return symbol_name ?? name ?? file_path ?? id ?? String(item);
        }
        return String(item);
    });
    const capped = arr.slice(0, maxItems);
    const suffix = arr.length > maxItems ? `,+${arr.length - maxItems}` : "";
    return `[${capped.join(",")}${suffix}]`;
}
function parseRawFileRecallEntries(text, normalized) {
    const entries = [];
    let body = {};
    try {
        body = asRecord(JSON.parse(text));
    }
    catch {
        body = {};
    }
    for (const hit of parseFileMemoryHits(text)) {
        entries.push({
            dedupKey: `${hit.id}:${hit.valid_from}`,
            snippet: formatFileMemorySnippet(hit),
        });
    }
    let astDetail = "";
    if (Array.isArray(body.related_memories) && body.related_memories.length > 0) {
        astDetail = `related_memories=${body.related_memories.length}(${formatAstEntryList(body.related_memories, 5)})`;
    }
    if (body.ast_summary && typeof body.ast_summary === "object") {
        const ast = body.ast_summary;
        const parts = [];
        if (typeof ast.blast_radius_count === "number") {
            parts.push(`blast_radius_count=${ast.blast_radius_count}`);
        }
        if (ast.blast_radius !== undefined) {
            if (Array.isArray(ast.blast_radius)) {
                parts.push(`blast_radius=${formatAstEntryList(ast.blast_radius, 5)}`);
            }
            else if (typeof ast.blast_radius === "number" || typeof ast.blast_radius === "string") {
                parts.push(`blast_radius=${String(ast.blast_radius)}`);
            }
        }
        for (const key of ["symbols", "callers", "callees", "imports", "importers"]) {
            const val = ast[key];
            if (Array.isArray(val) && val.length > 0) {
                parts.push(`${key}=${formatAstEntryList(val, 5)}`);
            }
            else if (val !== undefined && !Array.isArray(val)) {
                parts.push(`${key}=${String(val)}`);
            }
        }
        if (parts.length > 0) {
            astDetail += astDetail ? ` | ast_summary={${parts.join(", ")}}` : `ast_summary={${parts.join(", ")}}`;
        }
    }
    if (astDetail) {
        entries.push({ snippet: `[NeuroStrata AST ${normalized}] ${astDetail}` });
    }
    return entries;
}
/**
 * Recall governing memories for a touched file. LRU path dedup (no re-query within the
 * window) and injectedMemoryIds dedup (each memory injected once per session).
 */
export async function prefetchMemoriesForFile(namespace, filePath, opts) {
    const normalized = filePath.trim();
    if (normalized === "")
        return [];
    const key = `${namespace}:${normalized}`;
    if (prefetchedPaths.has(key)) {
        touchPrefetchedPath(key);
        return [];
    }
    if (!mcpClient)
        return [];
    try {
        const text = await mcpClient.callTool("neurostrata_memories_for_file", {
            namespace,
            path: normalized,
            limit: 5,
        });
        recordPrefetchedPath(key);
        try {
            recordLocalSessionQuery(namespace, "neurostrata_memories_for_file", normalized, opts?.recordLedgerDir);
        }
        catch {
            // ledger is best-effort
        }
        const entries = parseRawFileRecallEntries(text, normalized);
        cacheSubagentFileRecall(key, entries);
        const out = [];
        const added = [];
        for (const entry of entries) {
            if (entry.dedupKey) {
                if (injectedMemoryIds.has(entry.dedupKey))
                    continue;
                injectedMemoryIds.add(entry.dedupKey);
                added.push(entry.dedupKey);
            }
            out.push(entry.snippet);
        }
        pendingPrefetchDeliveries.set(key, added);
        return capSnippets(out);
    }
    catch {
        return [];
    }
}
/**
 * Mark a prefetch as delivered: its memories were handed to the caller, so the
 * dedup state stays and a later touch of the same file does not re-inject
 * them.
 */
export function confirmPrefetchDelivery(namespace, filePath) {
    const normalized = filePath.trim();
    if (!normalized)
        return;
    pendingPrefetchDeliveries.delete(`${namespace}:${normalized}`);
}
/**
 * Roll back the dedup state of a prefetch whose recall was never delivered
 * (the guard denied the edit). The path is un-marked and the memory ids the
 * prefetch consumed are returned, so the allowed retry still sees the
 * governing rules.
 */
export function restorePrefetchDelivery(namespace, filePath) {
    const normalized = filePath.trim();
    if (!normalized)
        return;
    const key = `${namespace}:${normalized}`;
    const added = pendingPrefetchDeliveries.get(key);
    // Only a prefetch that is still pending was consumed but not delivered.
    // A path whose recall was already delivered keeps its dedup state.
    if (added === undefined)
        return;
    for (const id of added)
        injectedMemoryIds.delete(id);
    pendingPrefetchDeliveries.delete(key);
    prefetchedPaths.delete(key);
}
async function recallMemoriesForSubagentFile(namespace, filePath, seenInPrompt) {
    const normalized = filePath.trim();
    if (normalized === "")
        return [];
    const key = `${namespace}:${normalized}`;
    let entries = subagentFileRecallCache.get(key);
    if (!entries) {
        if (!mcpClient)
            return [];
        try {
            const text = await mcpClient.callTool("neurostrata_memories_for_file", {
                namespace,
                path: normalized,
                limit: 5,
            });
            try {
                recordLocalSessionQuery(namespace, "neurostrata_memories_for_file", normalized);
            }
            catch {
                // ledger is best-effort
            }
            entries = parseRawFileRecallEntries(text, normalized);
            cacheSubagentFileRecall(key, entries);
        }
        catch {
            return [];
        }
    }
    else {
        cacheSubagentFileRecall(key, entries);
    }
    const out = [];
    for (const entry of entries) {
        if (entry.dedupKey) {
            if (seenInPrompt.has(entry.dedupKey))
                continue;
            seenInPrompt.add(entry.dedupKey);
        }
        out.push(entry.snippet);
    }
    return capSnippets(out);
}
export function formatFileTouchRecallBlock(filePath, snippets) {
    return `[NeuroStrata Governing Memories for ${filePath}]\n${snippets.join("\n")}`;
}
// ──────────────────────────────────────────────────────────────────────
// Subagent prompt injection (Enhancement 2 & 3)
// ──────────────────────────────────────────────────────────────────────
export const SUBAGENT_CONTEXT_MARKER = "### NeuroStrata Task & Graph Context";
export function buildSubagentContextHeader(namespace, taskId, extraContext) {
    const taskLabel = taskId ?? "unclaimed (claim a task first)";
    const extra = extraContext && extraContext.trim() ? `\n${extraContext.trim()}\n` : "";
    return (`${SUBAGENT_CONTEXT_MARKER} (AUTO-INJECTED BY NEUROSTRATA PLUGIN)\n` +
        `- Namespace: \`${namespace}\` | Active Task ID: \`${taskLabel}\`\n` +
        `- Mandatory Graph-First Lookup Rule: Before editing any file, check whether its path and governing memory IDs (\`mem-...\`) are already listed under \`### NeuroStrata Task & Graph Context\` in your prompt. For any file you touch that is NOT listed in that block, call \`neurostrata_memories_for_file\` first (or \`neurostrata_search_memory\` / \`neurostrata_related\`).\n` +
        extra +
        `\n`);
}
export function injectSubagentPrompt(target, namespace, taskId, extraContext) {
    const raw = typeof target.prompt === "string" ? target.prompt : null;
    if (raw === null || raw.includes(SUBAGENT_CONTEXT_MARKER))
        return false;
    target.prompt = buildSubagentContextHeader(namespace, taskId, extraContext) + raw;
    return true;
}
export function extractRepoFilePaths(promptText) {
    const regex = /(?:[A-Za-z0-9_.-]+\/)+[A-Za-z0-9_.-]+\.[A-Za-z0-9]+/g;
    const out = [];
    const seen = new Set();
    let match;
    while ((match = regex.exec(promptText))) {
        const token = match[0];
        if (token.startsWith("http://") || token.startsWith("https://"))
            continue;
        if (!seen.has(token)) {
            seen.add(token);
            out.push(token);
        }
    }
    return out;
}
// Cached daemon subagent prompt injections keyed by active task id.
export const cachedTaskInjection = new Map();
export const cachedSubagentPromptByTask = cachedTaskInjection;
/**
 * Resolve the active task from the daemon (fail-soft). Caches activeTaskId,
 * activeSessionId and cachedTaskInjection.
 */
export async function resolveActiveTaskFromDaemon(namespace, preferredTaskId) {
    if (!mcpClient) {
        return { taskId: activeTaskId, sessionId: activeSessionId, promptInjection: null };
    }
    try {
        const text = await mcpClient.callTool("neurostrata_task_list", { namespace, status: "in_progress" });
        let body = {};
        try {
            body = asRecord(JSON.parse(text));
        }
        catch {
            body = {};
        }
        const tasks = Array.isArray(body.tasks) ? body.tasks : Array.isArray(body.items) ? body.items : [];
        let target = null;
        for (const item of tasks) {
            if (typeof item !== "object" || item === null)
                continue;
            const rec = item;
            const recId = asString(rec.id);
            if (recId) {
                cachedTaskInjection.set(recId, asString(rec.subagent_prompt_injection) ?? null);
            }
            if (preferredTaskId && recId === preferredTaskId) {
                target = rec;
                break;
            }
            if (!target && asString(rec.status) === "in_progress") {
                target = rec;
            }
        }
        if (!target && tasks.length > 0 && typeof tasks[0] === "object" && tasks[0] !== null) {
            target = tasks[0];
        }
        if (target) {
            const taskId = asString(target.id);
            const sessionId = asString(target.session_id) ??
                asString(target.claim?.session_id) ??
                null;
            const promptInjection = asString(target.subagent_prompt_injection) ?? null;
            if (taskId) {
                if (!preferredTaskId || taskId === preferredTaskId) {
                    activeTaskId = taskId;
                    activeSessionId = sessionId;
                }
                cachedTaskInjection.set(taskId, promptInjection);
            }
            if (preferredTaskId && !cachedTaskInjection.has(preferredTaskId)) {
                cachedTaskInjection.set(preferredTaskId, promptInjection);
            }
            return {
                taskId: preferredTaskId ?? taskId ?? null,
                sessionId: sessionId ?? activeSessionId,
                promptInjection: preferredTaskId ? (cachedTaskInjection.get(preferredTaskId) ?? promptInjection) : promptInjection,
            };
        }
        if (preferredTaskId && !cachedTaskInjection.has(preferredTaskId)) {
            cachedTaskInjection.set(preferredTaskId, null);
        }
    }
    catch {
        // fail-soft: daemon unreachable or "Unknown tool"
    }
    return { taskId: activeTaskId, sessionId: activeSessionId, promptInjection: null };
}
/**
 * Prefetch governing memories for repo file paths mentioned in a subagent prompt and
 * inject the daemon's subagent_prompt_injection plus any recalled snippets into the
 * target prompt. Idempotent.
 */
export async function enrichAndInjectSubagentPrompt(target, namespace, taskId) {
    if (typeof target.prompt !== "string" || target.prompt.includes(SUBAGENT_CONTEXT_MARKER)) {
        return false;
    }
    let effectiveTaskId = taskId ?? null;
    let injection = null;
    if (!effectiveTaskId) {
        const resolved = await resolveActiveTaskFromDaemon(namespace);
        effectiveTaskId = resolved.taskId;
        injection = resolved.promptInjection;
    }
    else if (!cachedTaskInjection.has(effectiveTaskId)) {
        const resolved = await resolveActiveTaskFromDaemon(namespace, effectiveTaskId);
        injection = cachedTaskInjection.get(effectiveTaskId) ?? resolved.promptInjection ?? null;
    }
    else {
        injection = cachedTaskInjection.get(effectiveTaskId) ?? null;
    }
    const extraLines = [];
    if (injection)
        extraLines.push(injection);
    const files = extractRepoFilePaths(target.prompt).slice(0, 5);
    const seenInPrompt = new Set();
    for (const fp of files) {
        try {
            const snippets = await recallMemoriesForSubagentFile(namespace, fp, seenInPrompt);
            extraLines.push(...snippets);
        }
        catch {
            // prefetch failures never block subagent injection
        }
    }
    return injectSubagentPrompt(target, namespace, effectiveTaskId, extraLines.join("\n"));
}
// ──────────────────────────────────────────────────────────────────────
// Session / task tracking state
// ──────────────────────────────────────────────────────────────────────
let config = DEFAULT_CONFIG;
let mcpClient = undefined;
let pluginCtx = null;
const TASK_MUTATION_TOOLS = new Set([
    "neurostrata_task_claim",
    "neurostrata_task_update",
    "neurostrata_task_complete",
]);
const MEMORY_MUTATION_TOOLS = new Set([
    "neurostrata_archive_memory",
    "neurostrata_add_memory",
    "neurostrata_supersede_memory",
    "neurostrata_update_memory",
]);
function trackNativeTaskToolCall(toolName, payloadArgs, outerInput) {
    if (!TASK_MUTATION_TOOLS.has(toolName) && !MEMORY_MUTATION_TOOLS.has(toolName))
        return;
    const sessionId = asString(payloadArgs.session_id) ??
        asString(outerInput?.session_id) ??
        asString(outerInput?.sessionID);
    if (sessionId)
        activeSessionId = sessionId;
    const taskId = asString(payloadArgs.id);
    if (toolName === "neurostrata_task_claim" ||
        toolName === "neurostrata_task_complete" ||
        payloadArgs.note !== undefined) {
        mutationsSinceNote = 0;
    }
    if (MEMORY_MUTATION_TOOLS.has(toolName)) {
        cachedTaskInjection.clear();
        prefetchedPaths.clear();
        injectedMemoryIds.clear();
        subagentFileRecallCache.clear();
        return;
    }
    if (toolName === "neurostrata_task_complete") {
        if (activeTaskId)
            cachedTaskInjection.delete(activeTaskId);
        activeTaskId = null;
    }
    else if (taskId) {
        if (toolName === "neurostrata_task_claim" ||
            toolName === "neurostrata_task_update" ||
            payloadArgs.title !== undefined) {
            cachedTaskInjection.delete(taskId);
            if (activeTaskId)
                cachedTaskInjection.delete(activeTaskId);
        }
        activeTaskId = taskId;
    }
}
export let activeSessionId = null;
export let activeTaskId = null;
export let mutationsSinceNote = 0;
export function resetState() {
    activeSessionId = null;
    activeTaskId = null;
    mutationsSinceNote = 0;
    cachedTaskInjection.clear();
    subagentFileRecallCache.clear();
    pendingPrefetchDeliveries.clear();
}
export function resetClientForTesting() {
    mcpClient = undefined;
    resetState();
}
export function getTrackingState() {
    return { activeTaskId, activeSessionId, mutationsSinceNote };
}
// ──────────────────────────────────────────────────────────────────────
// Bug 5: execute.before / execute.after recall delivery
// ──────────────────────────────────────────────────────────────────────
export const pendingRecallBlocks = [];
export const pendingRecallByCall = new Map();
function extractRecallCallKey(primary, secondary) {
    for (const src of [primary, secondary]) {
        if (!src)
            continue;
        const id = asString(src.callID) ??
            asString(src.call_id) ??
            asString(src.toolCallId) ??
            asString(src.tool_call_id) ??
            asString(src.id);
        if (id)
            return `call:${id}`;
    }
    return undefined;
}
function extractRecallSessionKey(primary, secondary) {
    for (const src of [primary, secondary]) {
        if (!src)
            continue;
        const sid = asString(src.sessionID) ?? asString(src.session_id);
        if (sid)
            return `session:${sid}`;
    }
    return undefined;
}
function enqueuePendingRecall(block, outerInput, innerArgs) {
    if (pendingRecallBlocks.length === 0) {
        pendingRecallByCall.clear();
    }
    pendingRecallBlocks.push(block);
    const key = extractRecallCallKey(outerInput, innerArgs) ??
        extractRecallSessionKey(outerInput, innerArgs);
    if (key) {
        pendingRecallByCall.set(key, block);
    }
}
function consumePendingRecall(meta, target) {
    if (pendingRecallBlocks.length === 0) {
        pendingRecallByCall.clear();
    }
    const afterTool = asString(meta?.tool) ??
        asString(meta?.tool_name) ??
        asString(meta?.name) ??
        asString(target?.tool) ??
        asString(target?.tool_name);
    if (afterTool && !isFileTouchTool(afterTool)) {
        return undefined;
    }
    const removeFromArray = (val) => {
        const idx = pendingRecallBlocks.indexOf(val);
        if (idx !== -1)
            pendingRecallBlocks.splice(idx, 1);
    };
    const callKey = extractRecallCallKey(meta, target);
    if (callKey) {
        const matched = pendingRecallByCall.get(callKey);
        if (matched !== undefined) {
            pendingRecallByCall.delete(callKey);
            removeFromArray(matched);
        }
        return matched;
    }
    const sessionKey = extractRecallSessionKey(meta, target);
    if (sessionKey) {
        const matched = pendingRecallByCall.get(sessionKey);
        if (matched !== undefined) {
            pendingRecallByCall.delete(sessionKey);
            removeFromArray(matched);
            return matched;
        }
        if (pendingRecallByCall.size > 0) {
            return undefined;
        }
    }
    const shifted = pendingRecallBlocks.shift();
    if (shifted !== undefined) {
        for (const [k, v] of pendingRecallByCall.entries()) {
            if (v === shifted) {
                pendingRecallByCall.delete(k);
                break;
            }
        }
    }
    return shifted;
}
function extractPatchPath(raw) {
    if (typeof raw !== "string")
        return undefined;
    const m = raw.match(/(?:^\*\*\*\s+(?:Update|Add|Delete)\s+File:\s*|^---\s+a\/|^\+\+\+\s+b\/)([^\r\n]+)/m);
    return m?.[1]?.trim();
}
function extractFilePath(args, altArgs, rawPatch) {
    return (asString(args.file_path) ??
        asString(args.filePath) ??
        asString(args.path) ??
        extractPatchPath(args.patchText ?? args.patch ?? args.diff ?? args.input) ??
        extractPatchPath(rawPatch) ??
        (altArgs
            ? (asString(altArgs.file_path) ??
                asString(altArgs.filePath) ??
                asString(altArgs.path) ??
                extractPatchPath(altArgs.patchText ?? altArgs.patch ?? altArgs.diff ?? altArgs.input))
            : undefined));
}
const executeAfterHandler = async (output, secondArg) => {
    // The host may pass (input, output) or a single `output`/`event` argument
    // whose `.output` holds the real output. Resolve metadata and target object.
    const hasSecondObject = secondArg !== undefined && secondArg !== null && typeof secondArg === "object";
    const meta = asRecord(output);
    let target = hasSecondObject ? secondArg : output;
    if (!hasSecondObject && target && typeof target === "object" && target.output !== undefined && typeof target.output === "object") {
        target = target.output;
    }
    const targetRec = asRecord(target);
    const block = consumePendingRecall(meta, targetRec) ??
        (targetRec ? targetRec.neurostrataContext : undefined) ??
        (meta ? meta.neurostrataContext : undefined);
    if (block && target && typeof target === "object") {
        const out = target;
        if (typeof out.output === "string")
            out.output = `${block}\n\n${out.output}`;
        else if (typeof out.content === "string")
            out.content = `${block}\n\n${out.content}`;
        out.additionalContext = block;
    }
    return target ?? output;
};
const executeBeforeHandler = async (input) => {
    const toolName = input?.tool ?? input?.tool_name ?? input?.name ?? input?.action ?? input?.command ?? "";
    const args = firstRecord(input?.input, input?.args, input);
    const altArgs = asRecord(input?.args);
    // Bug 3: OpenCode V2 Code Mode execution inspection.
    if (isCodeExecTool(toolName)) {
        const code = String(input?.input?.code ?? input?.args?.code ?? input?.code ?? "");
        const inspection = inspectCodeModeExecution(code);
        if (inspection.id) {
            cachedTaskInjection.delete(inspection.id);
            activeTaskId = inspection.id;
        }
        if (inspection.sessionId)
            activeSessionId = inspection.sessionId;
        if (inspection.taskClaimed || inspection.taskCompleted || inspection.note !== undefined) {
            mutationsSinceNote = 0;
        }
        if (inspection.taskCompleted) {
            if (activeTaskId)
                cachedTaskInjection.delete(activeTaskId);
            activeTaskId = null;
        }
        if (inspection.memoryMutated) {
            cachedTaskInjection.clear();
            prefetchedPaths.clear();
            injectedMemoryIds.clear();
            subagentFileRecallCache.clear();
        }
        for (const fp of inspection.queriedFiles) {
            try {
                await prefetchMemoriesForFile(config.namespace, fp);
            }
            catch {
                // prefetch never blocks execution
            }
        }
    }
    // Track native task mutation tools (claim/update/complete) and memory mutations.
    trackNativeTaskToolCall(toolName, firstRecord(args, altArgs), asRecord(input));
    // Memory mutation tools (archive/add/update) invalidate all cached prompts and file recall.
    if (MEMORY_MUTATION_TOOLS.has(toolName)) {
        cachedTaskInjection.clear();
        prefetchedPaths.clear();
        injectedMemoryIds.clear();
        subagentFileRecallCache.clear();
    }
    // Enhancement 1: resolve active task from daemon before subagent injection & mutating guard.
    if (!activeTaskId && (isSubagentTool(toolName) || isMutatingTool(toolName))) {
        try {
            await resolveActiveTaskFromDaemon(config.namespace);
        }
        catch {
            // fail-soft
        }
    }
    // Enhancement 2 & 3: subagent tool prompt enrichment.
    if (isSubagentTool(toolName)) {
        const targets = [];
        if (input?.input && typeof input.input === "object") {
            targets.push(input.input);
        }
        if (input?.args && typeof input.args === "object") {
            targets.push(input.args);
        }
        if (targets.length === 0 && input && typeof input === "object") {
            targets.push(input);
        }
        for (const target of targets) {
            try {
                await enrichAndInjectSubagentPrompt(target, config.namespace, activeTaskId);
            }
            catch {
                injectSubagentPrompt(target, config.namespace, activeTaskId);
            }
        }
    }
    // Bug 5: file-touch recall.
    let snippets = [];
    let filePath;
    if (isFileTouchTool(toolName)) {
        filePath = extractFilePath(args, altArgs, input?.input);
        if (filePath) {
            try {
                snippets = await prefetchMemoriesForFile(config.namespace, filePath);
            }
            catch {
                snippets = [];
            }
        }
    }
    let block;
    if (snippets.length > 0 && filePath) {
        block = formatFileTouchRecallBlock(filePath, snippets);
        input.additionalContext = block;
        input.neurostrataContext = block;
    }
    // Non-mutating file-touch with recalled snippets -> return the recall block.
    if (snippets.length > 0 && !isMutatingTool(toolName)) {
        if (block)
            enqueuePendingRecall(block, asRecord(input), args);
        if (filePath)
            confirmPrefetchDelivery(config.namespace, filePath);
        return { additionalContext: block, neurostrataContext: block, snippets };
    }
    // Mutating tool guard (fail closed).
    if (isMutatingTool(toolName)) {
        if (config.strictMode) {
            const call = {
                name: toolName,
                args: firstRecord(input?.input, input?.args, input),
                session_id: asString(input?.session_id) ?? asString(input?.sessionID) ?? activeSessionId ?? undefined,
            };
            const payload = JSON.stringify(call.args);
            const cwd = process.cwd();
            const session_id = call.session_id;
            const active_task_id = asString(args.active_task_id) ?? activeTaskId;
            const worktree = asString(args.worktree) ?? cwd;
            if (!mcpClient) {
                throw new Error(`Guard validation unavailable (NeuroStrata core degraded): no MCP client`);
            }
            try {
                const text = await mcpClient.callTool("neurostrata_guard_validate", {
                    namespace: config.namespace,
                    session_id,
                    active_task_id,
                    worktree,
                    action_type: toolName,
                    payload,
                    cwd,
                    is_mutating: true,
                    use_sandbox: false,
                    mutations_since_note: mutationsSinceNote,
                });
                const result = parseGuardValidate(text);
                if (result.verdict === "reject") {
                    const reason = result.reasons.join("; ") || result.constraints.join("; ") || result.bucket;
                    throw new Error(`Zero-Action Start violation: ${reason}`);
                }
            }
            catch (err) {
                const message = String(err);
                // The recall block never reaches the caller on a rejection, so the
                // prefetch dedup state is rolled back: an allowed retry must still
                // see the governing rules.
                if (filePath)
                    restorePrefetchDelivery(config.namespace, filePath);
                if (message.includes("Zero-Action Start violation:") || message.includes("Guard rejected"))
                    throw err;
                throw new Error(`Guard validation unavailable (NeuroStrata core degraded): ${message}`);
            }
        }
        mutationsSinceNote++;
    }
    if (filePath)
        confirmPrefetchDelivery(config.namespace, filePath);
    if (block) {
        enqueuePendingRecall(block, asRecord(input), args);
        return { additionalContext: block, neurostrataContext: block, snippets };
    }
    return input;
};
// ──────────────────────────────────────────────────────────────────────
// Plugin entry point
// ──────────────────────────────────────────────────────────────────────
let initializeResolveGuard = false;
export async function initialize(ctx) {
    pluginCtx = ctx;
    config = resolveConfig(ctx?.config ?? ctx?.pluginConfig ?? {});
    const token = config.bearerToken ?? loadDaemonToken(config.tokenFile);
    mcpClient = new NeuroStrataMCPClient({ endpoint: config.endpoint, token, timeoutMs: config.requestTimeoutMs });
    cachedTaskInjection.clear();
    subagentFileRecallCache.clear();
    setStatus({
        state: "uninitialized",
        endpoint: config.endpoint,
        namespace: config.namespace,
        authenticated: token !== null,
        missingTools: [],
        lastError: undefined,
    });
    const registerHook = async (name, handler) => {
        if (ctx?.tool?.hook && typeof ctx.tool.hook === "function") {
            await ctx.tool.hook(name, handler);
        }
        else if (ctx?.hooks && typeof ctx.hooks[name] === "function") {
            await ctx.hooks[name](handler);
        }
    };
    // Register execute.after FIRST, execute.before SECOND (tests overwrite registeredHook
    // on each ctx.tool.hook call and expect execute.before as the last registered hook).
    await registerHook("execute.after", executeAfterHandler);
    await registerHook("execute.before", executeBeforeHandler);
    // Claude Code style hook events.
    if (ctx?.hooks?.PreToolUse && typeof ctx.hooks.PreToolUse === "function") {
        await ctx.hooks.PreToolUse(async (event) => handleHookEvent(event));
    }
    // Fix 2: Register beforeToolExec, beforeToolCall, beforeCommand, and onTurnEnd hooks
    // (used by Codex / GitHub CLI hosts that route tool calls through these hook names).
    if (ctx?.hooks?.beforeToolExec && typeof ctx.hooks.beforeToolExec === "function") {
        await ctx.hooks.beforeToolExec(executeBeforeHandler);
    }
    if (ctx?.hooks?.beforeToolCall && typeof ctx.hooks.beforeToolCall === "function") {
        await ctx.hooks.beforeToolCall(executeBeforeHandler);
    }
    if (ctx?.hooks?.beforeCommand && typeof ctx.hooks.beforeCommand === "function") {
        await ctx.hooks.beforeCommand(executeBeforeHandler);
    }
    if (ctx?.hooks?.onTurnEnd && typeof ctx.hooks.onTurnEnd === "function") {
        await ctx.hooks.onTurnEnd(onTurnEnd);
    }
    await negotiateCapabilities();
    console.error("[NeuroStrata] plugin initialized", {
        namespace: config.namespace,
        endpoint: config.endpoint,
        authenticated: status.authenticated,
        state: status.state,
        missingTools: status.missingTools,
    });
}
async function negotiateCapabilities() {
    if (!mcpClient)
        return;
    try {
        const available = new Set(await mcpClient.listToolNames());
        const missing = REQUIRED_TOOLS.filter((t) => !available.has(t));
        setStatus({ state: missing.length > 0 ? "degraded" : "ready", missingTools: missing, lastError: undefined });
        if (missing.length > 0) {
            console.error("NeuroStrata core does not expose the tools this shim needs", { missing });
        }
    }
    catch (err) {
        setStatus({ state: "unavailable", lastError: String(err) });
        console.error("NeuroStrata core is unreachable; the shim is degraded", { error: String(err) });
    }
}
// ──────────────────────────────────────────────────────────────────────
// Bug 4: handleHookEvent (and alias handleClaudeHookEvent)
// ──────────────────────────────────────────────────────────────────────
export async function handleHookEvent(event) {
    // Stateless CLI invocations (cli-hook.mjs): auto-initialize a default client so
    // handleHookEvent works even if initialize() was never called with a full host context.
    if (!mcpClient) {
        config = resolveConfig({});
        const token = loadDaemonToken(config.tokenFile);
        mcpClient = new NeuroStrataMCPClient({ endpoint: config.endpoint, token, timeoutMs: config.requestTimeoutMs });
    }
    const hookEventName = asString(event.hook_event_name) ?? asString(event.eventName) ?? "PreToolUse";
    const toolNameRaw = asString(event.tool_name) ?? asString(event.toolName) ?? "";
    const toolInputRaw = event.tool_input ?? event.input ?? event.args ?? {};
    const toolInput = firstRecord(toolInputRaw);
    if (hookEventName === "PreToolUse") {
        // Track native task mutation tools (claim/update/complete) for state bookkeeping.
        trackNativeTaskToolCall(toolNameRaw, firstRecord(toolInput), asRecord(event));
        // Code execution: inspect OpenCode V2 Code Mode to update task/session state.
        if (isCodeExecTool(toolNameRaw)) {
            const code = String(toolInput.code ?? asRecord(toolInput.input)?.code ?? "");
            const inspection = inspectCodeModeExecution(code);
            if (inspection.id) {
                cachedTaskInjection.delete(inspection.id);
                activeTaskId = inspection.id;
            }
            if (inspection.sessionId)
                activeSessionId = inspection.sessionId;
            if (inspection.taskClaimed || inspection.taskCompleted || inspection.note !== undefined) {
                mutationsSinceNote = 0;
            }
            if (inspection.taskCompleted) {
                if (activeTaskId)
                    cachedTaskInjection.delete(activeTaskId);
                activeTaskId = null;
            }
            if (inspection.memoryMutated) {
                cachedTaskInjection.clear();
                prefetchedPaths.clear();
                injectedMemoryIds.clear();
                subagentFileRecallCache.clear();
            }
            for (const fp of inspection.queriedFiles) {
                try {
                    await prefetchMemoriesForFile(config.namespace, fp);
                }
                catch {
                    // prefetch never blocks execution
                }
            }
        }
        // Subagent tools: enrich & inject subagent prompt, return allow + updatedInput.
        if (isSubagentTool(toolNameRaw)) {
            await enrichAndInjectSubagentPrompt(toolInput, config.namespace, activeTaskId);
            return {
                continue: true,
                hookSpecificOutput: {
                    hookEventName: "PreToolUse",
                    permissionDecision: "allow",
                    updatedInput: toolInput,
                },
            };
        }
        // File-touch tools: prefetch governing memories, build recall block.
        let recallBlock;
        let recalledFilePath;
        if (isFileTouchTool(toolNameRaw)) {
            const filePath = extractFilePath(toolInput, undefined, toolInputRaw);
            if (filePath) {
                recalledFilePath = filePath;
                try {
                    const snippets = await prefetchMemoriesForFile(config.namespace, filePath);
                    if (snippets.length > 0)
                        recallBlock = formatFileTouchRecallBlock(filePath, snippets);
                }
                catch {
                    // prefetch failures are fail-soft
                }
            }
        }
        // A denial never delivers the recall block, so the prefetch dedup state
        // has to be rolled back: an allowed retry must still see the governing
        // rules. The block is attached to the denial as well, so the agent reads
        // the rules that blocked it.
        const denyWithRecall = (reason) => {
            if (recalledFilePath)
                restorePrefetchDelivery(config.namespace, recalledFilePath);
            return {
                ...(recallBlock ? { systemMessage: recallBlock } : {}),
                hookSpecificOutput: {
                    hookEventName: "PreToolUse",
                    permissionDecision: "deny",
                    permissionDecisionReason: reason,
                    ...(recallBlock ? { additionalContext: recallBlock } : {}),
                },
            };
        };
        // Mutating tools: resolve active task from daemon if unset, run guard_validate,
        // deny on reject or on guard-unavailable errors.
        if (isMutatingTool(toolNameRaw)) {
            if (!activeTaskId) {
                try {
                    await resolveActiveTaskFromDaemon(config.namespace);
                }
                catch {
                    // fail-soft
                }
            }
            if (config.strictMode) {
                const callArgs = firstRecord(toolInput);
                const payload = JSON.stringify(callArgs);
                const cwd = process.cwd();
                const session_id = asString(callArgs.session_id) ?? asString(event.session_id) ?? activeSessionId ?? undefined;
                const active_task_id = asString(callArgs.active_task_id) ?? activeTaskId;
                const worktree = asString(callArgs.worktree) ?? cwd;
                try {
                    if (!mcpClient)
                        throw new Error("no MCP client");
                    const text = await mcpClient.callTool("neurostrata_guard_validate", {
                        namespace: config.namespace,
                        session_id,
                        active_task_id,
                        worktree,
                        action_type: toolNameRaw,
                        payload,
                        cwd,
                        is_mutating: true,
                        use_sandbox: false,
                        mutations_since_note: mutationsSinceNote,
                    });
                    const result = parseGuardValidate(text);
                    if (result.verdict === "reject") {
                        const reason = result.reasons.join("; ") || result.constraints.join("; ") || result.bucket;
                        return denyWithRecall(reason);
                    }
                }
                catch (err) {
                    const message = String(err);
                    if (message.includes("Zero-Action Start violation:") ||
                        message.includes("Guard rejected")) {
                        return denyWithRecall(message);
                    }
                    return denyWithRecall(`Guard unavailable: ${message}`);
                }
            }
            mutationsSinceNote++;
        }
        if (recalledFilePath)
            confirmPrefetchDelivery(config.namespace, recalledFilePath);
        return {
            continue: true,
            ...(recallBlock ? { systemMessage: recallBlock } : {}),
            hookSpecificOutput: {
                hookEventName: "PreToolUse",
                permissionDecision: "allow",
                updatedInput: toolInput,
                ...(recallBlock ? { additionalContext: recallBlock } : {}),
            },
        };
    }
    if (["Stop", "SubagentStop"].includes(hookEventName)) {
        const transcript = asString(event.transcript) ??
            asString(event.content) ??
            asString(event.last_assistant_message);
        if (hookEventName === "Stop" && transcript) {
            await onTurnEnd({ role: "assistant", content: transcript });
        }
        return { hookSpecificOutput: { hookEventName, permissionDecision: "allow" } };
    }
    return { hookSpecificOutput: { hookEventName, permissionDecision: "allow" } };
}
export const handleClaudeHookEvent = handleHookEvent;
// ──────────────────────────────────────────────────────────────────────
// Turn hook + context plan
// ──────────────────────────────────────────────────────────────────────
export async function onTurnEnd(turn) {
    if (!config.intentDetection.enabled)
        return;
    const text = turn.content;
    if (text.includes("neurostrata_task_create"))
        return;
    const signals = detectTaskIntentsLocal(text);
    if (signals.length === 0)
        return;
    if (!config.intentDetection.injectReminder)
        return;
    const markerList = signals.map((s) => s.marker).join(", ");
    console.error(`[NeuroStrata] Task intent detected (${markerList}) but no neurostrata_task_create in this turn. Consider creating a task to track this work.`);
}
export async function fetchContextPlan(opts) {
    if (!mcpClient)
        throw new Error("NeuroStrata MCP client not initialized");
    return await mcpClient.callTool("neurostrata_context_plan", opts);
}
// ──────────────────────────────────────────────────────────────────────
// Adapter factory
// ──────────────────────────────────────────────────────────────────────
export function createHarnessPlugin(pluginId = "neurostrata", harnessName = "OpenCode") {
    return {
        id: pluginId,
        name: harnessName,
        setup: initialize,
        initialize,
        handleHookEvent,
        handleClaudeHookEvent,
        onTurnEnd,
        fetchContextPlan,
        adapterStatus,
    };
}
export const plugin = createHarnessPlugin("neurostrata-codex", "Codex");
export default plugin;
