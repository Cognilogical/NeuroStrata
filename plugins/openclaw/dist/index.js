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
import { existsSync, lstatSync, readFileSync, realpathSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import { basename, dirname, join, relative, resolve } from 'node:path';
// ──────────────────────────────────────────────────────────────────────
// Configuration
// ──────────────────────────────────────────────────────────────────────
/** Where the daemon serves MCP. The setup default is 34343, not 8080. */
export const DEFAULT_ENDPOINT = 'http://127.0.0.1:34343/mcp';
/** The daemon's bearer token, as the daemon itself lays it down. */
export const DEFAULT_TOKEN_FILE = 'daemon.token';
/** The tools this shim needs from the core to do its job. */
export const REQUIRED_TOOLS = [
    'neurostrata_guard_validate',
    'neurostrata_task_create'
];
const DEFAULT_CONFIG = {
    namespace: 'NeuroStrata',
    endpoint: DEFAULT_ENDPOINT,
    bearerToken: null,
    tokenFile: null,
    strictMode: true,
    requestTimeoutMs: 5000,
    intentDetection: { enabled: true, injectReminder: true }
};
function asRecord(value) {
    return typeof value === 'object' && value !== null
        ? value
        : {};
}
function asString(value) {
    return typeof value === 'string' && value.trim() !== ''
        ? value.trim()
        : undefined;
}
function asNumber(value) {
    return typeof value === 'number' && Number.isFinite(value) && value > 0
        ? value
        : undefined;
}
function asBoolean(value) {
    return typeof value === 'boolean' ? value : undefined;
}
/**
 * Merge the host config over the defaults.
 *
 * `guardEndpoint` is still honored: it is what older installs wrote, and
 * silently ignoring it would point the shim at the wrong port.
 */
export function resolveConfig(raw, cwd = process.cwd()) {
    const input = asRecord(raw);
    const intent = asRecord(input.intentDetection);
    const endpoint = asString(input.endpoint) ??
        asString(input.guardEndpoint) ??
        DEFAULT_CONFIG.endpoint;
    // Preserve the exact namespace for MCP routing; sanitization happens only
    // where the value is interpolated into a prompt header (#329 review).
    // Same order as the core plugin and `neurostrata-mcp namespace resolve`:
    // project marker, harness config, NEUROSTRATA_NAMESPACE, discovery.
    const namespace = findRootMarker(resolve(cwd))?.namespace ??
        asString(input.namespace) ??
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
            injectReminder: asBoolean(intent.injectReminder) ??
                DEFAULT_CONFIG.intentDetection.injectReminder
        }
    };
}
/** Marker files already warned about in this process: one line each. */
const warnedMarkers = new Set();
/**
 * Says on stderr, once per file, that the project marker at `path` cannot be
 * read and is skipped: the namespace then comes from the other sources, as
 * `neurostrata-mcp` does with its own warning (design section 5).
 */
function warnUnreadableMarker(path, reason) {
    if (warnedMarkers.has(path))
        return;
    warnedMarkers.add(path);
    console.error(`[NeuroStrata] warning: the project marker ${path} could not be read (${reason}); it is skipped`);
}
/**
 * The raw spelling of every top-level `version` value in a JSON object text
 * that already parsed. JSON.parse reads `1.0` as 1, but the daemon's reader
 * (an unsigned integer) refuses it, so the spelling is checked here.
 */
function topLevelVersionSpellings(text) {
    const found = [];
    let depth = 0;
    let i = 0;
    while (i < text.length) {
        const c = text[i];
        if (c === '"') {
            let j = i + 1;
            while (j < text.length && text[j] !== '"')
                j += text[j] === '\\' ? 2 : 1;
            const literal = text.slice(i, j + 1);
            i = j + 1;
            if (depth !== 1)
                continue;
            let k = i;
            while (k < text.length && /\s/.test(text[k]))
                k++;
            if (text[k] !== ':' || JSON.parse(literal) !== 'version')
                continue;
            k++;
            while (k < text.length && /\s/.test(text[k]))
                k++;
            const number = /^-?[0-9][0-9.eE+-]*/.exec(text.slice(k));
            found.push(number ? number[0] : text[k] ?? '');
            i = k;
            continue;
        }
        if (c === '{' || c === '[')
            depth++;
        else if (c === '}' || c === ']')
            depth--;
        i++;
    }
    return found;
}
const NAMESPACE_PATTERN = /^[A-Za-z0-9][A-Za-z0-9_.:-]*$/;
/**
 * A namespace value as written, or undefined when it is not one: empty, an
 * unexpanded variable, or anything but `[A-Za-z0-9_.:-]` starting with a
 * letter or digit. The Rust installer's `usable_namespace` rule.
 */
export function usableNamespace(raw) {
    if (typeof raw !== 'string')
        return undefined;
    const ns = raw.trim().replace(/^["']+|["']+$/g, '');
    return NAMESPACE_PATTERN.test(ns) ? ns : undefined;
}
/** Absolute on this host: a drive or UNC path on Windows, `/` elsewhere. */
function isHostAbsolute(p) {
    if (process.platform === 'win32') {
        return /^[A-Za-z]:[\\/]/.test(p) || /^[\\/]{2}[^\\/]/.test(p);
    }
    return p.startsWith('/');
}
/** Absolute on any host, for paths git wrote into `.git` and `commondir`. */
function isAbsoluteAny(p) {
    return p.startsWith('/') || p.startsWith('\\') || /^[A-Za-z]:[\\/]/.test(p);
}
/** The canonical spelling of an existing path, as Rust's `dunce` gives it. */
function canonicalPath(p) {
    try {
        const real = realpathSync.native(p);
        // Drop the verbatim prefix where a plain spelling exists.
        if (real.startsWith('\\\\?\\UNC\\'))
            return '\\\\' + real.slice(8);
        if (real.startsWith('\\\\?\\'))
            return real.slice(4);
        return real;
    }
    catch {
        return null;
    }
}
/** `p` canonicalized through its nearest existing ancestor. */
function canonicalThroughExisting(p) {
    const missing = [];
    let existing = p;
    while (true) {
        const canonical = canonicalPath(existing);
        if (canonical !== null) {
            return missing.reduceRight((acc, part) => join(acc, part), canonical);
        }
        const parent = dirname(existing);
        if (parent === existing)
            return p;
        missing.push(basename(existing));
        existing = parent;
    }
}
function isDirectory(p) {
    try {
        return statSync(p).isDirectory();
    }
    catch {
        return false;
    }
}
function isFile(p) {
    try {
        return statSync(p).isFile();
    }
    catch {
        return false;
    }
}
function ancestorsOf(dir) {
    const out = [];
    let current = dir;
    while (true) {
        out.push(current);
        const next = dirname(current);
        if (next === current)
            return out;
        current = next;
    }
}
function pathParts(rel) {
    return rel.split(/[\\/]/).filter((part) => part !== '' && part !== '.');
}
/** The nearest folder at or above `dir` holding `.git` (folder or file). */
function checkoutTop(dir) {
    return ancestorsOf(dir).find((d) => existsSync(join(d, '.git'))) ?? null;
}
/** `rel` against `base` unless it is already absolute. */
function joinedGitPath(base, rel) {
    return isAbsoluteAny(rel) ? rel : join(base, rel);
}
/** The gitdir a checkout's `.git` FILE names (relative to the checkout). */
function gitdirOfFile(checkout) {
    let text;
    try {
        text = readFileSync(join(checkout, '.git'), 'utf8');
    }
    catch {
        return null;
    }
    for (const line of text.split(/\r?\n/)) {
        const trimmed = line.trim();
        if (trimmed.startsWith('gitdir:')) {
            return joinedGitPath(checkout, trimmed.slice('gitdir:'.length).trim());
        }
    }
    return null;
}
/** The common directory a linked worktree's gitdir names in `commondir`. */
function linkedCommonDir(gitdir) {
    try {
        const common = readFileSync(join(gitdir, 'commondir'), 'utf8').trim();
        return joinedGitPath(gitdir, common);
    }
    catch {
        return null;
    }
}
/**
 * The main checkout when `top` is a linked worktree's top (its gitdir names
 * a `commondir` that is a checkout's `.git`), else null. A submodule (no
 * `commondir`) or a bare repository's worktree (common dir not `.git`) has
 * none.
 */
function linkedMainCheckout(top) {
    if (isDirectory(join(top, '.git')))
        return null;
    const gitdir = gitdirOfFile(top);
    const common = gitdir === null ? null : linkedCommonDir(gitdir);
    if (common === null)
        return null;
    const normalized = resolve(common);
    if (basename(normalized) !== '.git')
        return null;
    const main = canonicalPath(dirname(normalized));
    if (main === null)
        return null;
    const own = canonicalPath(top) ?? top;
    return main === own ? null : main;
}
/** The marker in `dir`: valid, unreadable, or null when there is none. */
function readRootMarker(dir) {
    const path = join(dir, '.NeuroStrata', 'config.json');
    const unreadable = (reason) => ({ valid: false, path, reason });
    let text;
    try {
        text = readFileSync(path, 'utf8');
    }
    catch (err) {
        return err?.code === 'ENOENT'
            ? null
            : unreadable(String(err?.message ?? err));
    }
    if (text.charCodeAt(0) === 0xfeff)
        text = text.slice(1);
    let parsed;
    try {
        parsed = JSON.parse(text);
    }
    catch {
        return unreadable('it is not valid JSON');
    }
    if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
        return unreadable('it is not a JSON object');
    }
    const doc = parsed;
    const sourceId = doc.source_id;
    const versions = topLevelVersionSpellings(text);
    if (doc.version !== 1 || versions.length !== 1 || versions[0] !== '1') {
        return unreadable('its version is not 1');
    }
    if (typeof doc.namespace !== 'string' || !NAMESPACE_PATTERN.test(doc.namespace)) {
        return unreadable('its namespace is missing or not a namespace');
    }
    if (sourceId !== undefined && sourceId !== null && typeof sourceId !== 'string') {
        return unreadable('its source_id is not a string');
    }
    return { valid: true, namespace: doc.namespace };
}
/**
 * The marker in `dir` when there is one to report. A `.NeuroStrata` that is
 * a link pointing elsewhere, or a folder without `config.json`, is none.
 */
function markerAt(dir) {
    const folder = join(dir, '.NeuroStrata');
    try {
        const st = lstatSync(folder);
        if (st.isSymbolicLink() || !st.isDirectory())
            return null;
    }
    catch {
        return null;
    }
    const realFolder = canonicalPath(folder);
    const realDir = canonicalPath(dir);
    if (realFolder === null || realDir === null)
        return null;
    if (realFolder !== join(realDir, '.NeuroStrata'))
        return null;
    return readRootMarker(dir);
}
function depthBelow(dir, top) {
    return pathParts(relative(top, dir)).length;
}
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
export function findRootMarker(startDir) {
    if (typeof startDir !== 'string' || !isHostAbsolute(startDir))
        return null;
    let start = canonicalThroughExisting(startDir);
    if (isFile(start))
        start = dirname(start);
    const decide = (root, read, via) => {
        if (read.valid)
            return { root, namespace: read.namespace, via };
        warnUnreadableMarker(read.path, read.reason);
        return null;
    };
    const top = checkoutTop(start);
    if (top === null) {
        for (const dir of ancestorsOf(start)) {
            const read = markerAt(dir);
            if (read)
                return decide(dir, read, 'nogit');
        }
        return null;
    }
    let own = null;
    for (const dir of ancestorsOf(start)) {
        const read = markerAt(dir);
        if (read) {
            own = { depth: depthBelow(dir, top), root: dir, read };
            break;
        }
        if (dir === top)
            break;
    }
    let main = null;
    const mainTop = linkedMainCheckout(top);
    if (mainTop !== null) {
        const mainCanonical = canonicalPath(mainTop) ?? mainTop;
        const insideMain = (p) => {
            const rel = relative(mainCanonical, p);
            const parts = pathParts(rel);
            return parts.length === 0 || (parts[0] !== '..' && !isAbsoluteAny(rel));
        };
        const allowed = [mainTop];
        let dir = mainTop;
        for (const part of pathParts(relative(top, start))) {
            dir = join(dir, part);
            let real = false;
            try {
                const st = lstatSync(dir);
                real = st.isDirectory() && !st.isSymbolicLink();
            }
            catch { }
            const canonical = canonicalPath(dir);
            // A link in the main checkout may point into another repository; its
            // markers are not this worktree's. Stop at links and at any folder
            // whose canonical form leaves the main checkout.
            if (!real ||
                existsSync(join(dir, '.git')) ||
                canonical === null ||
                !insideMain(canonical))
                break;
            allowed.push(dir);
        }
        for (let i = allowed.length - 1; i >= 0; i--) {
            const read = markerAt(allowed[i]);
            if (read) {
                const below = relative(mainTop, allowed[i]);
                main = {
                    depth: depthBelow(allowed[i], mainTop),
                    root: below === '' ? top : join(top, below),
                    read
                };
                break;
            }
        }
    }
    if (own && main) {
        return main.depth > own.depth
            ? decide(main.root, main.read, 'main')
            : decide(own.root, own.read, 'own');
    }
    if (own)
        return decide(own.root, own.read, 'own');
    if (main)
        return decide(main.root, main.read, 'main');
    return null;
}
/** Each harness's plugin config, relative to the project root, and the
 * namespace's place inside it. The installer writes these files. */
const HARNESS_CONFIG_FILES = {
    claude: { file: ['.NeuroStrata', 'plugins', 'claude', 'neurostrata.json'], key: ['namespace'] },
    opencode: { file: ['.opencode', 'neurostrata-plugin.json'], key: ['neurostrata', 'namespace'] },
    codex: { file: ['.codex', 'plugins', 'neurostrata.json'], key: ['namespace'] },
    hermes: { file: ['.hermes', 'plugins', 'neurostrata.json'], key: ['namespace'] },
    openclaw: { file: ['.openclaw', 'plugins', 'neurostrata.json'], key: ['namespace'] },
    'github-cli': {
        file: ['.NeuroStrata', 'plugins', 'github-cli', 'copilot-extension.json'],
        key: ['namespace']
    }
};
/**
 * The namespace `harness`'s plugin config names for the project at
 * `projectDir`, read from its checkout's top, then (for a linked worktree,
 * whose untracked configs usually live only in the main checkout) the main
 * checkout's; outside git from `projectDir` itself. Empty when none names a
 * usable namespace.
 *
 * `runnerDir` is the folder of the hook runner the installer wrote,
 * `<root>/.NeuroStrata/plugins/<harness>/`: the config of the project that
 * installed it, `<root>`, is read first.
 */
export function readHarnessConfig(harness, projectDir, runnerDir) {
    const spec = HARNESS_CONFIG_FILES[harness];
    if (!spec)
        return {};
    const roots = [];
    if (typeof runnerDir === 'string' && runnerDir !== '') {
        const plugins = dirname(resolve(runnerDir));
        const stateDir = dirname(plugins);
        if (basename(resolve(runnerDir)) === harness &&
            basename(plugins) === 'plugins' &&
            basename(stateDir) === '.NeuroStrata') {
            roots.push(dirname(stateDir));
        }
    }
    if (typeof projectDir === 'string' && projectDir !== '') {
        const start = canonicalThroughExisting(resolve(projectDir));
        const top = checkoutTop(start);
        roots.push(...(top === null ? [start] : [top, linkedMainCheckout(top)]));
    }
    for (const root of roots) {
        if (root === null)
            continue;
        try {
            let value = JSON.parse(readFileSync(join(root, ...spec.file), 'utf8').replace(/^\uFEFF/, ''));
            for (const key of spec.key)
                value = asRecord(value)[key];
            const namespace = usableNamespace(value);
            if (namespace)
                return { namespace };
        }
        catch {
            // missing or malformed: try the next root
        }
    }
    return {};
}
/**
 * Walk up from resolve(startDir) to the filesystem root looking for `.git`
 * (dir or file) or `.NeuroStrata`; return the directory containing it, or
 * resolve(startDir) if none found.
 */
export function findProjectRoot(startDir = process.cwd()) {
    // Pass 1: walk up looking for `.git` first; a `.git` anchors the project root.
    let current = resolve(startDir);
    while (true) {
        if (existsSync(join(current, '.git'))) {
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
        if (existsSync(join(current, '.NeuroStrata'))) {
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
        const sessionQueriesPath = join(current, '.NeuroStrata', 'session_queries.json');
        try {
            if (existsSync(sessionQueriesPath)) {
                const content = readFileSync(sessionQueriesPath, 'utf8');
                const parsed = JSON.parse(content);
                if (parsed !== null &&
                    typeof parsed === 'object' &&
                    typeof parsed.namespace === 'string' &&
                    parsed.namespace.trim() !== '') {
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
    return basename(root) || 'default';
}
// ──────────────────────────────────────────────────────────────────────
// Credential
// ──────────────────────────────────────────────────────────────────────
/** The absolute path of the daemon token file. */
export function daemonTokenPath(tokenFile) {
    if (tokenFile && tokenFile.trim() !== '')
        return tokenFile.trim();
    return join(homedir(), '.config', 'neurostrata', DEFAULT_TOKEN_FILE);
}
/**
 * Read the daemon's bearer token.
 *
 * Returns `null` when there is no readable token: the daemon generates one
 * on first start, so an absent file means "the daemon has never run", not
 * "authentication is optional". The caller must report that as degraded
 * rather than pretend the request is anonymous.
 */
export function loadDaemonToken(tokenFile) {
    try {
        const raw = readFileSync(daemonTokenPath(tokenFile), 'utf8').trim();
        return raw.length > 0 ? raw : null;
    }
    catch {
        return null;
    }
}
/** The request headers, with the bearer attached when there is one. */
export function buildHeaders(token) {
    const headers = { 'Content-Type': 'application/json' };
    if (token)
        headers.Authorization = `Bearer ${token}`;
    return headers;
}
/** Build the `tools/call` request the core actually answers. */
export function buildToolCallRequest(id, name, args) {
    return {
        jsonrpc: '2.0',
        id,
        method: 'tools/call',
        params: { name, arguments: args }
    };
}
/** Build the `tools/list` request used for capability negotiation. */
export function buildToolsListRequest(id) {
    return { jsonrpc: '2.0', id, method: 'tools/list', params: {} };
}
/**
 * Unwrap `result.content[].text`.
 *
 * The core answers a tool call with a content array, not with the typed
 * result a tool would return. An answer with no text is a broken answer:
 * it is reported, never treated as an empty success.
 */
export function extractToolText(result) {
    const content = result?.content;
    if (!Array.isArray(content)) {
        throw new Error('tool result carried no content array');
    }
    const text = content
        .map((part) => (typeof part?.text === 'string' ? part.text : ''))
        .filter((part) => part.length > 0)
        .join('\n');
    if (text.length === 0) {
        throw new Error('tool result carried no text content');
    }
    return text;
}
/** The tool names from a `tools/list` answer. */
export function extractToolNames(result) {
    const tools = result
        ?.tools;
    if (!Array.isArray(tools)) {
        throw new Error('tools/list returned no tools array');
    }
    return tools
        .map((tool) => (typeof tool?.name === 'string' ? tool.name : ''))
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
    /** Call one tool and return its text content. */
    async callTool(name, args) {
        const id = ++this.requestId;
        const response = await this.post(buildToolCallRequest(id, name, args));
        if (response.error) {
            throw new Error(`[${response.error.code ?? '?'}] ${response.error.message ?? 'tool call failed'}`);
        }
        if (response.result === undefined || response.result === null) {
            throw new Error(`${name} returned no result`);
        }
        const text = extractToolText(response.result);
        const isError = response.result.isError;
        if (isError) {
            throw new Error(`${name} failed: ${text}`);
        }
        return text;
    }
    /** The names of the tools the core exposes. */
    async listToolNames() {
        const id = ++this.requestId;
        const response = await this.post(buildToolsListRequest(id));
        if (response.error) {
            throw new Error(`[${response.error.code ?? '?'}] ${response.error.message ?? 'tools/list failed'}`);
        }
        return extractToolNames(response.result);
    }
    async post(body) {
        const controller = new AbortController();
        const timer = setTimeout(() => controller.abort(), this.timeoutMs);
        try {
            const response = await fetch(this.endpoint, {
                method: 'POST',
                headers: buildHeaders(this.token),
                body: JSON.stringify(body),
                signal: controller.signal
            });
            if (!response.ok) {
                throw new Error(`HTTP ${response.status} ${response.statusText}`);
            }
            return (await response.json());
        }
        catch (err) {
            if (err instanceof Error && err.name === 'AbortError') {
                throw new Error(`request timed out after ${this.timeoutMs}ms`);
            }
            throw err;
        }
        finally {
            clearTimeout(timer);
        }
    }
}
// ──────────────────────────────────────────────────────────────────────
// Guard verdict
// ──────────────────────────────────────────────────────────────────────
/**
 * Read the guard's answer.
 *
 * The core prefixes the body with `namespace:` and `trace_id:` lines, so the
 * JSON body is what lies between the first `{` and the last `}`. The verdict
 * is tagged with `bucket`, not `type`.
 */
export function parseGuardValidate(text) {
    const start = text.indexOf('{');
    const end = text.lastIndexOf('}');
    if (start === -1 || end <= start) {
        throw new Error(`guard_validate returned no JSON body: ${text.slice(0, 200)}`);
    }
    let body;
    try {
        body = JSON.parse(text.slice(start, end + 1));
    }
    catch (err) {
        throw new Error(`guard_validate returned an unreadable body: ${String(err)}`);
    }
    const verdict = asRecord(body.verdict);
    const bucket = asString(verdict.bucket) ?? 'Unknown';
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
    const rejecting = bucket === 'DeterministicReject' || bucket === 'SandboxReject';
    return {
        verdict: rejecting ? 'reject' : 'pass',
        bucket,
        reasons,
        constraints,
        notes,
        rule_ids
    };
}
function initialStatus() {
    return {
        state: 'uninitialized',
        endpoint: DEFAULT_ENDPOINT,
        namespace: 'NeuroStrata',
        authenticated: false,
        missingTools: [],
        updatedAt: new Date(0).toISOString()
    };
}
let status = initialStatus();
/**
 * Session/task tracking between turn content and guard validation.
 *
 * OpenClaw exposes no tool-call hook, so task mutations are observed
 * best-effort from turn content mentioning `neurostrata_task_claim`,
 * `neurostrata_task_update` or `neurostrata_task_complete`, with the
 * `id` / `session_id` parameters the payload carries.
 */
let activeSessionId = null;
let activeTaskId = null;
/**
 * Mutations since the heavyweight channels were last touched (a note on the
 * active task, a claim, or a completion). The guard consumes this through
 * `neurostrata_guard_validate`'s `mutations_since_note` argument to pin the
 * Breath prompt.
 */
let mutationsSinceNote = 0;
/** Reset the tracked session/task state (test hook). */
export function resetState() {
    activeSessionId = null;
    activeTaskId = null;
    mutationsSinceNote = 0;
}
/**
 * Read the `id` / `session_id` a task-mutation payload carries from turn
 * content. Handles both JSON (`"id": "x"`) and call-ish (`id = "x"`)
 * spellings; both `session_id` and camelCase `sessionID` are accepted.
 */
function extractTrackingFromText(text) {
    const id = /(?<![\w-])id["'\s]*[:=]\s*["']([^"']+)["']/.exec(text)?.[1];
    const sessionId = /session[_\s-]?id["'\s]*[:=]\s*["']([^"']+)["']/i.exec(text)?.[1] ??
        undefined;
    return { taskId: id, sessionId };
}
/** Update the tracked session/task state from a turn, best-effort. */
function trackFromTurn(turn) {
    const lower = turn.content.toLowerCase();
    const isClaimOrComplete = lower.includes('neurostrata_task_claim') ||
        lower.includes('neurostrata_task_update') ||
        lower.includes('neurostrata_task_complete');
    if (!isClaimOrComplete)
        return;
    const { taskId, sessionId } = extractTrackingFromText(turn.content);
    if (sessionId)
        activeSessionId = sessionId;
    // Heavyweight turn: a note, claim, or completion satisfies the Breath
    // prompt, so the mutation counter starts over. Best-effort: the turn text
    // is the only source openclaw exposes, so a bare "note" mention counts.
    if (lower.includes('neurostrata_task_claim') ||
        lower.includes('neurostrata_task_complete') ||
        (lower.includes('neurostrata_task_update') && lower.includes('note'))) {
        mutationsSinceNote = 0;
    }
    if (lower.includes('neurostrata_task_complete')) {
        activeTaskId = null;
    }
    else if (taskId) {
        activeTaskId = taskId;
    }
}
/** The adapter's current state. A degraded core is visible here. */
export function adapterStatus() {
    return { ...status, missingTools: [...status.missingTools] };
}
function setStatus(patch) {
    status = { ...status, ...patch, updatedAt: new Date().toISOString() };
}
// ──────────────────────────────────────────────────────────────────────
// Intent Detection (local — mirrors src/task/intent.rs)
// ──────────────────────────────────────────────────────────────────────
export const MARKERS = [
    ['todo:', 'todo:'],
    ['fixme:', 'fixme:'],
    ['follow-up:', 'follow-up:'],
    ['followup:', 'followup:'],
    ['next step:', 'next step:'],
    ['action item:', 'action item:'],
    ['remaining work:', 'remaining work:'],
    ['we should also ', 'we should also'],
    ['later we need to ', 'later we need to'],
    ['need to follow up', 'need to follow up'],
    ['still need to ', 'still need to'],
    ['must also fix ', 'must also fix'],
    ['filed follow-up', 'filed follow-up'],
    ['defer ', 'defer'],
    ['deferred ', 'deferred']
];
export const NEGATION_PREFIXES = ['never ', 'do not ', "don't ", 'do not, '];
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
        // negation guard
        const clauseStart = text.slice(0, idx).lastIndexOf('\n') + 1;
        let start = clauseStart;
        const sentPos = text.slice(0, idx).lastIndexOf('. ');
        if (sentPos !== -1 && sentPos + 2 > start)
            start = sentPos + 2;
        const clausePrefix = text.slice(start, idx).trimStart().toLowerCase();
        if (NEGATION_PREFIXES.some((p) => clausePrefix.startsWith(p)))
            continue;
        // excerpt
        const from = idx;
        const rest = text.slice(from);
        const end = Math.min(rest.indexOf('\n') === -1 ? rest.length : rest.indexOf('\n'), 160);
        let excerpt = rest.slice(0, end).trimEnd();
        if (excerpt.length > 160)
            excerpt = excerpt.slice(0, 160);
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
export const prefetchedPaths = new Map();
/** `(memory_id:valid_from)` keys already injected this session. */
export const injectedMemoryIds = new Set();
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
/**
 * Read the `memories_for_file` answer: the array arrives as `hits` or
 * `memories` between the first `{` and the last `}` of the content text.
 */
export function parseFileMemoryHits(text) {
    const start = text.indexOf('{');
    const end = text.lastIndexOf('}');
    if (start === -1 || end <= start) {
        throw new Error(`memories_for_file returned no JSON body: ${text.slice(0, 200)}`);
    }
    let body;
    try {
        body = JSON.parse(text.slice(start, end + 1));
    }
    catch (err) {
        throw new Error(`memories_for_file returned an unreadable body: ${String(err)}`);
    }
    const raw = Array.isArray(body.hits)
        ? body.hits
        : Array.isArray(body.memories)
            ? body.memories
            : [];
    const out = [];
    for (const item of raw) {
        if (typeof item !== 'object' || item === null)
            continue;
        const hit = item;
        const id = asString(hit.id) ?? asString(hit.memory_id);
        const valid_from = asString(hit.valid_from) ?? '';
        if (!id)
            continue; // a hit without an id cannot be deduped — never inject
        out.push({
            id,
            valid_from,
            memory_type: asString(hit.memory_type) ?? asString(hit.type) ?? 'memory',
            content: asString(hit.content) ??
                asString(hit.text) ??
                asString(hit.summary) ??
                ''
        });
    }
    return out;
}
function formatFileMemorySnippet(hit) {
    const where = hit.valid_from ? ` (valid_from ${hit.valid_from})` : '';
    return `[NeuroStrata ${hit.memory_type} ${hit.id}${where}] ${hit.content}`;
}
/** Cap the recalled snippets so one file touch can never flood context. */
function capSnippets(snippets) {
    let total = 0;
    const out = [];
    for (const snippet of snippets) {
        if (total >= PREFETCH_MAX_CHARS)
            break;
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
export async function prefetchMemoriesForFile(namespace, filePath) {
    const path = filePath.trim();
    if (path === '')
        return [];
    const key = `${namespace}:${path}`;
    if (prefetchedPaths.has(key)) {
        touchPrefetchedPath(key);
        return [];
    }
    // Record the LRU entry only after the recall actually succeeded: a
    // disconnected or failed call must never permanently suppress prefetch
    // for this file within the LRU window.
    if (!mcpClient)
        return [];
    const text = await mcpClient.callTool('neurostrata_memories_for_file', {
        namespace,
        path,
        limit: 5
    });
    recordPrefetchedPath(key);
    const out = [];
    for (const hit of parseFileMemoryHits(text)) {
        const dedupKey = `${hit.id}:${hit.valid_from}`;
        if (injectedMemoryIds.has(dedupKey))
            continue;
        injectedMemoryIds.add(dedupKey);
        out.push(formatFileMemorySnippet(hit));
    }
    return capSnippets(out);
}
export const SUBAGENT_CONTEXT_MARKER = '### NeuroStrata Task & Graph Context';
export function sanitizeSubagentHeaderToken(value) {
    return String(value ?? '').replace(/[^A-Za-z0-9_.:-]/g, '');
}
/** Collapse backticks and newlines so a caller-supplied value cannot break
 *  out of the header's code span (issue #316: the namespace can arrive from
 *  a committed .NeuroStrata/session_queries.json). */
function codeSpanValue(value) {
    return String(value ?? '').replace(/[`\r\n\u2028\u2029]+/g, ' ');
}
export function buildSubagentContextHeader(namespace, taskId) {
    const taskLabel = taskId ?? 'unclaimed (claim a task first)';
    const safeNamespace = sanitizeSubagentHeaderToken(namespace);
    const safeTaskLabel = taskId === null ? taskLabel : sanitizeSubagentHeaderToken(taskLabel);
    return (`${SUBAGENT_CONTEXT_MARKER} (AUTO-INJECTED BY NEUROSTRATA PLUGIN)\n` +
        `- Namespace: \`${codeSpanValue(namespace)}\` | Active Task ID: \`${codeSpanValue(taskLabel)}\`\n` +
        `- Mandatory Graph-First Lookup Rule: Before blind \`grep\`/\`glob\` or editing files, query \`neurostrata_memories_for_file\`, \`neurostrata_related\`, \`neurostrata_search_memory\` (\`include_related: true\`), or \`neurostrata_sparql_query\` (or \`neurostrata-mcp\` CLI) to inspect governing rules (\`GOVERNS\`) and AST call/import edges (\`DEPENDS_ON\`, \`calls\`, \`imports\`).\n\n`);
}
export function injectSubagentPrompt(target, namespace, taskId) {
    const raw = typeof target.prompt === 'string' ? target.prompt : null;
    if (raw === null || raw.includes(SUBAGENT_CONTEXT_MARKER))
        return false;
    target.prompt = buildSubagentContextHeader(namespace, taskId) + raw;
    return true;
}
// ──────────────────────────────────────────────────────────────────────
// Plugin Entry Point
// ──────────────────────────────────────────────────────────────────────
let config = DEFAULT_CONFIG;
let mcpClient;
/**
 * The plugin context, captured on initialize.
 *
 * The handlers below are module-level functions, so they cannot close over
 * `initialize`'s parameter. Capturing it here is what makes `ctx` reachable
 * from them at all.
 */
let pluginCtx = null;
/** The project folder this session runs in, from the host or the runner. */
let sessionProjectDir;
/** The NeuroStrata tools whose scope follows the session's project folder. */
const SESSION_SCOPED_TOOLS = ['neurostrata_task_claim', 'neurostrata_get_snapshot'];
/**
 * Whether `toolName` is one of SESSION_SCOPED_TOOLS, bare or as a host
 * spells an MCP tool (`mcp__<server>__neurostrata_task_claim`,
 * `<server>_neurostrata_task_claim`, `<server>.neurostrata_task_claim`).
 * A copy of core's `isSessionScopedTool`.
 */
export function isSessionScopedTool(toolName) {
    const name = String(toolName ?? '');
    return SESSION_SCOPED_TOOLS.some((tool) => name === tool ||
        (name.endsWith(tool) && /[_.:]$/.test(name.slice(0, -tool.length))));
}
/**
 * Claim scope from a linked worktree (design 6.5): passes the session's
 * project folder as `worktree` on `neurostrata_task_claim` and
 * `neurostrata_get_snapshot` calls that carry no `worktree`, `project_root`
 * or `cwd`, which the daemon would otherwise scope to the namespace's
 * registry root (the main checkout when a marker applies). Changes `args`
 * in place; true when it did. A copy of core's `addSessionWorktree`.
 */
export function addSessionWorktree(toolName, args, projectDir) {
    if (!isSessionScopedTool(toolName) || !projectDir)
        return false;
    if (args === null || typeof args !== 'object' || Array.isArray(args))
        return false;
    if (asString(args.worktree) ||
        asString(args.project_root) ||
        asString(args.cwd))
        return false;
    args.worktree = projectDir;
    return true;
}
function requireCtx() {
    if (!pluginCtx) {
        throw new Error('NeuroStrata plugin used before initialize()');
    }
    return pluginCtx;
}
export async function initialize(ctx) {
    pluginCtx = ctx ?? null;
    sessionProjectDir = asString(ctx?.projectDir) ?? process.cwd();
    config = resolveConfig(ctx?.config ?? ctx ?? {}, sessionProjectDir);
    const token = config.bearerToken ?? loadDaemonToken(config.tokenFile);
    mcpClient = new NeuroStrataMCPClient({
        endpoint: config.endpoint,
        token,
        timeoutMs: config.requestTimeoutMs
    });
    setStatus({
        state: 'uninitialized',
        endpoint: config.endpoint,
        namespace: config.namespace,
        authenticated: token !== null,
        missingTools: [],
        lastError: undefined
    });
    // Register file operation hook. The CLI runner calls initialize with only
    // a config object (no hooks host), so every registration is optional: the
    // runner path dispatches through handleHookEvent instead.
    ctx?.hooks?.beforeFileOp?.(wrapFileOp);
    // Register turn hook for intent detection
    ctx?.hooks?.onTurnEnd?.(onTurnEnd);
    await negotiateCapabilities();
    ctx?.logger?.info?.('NeuroStrata OpenClaw plugin initialized', {
        namespace: config.namespace,
        endpoint: config.endpoint,
        authenticated: status.authenticated,
        state: status.state,
        missingTools: status.missingTools
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
async function negotiateCapabilities() {
    try {
        const available = new Set(await mcpClient.listToolNames());
        const missing = REQUIRED_TOOLS.filter((tool) => !available.has(tool));
        setStatus({
            state: missing.length > 0 ? 'degraded' : 'ready',
            missingTools: missing,
            lastError: undefined
        });
        if (missing.length > 0) {
            requireCtx().logger?.warn('NeuroStrata core does not expose the tools this shim needs', { missing });
        }
    }
    catch (err) {
        setStatus({
            state: 'unavailable',
            lastError: String(err)
        });
        requireCtx().logger?.warn('NeuroStrata core is unreachable; the shim is degraded', { error: String(err) });
    }
}
/** Run the Zero-Action Start guard for one mutating action. Never throws:
 *  wrapFileOp maps the verdict to a FileOpResult (preserving its log lines),
 *  and the CLI runner entry point maps it to a hook decision. */
async function runActionGuard(actionType, payload) {
    if (!config.strictMode) {
        mutationsSinceNote++;
        return { verdict: 'pass' };
    }
    const cwd = process.cwd();
    try {
        const text = await mcpClient.callTool('neurostrata_guard_validate', {
            namespace: config.namespace,
            session_id: activeSessionId ?? 'unknown',
            active_task_id: activeTaskId,
            worktree: cwd,
            action_type: actionType,
            payload,
            cwd,
            is_mutating: true,
            use_sandbox: false,
            mutations_since_note: mutationsSinceNote
        });
        const result = parseGuardValidate(text);
        if (result.verdict === 'reject') {
            const reason = result.reasons.join('; ') ||
                result.constraints.join('; ') ||
                result.bucket;
            return {
                verdict: 'reject',
                reason,
                ruleIds: result.rule_ids,
                guardVerdict: result
            };
        }
    }
    catch (err) {
        return { verdict: 'unavailable', message: String(err) };
    }
    mutationsSinceNote++;
    return { verdict: 'pass' };
}
/** Runner event tool names that map onto OpenClaw's native FileOperation. */
const OPENCLAW_FILE_OP_TOOLS = {
    write_file: 'write',
    write: 'write',
    edit: 'edit',
    patch_file: 'edit',
    delete: 'delete',
    mkdir: 'mkdir'
};
/** Runner event tool names that are mutating shells. */
const OPENCLAW_SHELL_TOOLS = ['terminal', 'bash', 'shell', 'command'];
const guardToHookResponse = (guard) => {
    if (guard.verdict === 'reject') {
        return {
            hookSpecificOutput: {
                hookEventName: 'PreToolUse',
                permissionDecision: 'deny',
                permissionDecisionReason: `Zero-Action Start violation: ${guard.reason}`
            }
        };
    }
    if (guard.verdict === 'unavailable') {
        return {
            hookSpecificOutput: {
                hookEventName: 'PreToolUse',
                permissionDecision: 'deny',
                permissionDecisionReason: `Guard validation unavailable (NeuroStrata core degraded): ${guard.message}`
            }
        };
    }
    return {
        continue: true,
        hookSpecificOutput: {
            hookEventName: 'PreToolUse',
            permissionDecision: 'allow'
        }
    };
};
/**
 * CLI hook runner entry point. The daemon installs hook-runner.mjs beside
 * dist/ and dispatches host hook events here; initialize may have been
 * called with only a config object (no hooks host), so a default client is
 * built the same way core's handleHookEvent does. File-op and shell events
 * run the same guard as the native beforeFileOp hook, with a rejection
 * returned as a deny decision instead of a failed FileOpResult.
 */
export async function handleHookEvent(event) {
    if (!mcpClient) {
        config = resolveConfig({});
        const token = loadDaemonToken(config.tokenFile);
        mcpClient = new NeuroStrataMCPClient({
            endpoint: config.endpoint,
            token,
            timeoutMs: config.requestTimeoutMs
        });
    }
    const hookEventName = asString(event.hook_event_name) ?? asString(event.eventName) ?? 'PreToolUse';
    const toolName = asString(event.tool_name) ?? asString(event.toolName) ?? '';
    const rawInput = event.tool_input ?? event.input ?? event.args ?? {};
    const toolInput = rawInput && typeof rawInput === 'object'
        ? rawInput
        : {};
    if (hookEventName !== 'PreToolUse') {
        // Stop turns carry the transcript; the daemon's CLI runner dispatches
        // here before its own onTurnEnd fallback, so forward it ourselves or
        // task-intent detection never runs for runner-carried Stop events
        // (Codex review #326).
        if (hookEventName === 'Stop' && typeof event.transcript === 'string') {
            try {
                await onTurnEnd({ role: 'assistant', content: event.transcript });
            }
            catch {
                // turn-end handling is fail-soft for the runner
            }
        }
        return { continue: true };
    }
    const fileOpType = OPENCLAW_FILE_OP_TOOLS[toolName];
    if (fileOpType) {
        const payload = JSON.stringify({
            path: asString(toolInput.path) ?? asString(toolInput.filePath) ?? '',
            content: asString(toolInput.content),
            oldContent: asString(toolInput.oldContent),
            newContent: asString(toolInput.newContent)
        });
        return guardToHookResponse(await runActionGuard(fileOpType, payload));
    }
    if (OPENCLAW_SHELL_TOOLS.includes(toolName)) {
        return guardToHookResponse(await runActionGuard(toolName, JSON.stringify(toolInput)));
    }
    const scoped = addSessionWorktree(toolName, toolInput, sessionProjectDir ?? asString(event.cwd) ?? process.cwd());
    return {
        continue: true,
        hookSpecificOutput: {
            hookEventName: 'PreToolUse',
            permissionDecision: 'allow',
            ...(scoped ? { updatedInput: toolInput } : {})
        }
    };
}
async function wrapFileOp(op) {
    // File-touch recall (§4.5): before guarding the operation, prefetch the
    // memories governing this file and inject them as a system message.
    // Fail-soft: file recall must never block or break the file operation.
    if (op.path && op.path.trim() !== '') {
        try {
            const snippets = await prefetchMemoriesForFile(config.namespace, op.path);
            if (snippets.length > 0) {
                requireCtx().conversation.injectSystemMessage(`[NeuroStrata] Governing memories for ${op.path}:\n${snippets.join('\n')}`);
            }
        }
        catch (err) {
            requireCtx().logger?.warn('File-touch prefetch failed', {
                path: op.path,
                error: String(err)
            });
        }
    }
    if (!config.strictMode) {
        // Executed without validation: it is still a mutation.
        mutationsSinceNote++;
        return { allowed: true };
    }
    const actionType = op.type; // "write" | "edit" | "delete" | "mkdir"
    const payload = JSON.stringify({
        path: op.path,
        content: op.content,
        oldContent: op.oldContent,
        newContent: op.newContent
    });
    const guard = await runActionGuard(actionType, payload);
    if (guard.verdict === 'reject') {
        requireCtx().logger.warn('Guard rejected file operation', {
            actionType,
            session_id: activeSessionId ?? 'unknown',
            active_task_id: activeTaskId,
            worktree: process.cwd(),
            reason: guard.reason,
            rule_ids: guard.ruleIds
        });
        return {
            allowed: false,
            reason: `Zero-Action Start violation: ${guard.reason}`,
            guardVerdict: guard.guardVerdict,
            degraded: false
        };
    }
    if (guard.verdict === 'unavailable') {
        setStatus({ state: 'unavailable', lastError: guard.message });
        requireCtx().logger.error('Guard validation failed', {
            error: guard.message
        });
        // Fail closed: a guard that cannot answer is not a guard that passed.
        return {
            allowed: false,
            reason: `Guard validation unavailable (NeuroStrata core degraded): ${guard.message}`,
            degraded: true
        };
    }
    // Allowed: the operation executes, so it counts as a mutation. The count
    // happens inside runActionGuard (shared with the runner entry point);
    // counting again here would double every strict-pass (Codex review #326).
    return { allowed: true };
}
// ──────────────────────────────────────────────────────────────────────
// Turn Hook: intent detection + nudge
// ──────────────────────────────────────────────────────────────────────
async function onTurnEnd(turn) {
    // Session/task tracking for guard validation (F15): runs before intent
    // detection so a rejected turn still updates the tracked state.
    trackFromTurn(turn);
    if (!config.intentDetection.enabled)
        return;
    if (turn.role === 'system')
        return; // don't re-process our own nudges
    const text = turn.content;
    if (text.includes('neurostrata_task_create'))
        return;
    // The core exposes no intent-detection tool: the earlier
    // `neurostrata_detect_task_intents` call was a phantom the core answered
    // with "Unknown tool". The lexical detector runs here instead, and
    // Phase 0 writes nothing — a detected intent is a nudge, never a task.
    const signals = detectTaskIntentsLocal(text);
    if (signals.length === 0)
        return;
    if (!config.intentDetection.injectReminder)
        return;
    const markerList = signals.map((signal) => signal.marker).join(', ');
    const nudge = `[NeuroStrata] Task intent detected (${markerList}) but no neurostrata_task_create in this turn. Consider creating a task to track this work.`;
    requireCtx().conversation.injectSystemMessage(nudge);
}
// ──────────────────────────────────────────────────────────────────────
// Everything above is exported at its declaration: the markers, the config
// resolver, the wire helpers, the verdict parser and the adapter status are
// the contract the adapter tests exercise.
// ──────────────────────────────────────────────────────────────────────
export default {
    id: 'neurostrata-openclaw',
    setup: initialize,
    initialize,
    handleHookEvent
};
//# sourceMappingURL=index.js.map