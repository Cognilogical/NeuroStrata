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

import {
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  renameSync,
  statSync,
  unlinkSync,
  writeFileSync
} from 'node:fs'
import { homedir } from 'node:os'
import { basename, dirname, join, relative, resolve } from 'node:path'

// ──────────────────────────────────────────────────────────────────────
// Configuration
// ──────────────────────────────────────────────────────────────────────

export const DEFAULT_ENDPOINT = 'http://127.0.0.1:34343/mcp'
export const DEFAULT_TOKEN_FILE = 'daemon.token'
export const REQUIRED_TOOLS = [
  'neurostrata_guard_validate',
  'neurostrata_task_create'
]

export interface PluginConfig {
  namespace: string
  endpoint: string
  bearerToken: string | null
  tokenFile: string | null
  strictMode: boolean
  requestTimeoutMs: number
  intentDetection: { enabled: boolean; injectReminder: boolean }
  /** Resolved project root directory (set in initialize from ctx.location / cwd). */
  projectRoot?: string
}

/** Alias kept for readability in handler signatures (event, config: ResolvedConfig). */
export type ResolvedConfig = PluginConfig

const DEFAULT_CONFIG: PluginConfig = {
  namespace: 'NeuroStrata',
  endpoint: DEFAULT_ENDPOINT,
  bearerToken: null,
  tokenFile: null,
  strictMode: true,
  requestTimeoutMs: 5000,
  intentDetection: { enabled: true, injectReminder: true }
}

function asRecord(value: unknown): Record<string, unknown> {
  return typeof value === 'object' && value !== null
    ? (value as Record<string, unknown>)
    : {}
}

function firstRecord(...candidates: unknown[]): Record<string, unknown> {
  for (const candidate of candidates) {
    if (typeof candidate === 'object' && candidate !== null) {
      return candidate as Record<string, unknown>
    }
  }
  return {}
}

function asString(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() !== ''
    ? value.trim()
    : undefined
}

function asNumber(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) && value > 0
    ? value
    : undefined
}

function asBoolean(value: unknown): boolean | undefined {
  return typeof value === 'boolean' ? value : undefined
}

/** Allowlist namespace/task tokens before code-span interpolation (#316). */
export function sanitizeSubagentHeaderToken(value: string): string {
  return String(value ?? '').replace(/[^A-Za-z0-9_.:-]/g, '')
}

/**
 * Resolve `namespace` in strict priority order, the same order as
 * `neurostrata-mcp namespace resolve`:
 *  1. the project root marker (`.NeuroStrata/config.json`, findRootMarker(cwd))
 *  2. pluginConfig.namespace (the harness config; non-empty trimmed string)
 *  3. process.env.NEUROSTRATA_NAMESPACE (non-empty trimmed string)
 *  4. discoverProjectNamespace(cwd)
 * Never falls back to a hardcoded "NeuroStrata".
 */
export function resolveConfig(
  pluginConfig: unknown = {},
  cwd = process.cwd()
): PluginConfig {
  const input = asRecord(pluginConfig)
  const intent = asRecord(input.intentDetection)
  const endpoint =
    asString(input.endpoint) ??
    asString(input.guardEndpoint) ??
    DEFAULT_CONFIG.endpoint
  // Preserve the exact namespace for MCP routing; sanitization happens only
  // where the value is interpolated into a prompt header (#329 review).
  const namespace =
    findRootMarker(resolve(cwd))?.namespace ??
    asString(input.namespace) ??
    asString(process.env.NEUROSTRATA_NAMESPACE) ??
    discoverProjectNamespace(cwd)
  return {
    namespace,
    endpoint,
    bearerToken: asString(input.bearerToken) ?? null,
    tokenFile: asString(input.tokenFile) ?? null,
    strictMode: asBoolean(input.strictMode) ?? DEFAULT_CONFIG.strictMode,
    requestTimeoutMs:
      asNumber(input.requestTimeoutMs) ?? DEFAULT_CONFIG.requestTimeoutMs,
    intentDetection: {
      enabled:
        asBoolean(intent.enabled) ?? DEFAULT_CONFIG.intentDetection.enabled,
      injectReminder:
        asBoolean(intent.injectReminder) ??
        DEFAULT_CONFIG.intentDetection.injectReminder
    }
  }
}

// ──────────────────────────────────────────────────────────────────────
// Bug 2: project-root / namespace discovery
// ──────────────────────────────────────────────────────────────────────

// The project root marker walk (`.NeuroStrata/config.json`). This mirrors
// `root_marker::find_root_marker` in the Rust daemon step for step; the
// shared cases in tests/fixtures/root-marker-cases.json are the contract both
// must pass. Only file-system reads, no process starts (#309).

/** Which walk found a project root marker. */
export type RootMarkerVia = 'own' | 'main' | 'nogit'

export interface FoundRootMarker {
  /** The project root, in the start path's own checkout. */
  root: string
  namespace: string
  via: RootMarkerVia
}

type MarkerRead =
  | { valid: true; namespace: string }
  | { valid: false; path: string; reason: string }

/** Marker files already warned about in this process: one line each. */
const warnedMarkers = new Set<string>()

/**
 * Says on stderr, once per file, that the project marker at `path` cannot be
 * read and is skipped: the namespace then comes from the other sources, as
 * `neurostrata-mcp` does with its own warning (design section 5).
 */
function warnUnreadableMarker(path: string, reason: string): void {
  if (warnedMarkers.has(path)) return
  warnedMarkers.add(path)
  console.error(
    `[NeuroStrata] warning: the project marker ${path} could not be read (${reason}); it is skipped`
  )
}

/**
 * The raw spelling of every top-level `version` value in a JSON object text
 * that already parsed. JSON.parse reads `1.0` as 1, but the daemon's reader
 * (an unsigned integer) refuses it, so the spelling is checked here.
 */
function topLevelVersionSpellings(text: string): string[] {
  const found: string[] = []
  let depth = 0
  let i = 0
  while (i < text.length) {
    const c = text[i]
    if (c === '"') {
      let j = i + 1
      while (j < text.length && text[j] !== '"') j += text[j] === '\\' ? 2 : 1
      const literal = text.slice(i, j + 1)
      i = j + 1
      if (depth !== 1) continue
      let k = i
      while (k < text.length && /\s/.test(text[k])) k++
      if (text[k] !== ':' || JSON.parse(literal) !== 'version') continue
      k++
      while (k < text.length && /\s/.test(text[k])) k++
      const number = /^-?[0-9][0-9.eE+-]*/.exec(text.slice(k))
      found.push(number ? number[0] : text[k] ?? '')
      i = k
      continue
    }
    if (c === '{' || c === '[') depth++
    else if (c === '}' || c === ']') depth--
    i++
  }
  return found
}

const NAMESPACE_PATTERN = /^[A-Za-z0-9][A-Za-z0-9_.:-]*$/

/**
 * A namespace value as written, or undefined when it is not one: empty, an
 * unexpanded variable, or anything but `[A-Za-z0-9_.:-]` starting with a
 * letter or digit. The Rust installer's `usable_namespace` rule.
 */
export function usableNamespace(raw: unknown): string | undefined {
  if (typeof raw !== 'string') return undefined
  const ns = raw.trim().replace(/^["']+|["']+$/g, '')
  return NAMESPACE_PATTERN.test(ns) ? ns : undefined
}

/** Absolute on this host: a drive or UNC path on Windows, `/` elsewhere. */
function isHostAbsolute(p: string): boolean {
  if (process.platform === 'win32') {
    return /^[A-Za-z]:[\\/]/.test(p) || /^[\\/]{2}[^\\/]/.test(p)
  }
  return p.startsWith('/')
}

/** Absolute on any host, for paths git wrote into `.git` and `commondir`. */
function isAbsoluteAny(p: string): boolean {
  return p.startsWith('/') || p.startsWith('\\') || /^[A-Za-z]:[\\/]/.test(p)
}

/** The canonical spelling of an existing path, as Rust's `dunce` gives it. */
function canonicalPath(p: string): string | null {
  try {
    const real = realpathSync.native(p)
    // Drop the verbatim prefix where a plain spelling exists.
    if (real.startsWith('\\\\?\\UNC\\')) return '\\\\' + real.slice(8)
    if (real.startsWith('\\\\?\\')) return real.slice(4)
    return real
  } catch {
    return null
  }
}

/** `p` canonicalized through its nearest existing ancestor. */
function canonicalThroughExisting(p: string): string {
  const missing: string[] = []
  let existing = p
  while (true) {
    const canonical = canonicalPath(existing)
    if (canonical !== null) {
      return missing.reduceRight((acc, part) => join(acc, part), canonical)
    }
    const parent = dirname(existing)
    if (parent === existing) return p
    missing.push(basename(existing))
    existing = parent
  }
}

function isDirectory(p: string): boolean {
  try {
    return statSync(p).isDirectory()
  } catch {
    return false
  }
}

function isFile(p: string): boolean {
  try {
    return statSync(p).isFile()
  } catch {
    return false
  }
}

function ancestorsOf(dir: string): string[] {
  const out: string[] = []
  let current = dir
  while (true) {
    out.push(current)
    const next = dirname(current)
    if (next === current) return out
    current = next
  }
}

function pathParts(rel: string): string[] {
  return rel.split(/[\\/]/).filter((part) => part !== '' && part !== '.')
}

/** The nearest folder at or above `dir` holding `.git` (folder or file). */
function checkoutTop(dir: string): string | null {
  return ancestorsOf(dir).find((d) => existsSync(join(d, '.git'))) ?? null
}

/** `rel` against `base` unless it is already absolute. */
function joinedGitPath(base: string, rel: string): string {
  return isAbsoluteAny(rel) ? rel : join(base, rel)
}

/** The gitdir a checkout's `.git` FILE names (relative to the checkout). */
function gitdirOfFile(checkout: string): string | null {
  let text: string
  try {
    text = readFileSync(join(checkout, '.git'), 'utf8')
  } catch {
    return null
  }
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim()
    if (trimmed.startsWith('gitdir:')) {
      return joinedGitPath(checkout, trimmed.slice('gitdir:'.length).trim())
    }
  }
  return null
}

/** The common directory a linked worktree's gitdir names in `commondir`. */
function linkedCommonDir(gitdir: string): string | null {
  try {
    const common = readFileSync(join(gitdir, 'commondir'), 'utf8').trim()
    return joinedGitPath(gitdir, common)
  } catch {
    return null
  }
}

/**
 * The main checkout when `top` is a linked worktree's top (its gitdir names
 * a `commondir` that is a checkout's `.git`), else null. A submodule (no
 * `commondir`) or a bare repository's worktree (common dir not `.git`) has
 * none.
 */
function linkedMainCheckout(top: string): string | null {
  if (isDirectory(join(top, '.git'))) return null
  const gitdir = gitdirOfFile(top)
  const common = gitdir === null ? null : linkedCommonDir(gitdir)
  if (common === null) return null
  const normalized = resolve(common)
  if (basename(normalized) !== '.git') return null
  const main = canonicalPath(dirname(normalized))
  if (main === null) return null
  const own = canonicalPath(top) ?? top
  return main === own ? null : main
}

/** The marker in `dir`: valid, unreadable, or null when there is none. */
function readRootMarker(dir: string): MarkerRead | null {
  const path = join(dir, '.NeuroStrata', 'config.json')
  const unreadable = (reason: string): MarkerRead => ({ valid: false, path, reason })
  let text: string
  try {
    text = readFileSync(path, 'utf8')
  } catch (err) {
    return (err as { code?: string })?.code === 'ENOENT'
      ? null
      : unreadable(String((err as Error)?.message ?? err))
  }
  if (text.charCodeAt(0) === 0xfeff) text = text.slice(1)
  let parsed: unknown
  try {
    parsed = JSON.parse(text)
  } catch {
    return unreadable('it is not valid JSON')
  }
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    return unreadable('it is not a JSON object')
  }
  const doc = parsed as Record<string, unknown>
  const sourceId = doc.source_id
  const versions = topLevelVersionSpellings(text)
  if (doc.version !== 1 || versions.length !== 1 || versions[0] !== '1') {
    return unreadable('its version is not 1')
  }
  if (typeof doc.namespace !== 'string' || !NAMESPACE_PATTERN.test(doc.namespace)) {
    return unreadable('its namespace is missing or not a namespace')
  }
  if (sourceId !== undefined && sourceId !== null && typeof sourceId !== 'string') {
    return unreadable('its source_id is not a string')
  }
  return { valid: true, namespace: doc.namespace }
}

/**
 * The marker in `dir` when there is one to report. A `.NeuroStrata` that is
 * a link pointing elsewhere, or a folder without `config.json`, is none.
 */
function markerAt(dir: string): MarkerRead | null {
  const folder = join(dir, '.NeuroStrata')
  try {
    const st = lstatSync(folder)
    if (st.isSymbolicLink() || !st.isDirectory()) return null
  } catch {
    return null
  }
  const realFolder = canonicalPath(folder)
  const realDir = canonicalPath(dir)
  if (realFolder === null || realDir === null) return null
  if (realFolder !== join(realDir, '.NeuroStrata')) return null
  return readRootMarker(dir)
}

function depthBelow(dir: string, top: string): number {
  return pathParts(relative(top, dir)).length
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
export function findRootMarker(startDir: string): FoundRootMarker | null {
  if (typeof startDir !== 'string' || !isHostAbsolute(startDir)) return null
  let start = canonicalThroughExisting(startDir)
  if (isFile(start)) start = dirname(start)

  const decide = (
    root: string,
    read: MarkerRead,
    via: RootMarkerVia
  ): FoundRootMarker | null => {
    if (read.valid) return { root, namespace: read.namespace, via }
    warnUnreadableMarker(read.path, read.reason)
    return null
  }

  const top = checkoutTop(start)
  if (top === null) {
    for (const dir of ancestorsOf(start)) {
      const read = markerAt(dir)
      if (read) return decide(dir, read, 'nogit')
    }
    return null
  }

  let own: { depth: number; root: string; read: MarkerRead } | null = null
  for (const dir of ancestorsOf(start)) {
    const read = markerAt(dir)
    if (read) {
      own = { depth: depthBelow(dir, top), root: dir, read }
      break
    }
    if (dir === top) break
  }

  let main: { depth: number; root: string; read: MarkerRead } | null = null
  const mainTop = linkedMainCheckout(top)
  if (mainTop !== null) {
    const mainCanonical = canonicalPath(mainTop) ?? mainTop
    const insideMain = (p: string): boolean => {
      const rel = relative(mainCanonical, p)
      const parts = pathParts(rel)
      return parts.length === 0 || (parts[0] !== '..' && !isAbsoluteAny(rel))
    }
    const allowed = [mainTop]
    let dir = mainTop
    for (const part of pathParts(relative(top, start))) {
      dir = join(dir, part)
      let real = false
      try {
        const st = lstatSync(dir)
        real = st.isDirectory() && !st.isSymbolicLink()
      } catch {}
      const canonical = canonicalPath(dir)
      // A link in the main checkout may point into another repository; its
      // markers are not this worktree's. Stop at links and at any folder
      // whose canonical form leaves the main checkout.
      if (
        !real ||
        existsSync(join(dir, '.git')) ||
        canonical === null ||
        !insideMain(canonical)
      )
        break
      allowed.push(dir)
    }
    for (let i = allowed.length - 1; i >= 0; i--) {
      const read = markerAt(allowed[i])
      if (read) {
        const below = relative(mainTop, allowed[i])
        main = {
          depth: depthBelow(allowed[i], mainTop),
          root: below === '' ? top : join(top, below),
          read
        }
        break
      }
    }
  }

  if (own && main) {
    return main.depth > own.depth
      ? decide(main.root, main.read, 'main')
      : decide(own.root, own.read, 'own')
  }
  if (own) return decide(own.root, own.read, 'own')
  if (main) return decide(main.root, main.read, 'main')
  return null
}

/** Whether two folders are the same, comparing case-insensitively on Windows. */
function sameFolder(a: string, b: string): boolean {
  const x = resolve(a)
  const y = resolve(b)
  return process.platform === 'win32'
    ? x.toLowerCase() === y.toLowerCase()
    : x === y
}

/**
 * Walk up from resolve(startDir) looking for `.git` (dir or file); return the
 * directory containing it. With no `.git` above (pass 2), the nearest folder
 * holding a valid project marker (`.NeuroStrata/config.json`), else the
 * nearest folder holding `.NeuroStrata` other than the home folder, else
 * resolve(startDir).
 */
export function findProjectRoot(
  startDir = process.cwd(),
  home: string = homedir()
): string {
  // Pass 1: walk up looking for `.git` first; a `.git` anchors the project root.
  let current = resolve(startDir)
  while (true) {
    if (existsSync(join(current, '.git'))) {
      return current
    }
    const next = dirname(current)
    if (next === current) break
    current = next
  }
  // Pass 2: no `.git` anywhere in the ancestor chain.
  const chain = ancestorsOf(resolve(startDir))
  const marked = chain.find((dir) => markerAt(dir)?.valid === true)
  if (marked !== undefined) return marked
  // `~/.NeuroStrata` holds the user's own state, not a project's: a session
  // started below home without a marker must not take home as its root.
  const nearest = chain.find(
    (dir) =>
      existsSync(join(dir, '.NeuroStrata')) && !(home && sameFolder(dir, home))
  )
  return nearest ?? resolve(startDir)
}

/** Each harness's plugin config, relative to the project root, and the
 * namespace's place inside it. The installer writes these files. */
const HARNESS_CONFIG_FILES: Record<string, { file: string[]; key: string[] }> = {
  claude: { file: ['.NeuroStrata', 'plugins', 'claude', 'neurostrata.json'], key: ['namespace'] },
  opencode: { file: ['.opencode', 'neurostrata-plugin.json'], key: ['neurostrata', 'namespace'] },
  codex: { file: ['.codex', 'plugins', 'neurostrata.json'], key: ['namespace'] },
  hermes: { file: ['.hermes', 'plugins', 'neurostrata.json'], key: ['namespace'] },
  openclaw: { file: ['.openclaw', 'plugins', 'neurostrata.json'], key: ['namespace'] },
  'github-cli': {
    file: ['.NeuroStrata', 'plugins', 'github-cli', 'copilot-extension.json'],
    key: ['namespace']
  }
}

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
export function readHarnessConfig(
  harness: string,
  projectDir: string,
  runnerDir?: string
): { namespace?: string } {
  const spec = HARNESS_CONFIG_FILES[harness]
  if (!spec) return {}
  const roots: Array<string | null> = []
  if (typeof runnerDir === 'string' && runnerDir !== '') {
    const plugins = dirname(resolve(runnerDir))
    const stateDir = dirname(plugins)
    if (
      basename(resolve(runnerDir)) === harness &&
      basename(plugins) === 'plugins' &&
      basename(stateDir) === '.NeuroStrata'
    ) {
      roots.push(dirname(stateDir))
    }
  }
  if (typeof projectDir === 'string' && projectDir !== '') {
    const start = canonicalThroughExisting(resolve(projectDir))
    const top = checkoutTop(start)
    roots.push(...(top === null ? [start] : [top, linkedMainCheckout(top)]))
  }
  for (const root of roots) {
    if (root === null) continue
    try {
      let value: unknown = JSON.parse(
        readFileSync(join(root, ...spec.file), 'utf8').replace(/^\uFEFF/, '')
      )
      for (const key of spec.key) value = asRecord(value)[key]
      const namespace = usableNamespace(value)
      if (namespace) return { namespace }
    } catch {
      // missing or malformed: try the next root
    }
  }
  return {}
}

/** How many SessionStart folders the Claude runner remembers. */
const CLAUDE_SESSION_DIRS_KEEP = 20

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
export function resolveHookProjectDir(
  harness: string,
  event: Record<string, unknown> = {},
  env: Record<string, string | undefined> = process.env,
  home: string = homedir()
): string {
  const cwd = asString(event?.cwd) ?? process.cwd()
  if (harness !== 'claude') return cwd
  const fromEnv = asString(env.CLAUDE_PROJECT_DIR)
  if (fromEnv) return fromEnv
  const sessionId = asString(event?.session_id)
  if (!sessionId) return cwd
  const root = findProjectRoot(cwd, home)
  // `~/.NeuroStrata` is the user's own state, not a project's: a session
  // whose root falls back to the home folder records nothing there.
  if (home && sameFolder(root, home)) return cwd
  const stateDir = join(root, '.NeuroStrata')
  const sessionPath = join(stateDir, 'active_session.json')
  let state: Record<string, unknown> = {}
  let unparsable = false
  try {
    const parsed: unknown = JSON.parse(readFileSync(sessionPath, 'utf8'))
    if (typeof parsed === 'object' && parsed !== null && !Array.isArray(parsed)) {
      state = parsed as Record<string, unknown>
    } else {
      unparsable = true
    }
  } catch (err) {
    unparsable = (err as { code?: string })?.code !== 'ENOENT'
  }
  const dirs = asRecord(state.claude_session_dirs)
  const recorded = asString(dirs[sessionId])
  if (asString(event?.hook_event_name) === 'SessionStart') {
    if (recorded) return recorded
    // Only where NeuroStrata state already lives; never create it here.
    if (!isDirectory(stateDir) || unparsable) return cwd
    const kept = Object.entries(dirs).filter(([id]) => id !== sessionId)
    kept.push([sessionId, cwd])
    state.claude_session_dirs = Object.fromEntries(
      kept.slice(-CLAUDE_SESSION_DIRS_KEEP)
    )
    writeJsonAtomically(sessionPath, state)
    return cwd
  }
  return recorded ?? cwd
}

/**
 * Replaces `path` with `value` as JSON through a temporary file in the same
 * folder and a rename, so a reader never sees half a file. Best effort: on
 * failure the old file stays and the temporary one is removed.
 */
function writeJsonAtomically(path: string, value: unknown): void {
  const tmp = `${path}.${process.pid}.${Date.now()}.tmp`
  try {
    writeFileSync(tmp, JSON.stringify(value, null, 2))
    renameSync(tmp, path)
  } catch {
    try {
      unlinkSync(tmp)
    } catch {
      // nothing was written
    }
  }
}

/** The NeuroStrata tools whose scope follows the session's project folder. */
const SESSION_SCOPED_TOOLS = ['neurostrata_task_claim', 'neurostrata_get_snapshot']

/**
 * Whether `toolName` is one of [SESSION_SCOPED_TOOLS], bare or as a harness
 * spells an MCP tool (`mcp__<server>__neurostrata_task_claim`,
 * `<server>_neurostrata_task_claim`, `<server>.neurostrata_task_claim`).
 */
export function isSessionScopedTool(toolName: string): boolean {
  const name = String(toolName ?? '')
  return SESSION_SCOPED_TOOLS.some(
    (tool) =>
      name === tool ||
      (name.endsWith(tool) && /[_.:]$/.test(name.slice(0, -tool.length)))
  )
}

/**
 * Claim scope from a linked worktree (design 6.5): `neurostrata_task_claim`
 * and `neurostrata_get_snapshot` without `worktree`, `project_root` or `cwd`
 * are scoped by the daemon to the namespace's registry root, the main
 * checkout when a marker applies. This passes the session's project folder
 * as `worktree` instead. Changes `args` in place; true when it did.
 */
export function addSessionWorktree(
  toolName: string,
  args: Record<string, unknown>,
  projectDir: string | undefined
): boolean {
  if (!isSessionScopedTool(toolName) || !projectDir) return false
  if (args === null || typeof args !== 'object' || Array.isArray(args)) return false
  if (
    asString(args.worktree) ||
    asString(args.project_root) ||
    asString(args.cwd)
  )
    return false
  args.worktree = projectDir
  return true
}

/**
 * 1. Walk up to findProjectRoot(startDir) checking <dir>/.NeuroStrata/session_queries.json;
 *    if it parses as JSON with a non-empty string .namespace, return it trimmed.
 * 2. Otherwise return basename(findProjectRoot(startDir)) || "default".
 */
export function discoverProjectNamespace(startDir = process.cwd()): string {
  const root = findProjectRoot(startDir)
  let current = resolve(startDir)
  while (true) {
    const sessionQueriesPath = join(
      current,
      '.NeuroStrata',
      'session_queries.json'
    )
    try {
      if (existsSync(sessionQueriesPath)) {
        const content = readFileSync(sessionQueriesPath, 'utf8')
        const parsed = JSON.parse(content)
        if (
          parsed !== null &&
          typeof parsed === 'object' &&
          typeof parsed.namespace === 'string' &&
          parsed.namespace.trim() !== ''
        ) {
          return parsed.namespace.trim()
        }
      }
    } catch {
      // fall through to next ancestor
    }
    if (current === root) break
    const next = dirname(current)
    if (next === current) break
    current = next
  }
  return basename(root) || 'default'
}

// ──────────────────────────────────────────────────────────────────────
// OpenCode V2 project-root & namespace resolution
// ──────────────────────────────────────────────────────────────────────

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
export function resolveProjectRoot(ctx?: any): string {
  const c = ctx ?? {}
  return (
    asString(c?.location?.project?.canonical) ??
    asString(c?.location?.project?.root) ??
    asString(c?.location?.directory) ??
    asString(c?.projectDir) ??
    asString(c?.directory) ??
    asString(c?.cwd) ??
    process.cwd()
  )
}

/**
 * Load an optional plugin config from <projectRoot>/.NeuroStrata/
 * (neurostrata-plugin.json, then PluginConfig.json). Missing or malformed
 * files are a no-op; the host-supplied ctx.config always wins the merge.
 */
function loadPluginConfigFromDisk(
  projectRoot: string
): Record<string, unknown> {
  for (const file of ['neurostrata-plugin.json', 'PluginConfig.json']) {
    const p = join(projectRoot, '.NeuroStrata', file)
    if (existsSync(p)) {
      try {
        return asRecord(JSON.parse(readFileSync(p, 'utf8')))
      } catch {
        // ignore malformed / unreadable config files
      }
    }
  }
  return {}
}

/** Effective project root for reading .NeuroStrata state files. */
function effectiveProjectRoot(config: ResolvedConfig): string {
  return config.projectRoot ?? resolveProjectRoot()
}

/**
 * Make a touched-file path relative to the project root when it lives under
 * it, so the injected `[NeuroStrata File Context — <relPath>]` block is
 * stable and short. Falls back to the raw path otherwise.
 */
function pathRelativeToProjectRoot(
  filePath: string,
  config: ResolvedConfig
): string {
  const root = effectiveProjectRoot(config)
  try {
    // Resolve a relative filePath against the project root (not process.cwd())
    // so the startsWith check below is meaningful when cwd != projectRoot.
    const resolved = resolve(root, filePath)
    const rootWithSep = root.endsWith('/') ? root : root + '/'
    if (resolved === root || resolved.startsWith(rootWithSep)) {
      return resolved.slice(root.length).replace(/^\/+/, '')
    }
  } catch {
    // fall through to the raw path
  }
  return filePath
}

// ──────────────────────────────────────────────────────────────────────
// Credential
// ──────────────────────────────────────────────────────────────────────

export function daemonTokenPath(tokenFile?: string | null): string {
  if (tokenFile && tokenFile.trim() !== '') {
    return tokenFile.trim()
  }
  const envTokenDir =
    (typeof process !== 'undefined' &&
      process.env?.NEUROSTRATA_TOKEN_DIR?.trim()) ??
    ''
  if (envTokenDir) {
    return envTokenDir
  }
  return join(homedir(), '.config', 'neurostrata', DEFAULT_TOKEN_FILE)
}

export function loadDaemonToken(tokenFile?: string | null): string | null {
  try {
    const raw = readFileSync(daemonTokenPath(tokenFile), 'utf8').trim()
    return raw.length > 0 ? raw : null
  } catch {
    return null
  }
}

export function buildHeaders(token: string | null): Record<string, string> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' }
  if (token) headers.Authorization = `Bearer ${token}`
  return headers
}

// ──────────────────────────────────────────────────────────────────────
// MCP wire format
// ──────────────────────────────────────────────────────────────────────

export interface JsonRpcRequest {
  jsonrpc: '2.0'
  id: number
  method: string
  params?: unknown
}

export interface JsonRpcResponse {
  jsonrpc?: string
  id?: number | string | null
  result?: unknown
  error?: { code?: number; message?: string }
}

export interface McpToolResult {
  content?: Array<{ type?: string; text?: string }>
  isError?: boolean
}

export function buildToolCallRequest(
  id: number,
  name: string,
  args: unknown
): JsonRpcRequest {
  return {
    jsonrpc: '2.0',
    id,
    method: 'tools/call',
    params: { name, arguments: args }
  }
}

export function buildToolsListRequest(id: number): JsonRpcRequest {
  return { jsonrpc: '2.0', id, method: 'tools/list', params: {} }
}

export function extractToolText(result: unknown): string {
  const content = (result as McpToolResult | undefined)?.content
  if (!Array.isArray(content))
    throw new Error('tool result carried no content array')
  const text = content
    .map((part) => (typeof part?.text === 'string' ? part.text : ''))
    .filter((part) => part.length > 0)
    .join('\n')
  if (text.length === 0) throw new Error('tool result carried no text content')
  return text
}

export function extractToolNames(result: unknown): string[] {
  const tools = (result as { tools?: Array<{ name?: string }> } | undefined)
    ?.tools
  if (!Array.isArray(tools))
    throw new Error('tools/list returned no tools array')
  return tools
    .map((tool) => (typeof tool?.name === 'string' ? tool.name : ''))
    .filter((name) => name.length > 0)
}

// ──────────────────────────────────────────────────────────────────────
// MCP client
// ──────────────────────────────────────────────────────────────────────

export class NeuroStrataMCPClient {
  private readonly endpoint: string
  private readonly token: string | null
  private readonly timeoutMs: number
  private requestId = 0
  /** Daemon memory advisory captured on the last errored tool call, drained by
   *  the plugin's execute.after handler so the agent sees it as a suffix line
   *  rather than an opaque throw. Cleared on every callTool. */
  private pendingDaemonAdvisory: string | null = null

  constructor(options: {
    endpoint: string
    token: string | null
    timeoutMs: number
  }) {
    this.endpoint = options.endpoint
    this.token = options.token
    this.timeoutMs = options.timeoutMs
  }

  /** Drain the advisory captured by the last errored callTool, if any. */
  flushPendingAdvisory(): string | null {
    const advisory = this.pendingDaemonAdvisory
    this.pendingDaemonAdvisory = null
    return advisory
  }

  async callTool(name: string, args: unknown): Promise<string> {
    const id = ++this.requestId
    const response = await this.post(buildToolCallRequest(id, name, args))
    // Compute the tool text once: it is the same payload the error path
    // throws on, and the daemon's advisory marker lives on the error
    // channel, so only that channel is scanned (never normal tool output).
    const text =
      response.result === undefined || response.result === null
        ? ''
        : (() => {
            try {
              return extractToolText(response.result)
            } catch {
              return ''
            }
          })()
    if (response.error) {
      this.pendingDaemonAdvisory = extractDaemonMemoryAdvisory(
        typeof response.error.message === 'string' ? response.error.message : ''
      )
      throw new Error(
        `[${response.error.code ?? '?'}] ${response.error.message ?? 'tool call failed'}`
      )
    }
    if (response.result === undefined || response.result === null) {
      this.pendingDaemonAdvisory = null
      throw new Error(`${name} returned no result`)
    }
    if ((response.result as McpToolResult).isError) {
      this.pendingDaemonAdvisory = extractDaemonMemoryAdvisory(text)
      throw new Error(`${name} failed: ${text}`)
    }
    this.pendingDaemonAdvisory = null
    return text
  }

  async listToolNames(): Promise<string[]> {
    const id = ++this.requestId
    const response = await this.post(buildToolsListRequest(id))
    if (response.error)
      throw new Error(
        `[${response.error.code ?? '?'}] ${response.error.message ?? 'tools/list failed'}`
      )
    return extractToolNames(response.result)
  }

  private async post(body: JsonRpcRequest): Promise<JsonRpcResponse> {
    const controller = new AbortController()
    const timer = setTimeout(() => controller.abort(), this.timeoutMs)
    try {
      const response = await fetch(this.endpoint, {
        method: 'POST',
        headers: buildHeaders(this.token),
        body: JSON.stringify(body),
        signal: controller.signal
      })
      if (!response.ok)
        throw new Error(`HTTP ${response.status} ${response.statusText}`)
      return (await response.json()) as JsonRpcResponse
    } catch (err) {
      if (err instanceof Error && err.name === 'AbortError')
        throw new Error(`request timed out after ${this.timeoutMs}ms`)
      throw err
    } finally {
      clearTimeout(timer)
    }
  }
}

// ──────────────────────────────────────────────────────────────────────
// Guard verdict
// ──────────────────────────────────────────────────────────────────────

export interface GuardValidateResult {
  verdict: 'pass' | 'reject'
  bucket: string
  reasons: string[]
  constraints: string[]
  notes: string[]
  rule_ids: string[]
}

export function parseGuardValidate(text: string): GuardValidateResult {
  const start = text.indexOf('{')
  const end = text.lastIndexOf('}')
  if (start === -1 || end <= start)
    throw new Error(
      `guard_validate returned no JSON body: ${text.slice(0, 200)}`
    )
  let body: Record<string, unknown>
  try {
    body = JSON.parse(text.slice(start, end + 1)) as Record<string, unknown>
  } catch (err) {
    throw new Error(
      `guard_validate returned an unreadable body: ${String(err)}`
    )
  }
  const verdict = asRecord(body.verdict)
  const bucket = asString(verdict.bucket) ?? 'Unknown'
  const reasons = Array.isArray(verdict.reasons)
    ? verdict.reasons.map((r) => String(r))
    : []
  const constraints = Array.isArray(verdict.constraints)
    ? verdict.constraints.map((c) => String(c))
    : []
  const notes = Array.isArray(verdict.notes)
    ? verdict.notes.map((n) => String(n))
    : []
  const rule_ids = Array.isArray(body.rule_ids_triggered)
    ? body.rule_ids_triggered.map((id) => String(id))
    : []
  const rejecting =
    bucket === 'DeterministicReject' || bucket === 'SandboxReject'
  return {
    verdict: rejecting ? 'reject' : 'pass',
    bucket,
    reasons,
    constraints,
    notes,
    rule_ids
  }
}

// ──────────────────────────────────────────────────────────────────────
// Adapter state
// ──────────────────────────────────────────────────────────────────────

export type AdapterState =
  | 'uninitialized'
  | 'ready'
  | 'degraded'
  | 'unavailable'

export interface AdapterStatus {
  state: AdapterState
  endpoint: string
  namespace: string
  authenticated: boolean
  missingTools: string[]
  lastError?: string
  updatedAt: string
}

function initialStatus(): AdapterStatus {
  return {
    state: 'uninitialized',
    endpoint: DEFAULT_ENDPOINT,
    namespace: 'NeuroStrata',
    authenticated: false,
    missingTools: [],
    updatedAt: new Date(0).toISOString()
  }
}

let status: AdapterStatus = initialStatus()

export function adapterStatus(): AdapterStatus {
  return { ...status, missingTools: [...status.missingTools] }
}

function setStatus(patch: Partial<AdapterStatus>): void {
  status = { ...status, ...patch, updatedAt: new Date().toISOString() }
}

// ──────────────────────────────────────────────────────────────────────
// Intent detection (local, mirrors src/task/intent.rs)
// ──────────────────────────────────────────────────────────────────────

export interface TaskIntentSignal {
  marker: string
  excerpt: string
}

export const MARKERS: readonly [string, string][] = [
  ['todo:', 'todo:'],
  ['fixme:', 'fixme:'],
  ['follow-up:', 'follow-up:'],
  ['followup:', 'followup:'],
  ['next step:', 'next step:'],
  ['action item:', 'action item:'],
  ['remaining work:', 'remaining work:'],
  ['we should also ', 'we should also'],
  ['later we need to ', 'later we need to'],
  ['need to follow up ', 'need to follow up'],
  ['still need to ', 'still need to'],
  ['must also fix ', 'must also fix'],
  ['filed follow-up ', 'filed follow-up'],
  ['defer ', 'defer'],
  ['deferred ', 'deferred']
]

export const NEGATION_PREFIXES = ['never ', 'do not ', "don't ", 'do not, ']

export function detectTaskIntentsLocal(text: string): TaskIntentSignal[] {
  const lower = text.toLowerCase()
  const out: TaskIntentSignal[] = []
  const seen = new Set<string>()
  for (const [matchStr, canonical] of MARKERS) {
    const idx = lower.indexOf(matchStr)
    if (idx === -1) continue
    if (seen.has(canonical)) continue
    const clauseStart = text.slice(0, idx).lastIndexOf('\n') + 1
    let start = clauseStart
    const sentPos = text.slice(0, idx).lastIndexOf('. ')
    if (sentPos !== -1 && sentPos + 2 > start) start = sentPos + 2
    const clausePrefix = text.slice(start, idx).trimStart().toLowerCase()
    if (NEGATION_PREFIXES.some((p) => clausePrefix.startsWith(p))) continue
    const rest = text.slice(idx)
    const end = Math.min(
      rest.indexOf('\n') === -1 ? rest.length : rest.indexOf('\n'),
      160
    )
    let excerpt = rest.slice(0, end).trimEnd()
    if (excerpt.length > 160) excerpt = excerpt.slice(0, 160)
    seen.add(canonical)
    out.push({ marker: canonical, excerpt })
  }
  return out
}

// ──────────────────────────────────────────────────────────────────────
// Wave 4 Task 4.1: turn-candidate extraction + conceptual bridge persistence
// ──────────────────────────────────────────────────────────────────────

export interface ProvenanceSpan {
  turn_id?: string
  role?: string
  source_tool?: string
  channel?: string
  evidence_quote?: string
  quote?: string
  char_range?: [number, number]
  parent_task_id?: string
  confidence?: number
}

export interface Turn {
  role: string
  content: string
  turn_id?: string
  source_tool?: string
  channel?: string
  parent_task_id?: string
}

export interface TurnCandidate {
  kind: 'task_obligation' | 'memory_candidate'
  category?: 'fact' | 'decision' | 'rule_candidate'
  title: string
  description?: string
  confidence: number
  auto_commit: boolean
  provenance: ProvenanceSpan
}

export interface ConceptualBridge {
  source_id?: string
  via_id?: string
  target_id?: string
  target_domain?: string
  target_type?: string
  target_summary?: string
  path?: string
}

/**
 * Extract task-obligation candidates from a turn's content. Detects local
 * task-intent markers (TODO:, FIXME:, follow-up:, ...) via
 * `detectTaskIntentsLocal` and attaches a provenance span
 * (turn_id, role, source_tool, channel, evidence_quote, quote, char_range,
 * confidence) to each. Pure: no daemon interaction. Rule candidates are
 * never produced here (rule candidates never auto-promote).
 */
export function extractTurnCandidates(turn: Turn): TurnCandidate[] {
  const { content, role, turn_id, source_tool, channel, parent_task_id } = turn
  const signals = detectTaskIntentsLocal(content)
  const out: TurnCandidate[] = []
  for (const signal of signals) {
    // Guard: never indexOf an empty excerpt.
    if (signal.excerpt.length === 0) continue
    // `signal.marker` is the lowercase canonical form; `signal.excerpt` is the
    // verbatim content substring, so index off the excerpt to land the span at
    // the actual (case-varying) marker text in `content`.
    const idx = content.indexOf(signal.excerpt)
    const start = idx === -1 ? 0 : idx
    const end = Math.min(content.length, start + signal.excerpt.length)
    out.push({
      kind: 'task_obligation',
      title: signal.excerpt,
      confidence: 0.9,
      auto_commit: false,
      provenance: {
        turn_id,
        role,
        source_tool,
        channel,
        evidence_quote: signal.excerpt,
        quote: signal.excerpt,
        char_range: [start, end],
        parent_task_id,
        confidence: 0.9
      }
    })
  }
  // Decision / fact markers: explicit DECISION:/FACT:/Architectural decision:/
  // We decided to: claims emitted as memory_candidate candidates with
  // ProvenanceSpan so the intake Lock 2 gate can see them.
  const DF_MARKERS =
    /\b(?:DECISION|FACT|Architectural decision|We decided to)\s*:\s*([^\n.]{8,200})/gi
  for (const m of content.matchAll(DF_MARKERS)) {
    const marker = m[0].split(':')[0].trim()
    const category =
      /^(?:DECISION|Architectural decision|We decided to)$/i.test(marker)
        ? 'decision'
        : 'fact'
    const excerpt = m[1]
    const start = m.index + m[0].indexOf(excerpt)
    const end = start + excerpt.length
    out.push({
      kind: 'memory_candidate',
      category,
      title: excerpt,
      confidence: 0.9,
      auto_commit: role === 'user' || role === 'checker',
      provenance: {
        turn_id,
        role,
        source_tool,
        channel,
        evidence_quote: excerpt,
        quote: excerpt,
        char_range: [start, end],
        parent_task_id,
        confidence: 0.9
      }
    })
  }
  return out
}

/**
 * Post turn candidates fail-soft to the intake ledger via
 * `neurostrata_task_candidates` (`action: "record"`). Never throws: a missing
 * MCP client or a daemon error is swallowed so a turn is never blocked by
 * intake-ledger submission.
 */
export async function postTurnCandidates(
  candidates: TurnCandidate[],
  namespace: string
): Promise<void> {
  if (!mcpClient || candidates.length === 0) return
  for (const cand of candidates) {
    try {
      await mcpClient.callTool('neurostrata_task_candidates', {
        namespace,
        action: 'record',
        kind: cand.kind,
        category: cand.category,
        confidence: cand.confidence,
        title: cand.title,
        description: cand.description,
        auto_commit: cand.auto_commit,
        task_id: cand.provenance?.parent_task_id ?? activeTaskId,
        provenance: cand.provenance
      })
    } catch {
      // fail-soft: intake-ledger submission never blocks a turn
    }
  }
}

/**
 * Extract de-duplicated conceptual bridges from `memories_for_file` hits.
 * Each hit may carry a `conceptual_bridges` array (source -> via -> target
 * 2-hop cross-domain paths). Bridges are de-duplicated by `path`.
 */
export function extractConceptualBridges(hits: unknown[]): ConceptualBridge[] {
  const out: ConceptualBridge[] = []
  const seen = new Set<string>()
  for (const hit of hits) {
    if (typeof hit !== 'object' || hit === null) continue
    const rec = hit as Record<string, unknown>
    const cbs = rec.conceptual_bridges
    if (!Array.isArray(cbs)) continue
    for (const cb of cbs) {
      if (typeof cb !== 'object' || cb === null) continue
      const b = cb as Record<string, unknown>
      const path = asString(b.path)
      if (!path || seen.has(path)) continue
      seen.add(path)
      out.push({
        source_id: asString(b.source_id),
        via_id: asString(b.via_id),
        target_id: asString(b.target_id),
        target_domain: asString(b.target_domain),
        target_type: asString(b.target_type),
        target_summary: asString(b.target_summary),
        path
      })
    }
  }
  return out
}

/**
 * Persist conceptual bridges into `.NeuroStrata/active_session.json` under
 * the `conceptual_bridges` key, merging with any existing fields so the
 * `sessionContextHandler` can inject them as `[NeuroStrata Conceptual Bridges]`.
 */
export function persistConceptualBridges(
  projectRoot: string,
  bridges: ConceptualBridge[]
): void {
  const nsDir = join(projectRoot, '.NeuroStrata')
  const sessionPath = join(nsDir, 'active_session.json')
  let active: Record<string, unknown> = {}
  if (existsSync(sessionPath)) {
    try {
      active = asRecord(JSON.parse(readFileSync(sessionPath, 'utf8')))
    } catch {
      active = {}
    }
  }
  const existing = Array.isArray(active.conceptual_bridges)
    ? (active.conceptual_bridges as ConceptualBridge[])
    : []
  const byId = new Map<string, ConceptualBridge>()
  let seq = 0
  const keyOf = (b: ConceptualBridge) =>
    b.target_id ?? b.source_id ?? b.via_id ?? `bridge:${seq++}`
  for (const b of existing) byId.set(keyOf(b), b)
  for (const b of bridges) byId.set(keyOf(b), b) // newer wins on dup id
  active.conceptual_bridges = Array.from(byId.values()).slice(0, 4)
  try {
    mkdirSync(nsDir, { recursive: true })
    writeFileSync(sessionPath, JSON.stringify(active, null, 2))
  } catch {
    // best-effort; bridge persistence never breaks a file touch
  }
}

// ──────────────────────────────────────────────────────────────────────
// Bug 3: canonical tool classification
// ──────────────────────────────────────────────────────────────────────

export const READ_FILE_TOOLS = new Set([
  'read',
  'Read',
  'read_file',
  'grep',
  'Grep',
  'glob',
  'Glob',
  'mcp_glob',
  'view',
  'cat'
])

export const MUTATING_FILE_TOOLS = new Set([
  'write',
  'Write',
  'write_file',
  'create_file',
  'create',
  'edit',
  'Edit',
  'MultiEdit',
  'edit_file',
  'str_replace_editor',
  'patch',
  'patch_file',
  'apply_patch',
  'replace_string_in_file',
  'insert_edit_into_file',
  // Claude Code edits notebook cells with NotebookEdit (#333/B3).
  'NotebookEdit'
])

export const SHELL_TOOLS = new Set([
  'bash',
  'Bash',
  'shell',
  'local_shell',
  'exec_command',
  'run_shell_command',
  'run_command',
  'execute_command',
  'terminal',
  'gh_exec'
])

export const MUTATING_TOOLS = new Set([...MUTATING_FILE_TOOLS, ...SHELL_TOOLS])

export const SUBAGENT_TOOLS = new Set([
  'subagent',
  'Task',
  'task',
  'dispatch_agent',
  'spawn_agent',
  'agent',
  'delegate_task'
])

export const CODE_EXEC_TOOLS = new Set(['execute', 'mcp_execute'])

export const TOOL_CLASSIFICATION = {
  READ_FILE_TOOLS,
  MUTATING_FILE_TOOLS,
  SHELL_TOOLS,
  MUTATING_TOOLS,
  SUBAGENT_TOOLS,
  CODE_EXEC_TOOLS
}

export function isFileTouchTool(name: string): boolean {
  return READ_FILE_TOOLS.has(name) || MUTATING_FILE_TOOLS.has(name)
}

export function isMutatingTool(name: string): boolean {
  return MUTATING_TOOLS.has(name)
}

// Tool output (stdout / file contents) is not user or assistant speech: intent
// markers such as TODO:/defer found there are code or log noise, not a task
// obligation. Never post intake candidates from read-only inspection tools.
const READ_ONLY_INSPECTION_TOOLS = new Set([
  'read',
  'read_file',
  'shell',
  'bash',
  'local_shell',
  'exec_command',
  'run_shell_command',
  'run_command',
  'execute_command',
  'terminal',
  'gh_exec',
  'grep',
  'glob',
  'view',
  'cat'
])

export function isReadOnlyInspectionTool(name: string): boolean {
  const lower = String(name).toLowerCase()
  // Code Mode's execute pair is also tool output, never user/assistant speech.
  return READ_ONLY_INSPECTION_TOOLS.has(lower) || CODE_EXEC_TOOLS.has(lower)
}

export function isSubagentTool(name: string): boolean {
  return SUBAGENT_TOOLS.has(name)
}

export function isCodeExecTool(name: string): boolean {
  return CODE_EXEC_TOOLS.has(name)
}

/**
 * Fail-closed recognition of a single read-only / verification shell segment.
 * Only an explicit allowlist passes; anything else is treated as mutating.
 */
function stripMatchingQuotes(token: string): string {
  const t = token.trim()
  if (t.length >= 2) {
    const first = t[0]
    const last = t[t.length - 1]
    if ((first === '"' && last === '"') || (first === "'" && last === "'")) {
      return t.slice(1, -1)
    }
  }
  return t
}

function isZeroActionExemptShellSegment(segment: string): boolean {
  const s = segment.trim()
  if (s.replace(/&&/g, '').includes('&')) return false
  let tokens = s.split(/\s+/).filter(Boolean).map(stripMatchingQuotes)
  while (tokens.length > 0 && /^[A-Za-z_][A-Za-z0-9_]*=/.test(tokens[0])) {
    tokens = tokens.slice(1)
  }
  if (tokens.length === 0) return false
  const bin = tokens[0].replace(/\\/g, '/').split('/').pop() ?? ''
  const args = tokens.slice(1)
  const hasFlag = (flags: string[]): boolean =>
    args.some((arg) =>
      flags.some(
        (flag) =>
          arg === flag ||
          (flag.startsWith('--')
            ? arg.startsWith(`${flag}=`)
            : arg.startsWith(flag))
      )
    )

  if (bin === 'git') {
    let rest = args
    while (rest.length > 0) {
      const head = rest[0]
      if (head === '-C' || head === '--git-dir' || head === '--work-tree') {
        rest = rest.slice(2)
      } else if (
        head === '--no-pager' ||
        head === '-P' ||
        head === '--no-optional-locks' ||
        head.startsWith('--git-dir=') ||
        head.startsWith('--work-tree=')
      ) {
        rest = rest.slice(1)
      } else {
        break
      }
    }
    const sub = rest[0]
    const subArgs = rest.slice(1)
    // Fail closed on every way to force a push: bundled short flags (-vf,
    // -uf), forced refspecs (+main), destructive remote operations, --dele*,
    // --no-verify, and empty-source :refspec. Short bundle d|f is only
    // checked before '=' and only for a real short cluster (not -oformat).
    if (sub === 'push') {
      const isMutatingPush = subArgs.some((a) => {
        if (a.startsWith('+')) return true
        if (a.startsWith('--force')) return true
        if (a === '--mirror' || a === '--delete' || a === '--prune') return true
        if (a.startsWith('--dele')) return true
        if (a === '--no-verify') return true
        if (a.startsWith(':') && a.length > 1) return true
        if (a.startsWith('-') && !a.startsWith('--')) {
          const bundle = a.split('=')[0]
          const letters = bundle.slice(1)
          // Parse the short-option cluster left-to-right: `-o` (--push-option)
          // consumes the rest as its attached value, so only flags appearing
          // before it decide mutability (`-fometa` = `-f -o meta`).
          for (const ch of letters) {
            if (!/[A-Za-z]/.test(ch)) break
            if (ch === 'f' || ch === 'd') return true
            if (ch === 'o') break
          }
        }
        return false
      })
      if (isMutatingPush) return false
    }
    if (
      [
        'push',
        'pull',
        'fetch',
        'status',
        'log',
        'diff',
        'show',
        'rev-parse'
      ].includes(sub)
    ) {
      return !hasFlag([
        '--output',
        '-O',
        '--ext-diff',
        '--textconv',
        '--exec',
        '-x',
        '--upload-pack',
        '--receive-pack'
      ])
    }
    if (sub === 'stash')
      return (
        (subArgs[0] === 'list' || subArgs[0] === 'show') &&
        !hasFlag([
          '--output',
          '-O',
          '--ext-diff',
          '--textconv',
          '--exec',
          '-x',
          '--upload-pack',
          '--receive-pack'
        ])
      )
    if (sub === 'branch') {
      return (
        subArgs.length > 0 &&
        subArgs.every((arg) =>
          ['--show-current', '-a', '-r', '--list', '-v', '-vv'].includes(arg)
        )
      )
    }
    return false
  }

  if (bin === 'neurostrata-mcp') {
    if (args[0] === 'task' || args[0] === 'doc-gate') return true
    return args[0] === 'synaptic-mesh' && args[1] === 'status'
  }

  if (bin === 'cargo') {
    const rest = args[0]?.startsWith('+') ? args.slice(1) : args
    return [
      'test',
      'check',
      'clippy',
      'bench',
      'metadata',
      'tree',
      'version',
      '--version'
    ].includes(rest[0])
  }

  if (bin === 'gh') {
    if (args[0] === 'pr')
      return ['view', 'list', 'checks', 'status', 'diff'].includes(args[1])
    if (args[0] === 'issue') return ['view', 'list', 'status'].includes(args[1])
    if (args[0] === 'run') return ['view', 'list', 'watch'].includes(args[1])
    if (args[0] === 'api') {
      return !hasFlag([
        '-X',
        '--method',
        '-f',
        '-F',
        '--field',
        '--raw-field',
        '--input'
      ])
    }
    return false
  }

  if (bin === 'node') {
    return args.some((arg) => arg === '--test' || arg.endsWith('test.mjs'))
  }

  if (bin === 'gate-receipt.sh' || bin === 'check-include-str-targets.sh')
    return true
  if (bin === 'bash') {
    const next = args[0] ?? ''
    return (
      next.endsWith('gate-receipt.sh') ||
      next.endsWith('check-include-str-targets.sh')
    )
  }

  if (bin === 'rustc') return args[0] === '--version' || args[0] === '-V'

  return false
}

/**
 * True only for shell tools whose payload is provably read-only/verification.
 * JSON payloads are unwrapped via `command`/`cmd`; substitutions, redirection
 * and chains containing any mutating segment fail closed.
 */
export function isZeroActionExemptShellPayload(
  actionType: string,
  payloadOrCommand: string
): boolean {
  if (
    !['bash', 'shell', 'command', 'exec'].includes(
      String(actionType).toLowerCase()
    )
  )
    return false
  let command = payloadOrCommand
  const trimmed = payloadOrCommand.trim()
  if (trimmed.startsWith('{')) {
    let parsed: Record<string, unknown>
    try {
      parsed = asRecord(JSON.parse(trimmed))
    } catch {
      return false
    }
    command = asString(parsed.command) ?? asString(parsed.cmd) ?? ''
  }
  const cmd = command.trim()
  if (
    !cmd ||
    cmd.includes('`') ||
    cmd.includes('$(') ||
    cmd.includes('>') ||
    cmd.includes('<')
  )
    return false
  // A lone '&' backgrounds the command; splitting on it would hide the operator.
  if (cmd.replace(/&&/g, '').includes('&')) return false
  const segments = cmd
    .split(/[;\n|&]+/)
    .map((segment) => segment.trim())
    .filter(Boolean)
  return segments.length > 0 && segments.every(isZeroActionExemptShellSegment)
}

export interface CodeModeInspection {
  id?: string
  sessionId?: string
  note?: string
  taskClaimed?: boolean
  taskUpdated?: boolean
  taskCompleted?: boolean
  memoryMutated?: boolean
  queriedFiles: string[]
}

function extractBalancedCallArgs(
  code: string,
  startAfterOpenParen: number
): string {
  let depth = 1
  let quote: string | null = null
  let i = startAfterOpenParen
  while (i < code.length && depth > 0) {
    const ch = code[i]
    if (quote) {
      if (ch === '\\') {
        i += 2
        continue
      }
      if (ch === quote) quote = null
      i++
      continue
    }
    if (ch === '"' || ch === "'" || ch === '`') {
      quote = ch
    } else if (ch === '(') {
      depth++
    } else if (ch === ')') {
      depth--
      if (depth === 0) break
    }
    i++
  }
  return code.slice(startAfterOpenParen, i)
}

/**
 * Inspect JS code strings passed to OpenCode V2 `execute` (Code Mode).
 * Detects neurostrata_task_claim / _update / _complete / _memories_for_file
 * calls in order and extracts id / session_id / note / path arguments from
 * each matched call's own argument slice.
 */
export function inspectCodeModeExecution(code: string): CodeModeInspection {
  const result: CodeModeInspection = { queriedFiles: [] }
  const callRe =
    /\b(neurostrata_(?:task_claim|task_update|task_complete|memories_for_file|locate|archive_memory|add_memory|supersede_memory|update_memory))\b(?:\s*["']\s*\])?\s*\(/g
  let match: RegExpExecArray | null
  while ((match = callRe.exec(code)) !== null) {
    const tool = match[1]
    const argSlice = extractBalancedCallArgs(
      code,
      match.index + match[0].length
    )
    // Keys may be bare identifiers (`id: "t1"`) or JSON-style quoted
    // (`{"id":"t1"}`); the quoted form puts a quote between the key and the
    // colon, so the key itself has to accept optional quotes.
    const idMatch = argSlice.match(
      /(?:^|[^\w$])["']?id["']?\s*:\s*["']([^"']+)["']/
    )
    const sessionMatch = argSlice.match(
      /(?:^|[^\w$])["']?session_id["']?\s*:\s*["']([^"']+)["']/
    )
    const noteMatch = argSlice.match(
      /(?:^|[^\w$])["']?note["']?\s*:\s*["']([^"']*)["']/
    )
    const pathMatch = argSlice.match(
      /(?:^|[^\w$])["']?(?:path|path_prefix|query)["']?\s*:\s*["']([^"']+)["']/
    )
    // A claim/update can pass variables or template literals instead of string
    // literals. Those values are not extractable, but the key itself still
    // proves the call's intent — detect it without mistaking explicit
    // undefined/null for a real value.
    const hasIdKey =
      /(?:^|[^\w$])["']?id["']?\s*:\s*(?!undefined\b|null\b)\S/.test(argSlice)
    const hasSessionKey =
      /(?:^|[^\w$])["']?session_id["']?\s*:\s*(?!undefined\b|null\b)\S/.test(
        argSlice
      )
    const hasNoteKey =
      /(?:^|[^\w$])["']?note["']?\s*:\s*(?!undefined\b|null\b)\S/.test(argSlice)

    if (tool === 'neurostrata_task_claim') {
      if (idMatch) result.id = idMatch[1]
      if (sessionMatch) result.sessionId = sessionMatch[1]
      if ((idMatch || hasIdKey) && (sessionMatch || hasSessionKey)) {
        result.taskClaimed = true
        result.taskCompleted = false
      }
    } else if (tool === 'neurostrata_task_update') {
      result.taskUpdated = true
      if (idMatch) result.id = idMatch[1]
      if (sessionMatch) result.sessionId = sessionMatch[1]
      if (noteMatch) result.note = noteMatch[1]
      else if (hasNoteKey) result.note = ''
    } else if (tool === 'neurostrata_task_complete') {
      result.taskCompleted = true
    } else if (
      tool === 'neurostrata_memories_for_file' ||
      tool === 'neurostrata_locate'
    ) {
      if (pathMatch) result.queriedFiles.push(pathMatch[1])
    } else if (
      tool === 'neurostrata_archive_memory' ||
      tool === 'neurostrata_add_memory' ||
      tool === 'neurostrata_supersede_memory' ||
      tool === 'neurostrata_update_memory'
    ) {
      result.memoryMutated = true
    }
  }
  return result
}

// ──────────────────────────────────────────────────────────────────────
// File-touch prefetch (LRU dedup + injected-memory dedup)
// ──────────────────────────────────────────────────────────────────────

export const PREFETCH_LRU_CAP = 128
export const PREFETCH_MAX_CHARS = 2048
export const prefetchedPaths = new Map<string, number>()
export const injectedMemoryIds = new Set<string>()

/**
 * Memory ids injected by a prefetch that has not been delivered yet. A denied
 * edit never delivers its recall block, so the ids it consumed have to be
 * given back -- otherwise the allowed retry finds the path and the memories
 * already marked as delivered and returns no snippets at all.
 */
const pendingPrefetchDeliveries = new Map<string, string[]>()

function recordPrefetchedPath(key: string): void {
  prefetchedPaths.set(key, Date.now())
  if (prefetchedPaths.size > PREFETCH_LRU_CAP) {
    const oldest = prefetchedPaths.keys().next().value
    if (oldest !== undefined) prefetchedPaths.delete(oldest)
  }
}

function touchPrefetchedPath(key: string): void {
  prefetchedPaths.delete(key)
  recordPrefetchedPath(key)
}

export interface FileMemoryHit {
  id: string
  valid_from: string
  memory_type: string
  content: string
}

export function parseFileMemoryHits(text: string): FileMemoryHit[] {
  const start = text.indexOf('{')
  const end = text.lastIndexOf('}')
  if (start === -1 || end <= start)
    throw new Error(
      `memories_for_file returned no JSON body: ${text.slice(0, 200)}`
    )
  let body: Record<string, unknown>
  try {
    body = JSON.parse(text.slice(start, end + 1)) as Record<string, unknown>
  } catch (err) {
    throw new Error(
      `memories_for_file returned an unreadable body: ${String(err)}`
    )
  }
  const raw = Array.isArray(body.hits)
    ? body.hits
    : Array.isArray(body.memories)
      ? body.memories
      : []
  const out: FileMemoryHit[] = []
  for (const item of raw) {
    if (typeof item !== 'object' || item === null) continue
    const hit = item as Record<string, unknown>
    const id = asString(hit.id) ?? asString(hit.memory_id)
    const valid_from = asString(hit.valid_from) ?? ''
    if (!id) continue
    out.push({
      id,
      valid_from,
      memory_type: asString(hit.memory_type) ?? asString(hit.type) ?? 'memory',
      content:
        asString(hit.content) ??
        asString(hit.text) ??
        asString(hit.summary) ??
        ''
    })
  }
  return out
}

function formatFileMemorySnippet(hit: FileMemoryHit): string {
  const where = hit.valid_from ? ` (valid_from ${hit.valid_from})` : ''
  return `[NeuroStrata ${hit.memory_type} ${hit.id}${where}] ${hit.content}`
}

function capSnippets(snippets: string[]): string[] {
  let total = 0
  const out: string[] = []
  for (const snippet of snippets) {
    if (total >= PREFETCH_MAX_CHARS) break
    const room = PREFETCH_MAX_CHARS - total
    out.push(snippet.length > room ? snippet.slice(0, room) : snippet)
    total += Math.min(snippet.length, room) + 1
  }
  return out
}

/**
 * Record a session-query ledger entry in <projectRoot>/.NeuroStrata/session_queries.json.
 * Only writes when opts.recordLedgerDir is provided OR <projectRoot>/.NeuroStrata already
 * exists on disk, so unit tests do not dirty random directories unless requested.
 */
function recordLocalSessionQuery(
  namespace: string,
  tool: string,
  query: string,
  recordLedgerDir?: string
): void {
  const rootDir = recordLedgerDir
    ? findProjectRoot(recordLedgerDir)
    : findProjectRoot()
  const shouldRecord =
    recordLedgerDir !== undefined || existsSync(join(rootDir, '.NeuroStrata'))
  if (!shouldRecord) return
  // The governed prefix is the queried path itself. Widening it to the first
  // path component would claim coverage of every sibling file under that
  // directory, so the pre-commit hook would silence warnings for files that
  // were never looked up.
  const entry = {
    ts: Math.floor(Date.now() / 1000),
    tool,
    query,
    governed_prefixes: [query]
  }
  const ledgerPath = join(rootDir, '.NeuroStrata', 'session_queries.json')
  let parsed: unknown
  try {
    parsed = existsSync(ledgerPath)
      ? JSON.parse(readFileSync(ledgerPath, 'utf8'))
      : undefined
  } catch {
    parsed = undefined
  }
  let out: Record<string, unknown>
  if (Array.isArray(parsed)) {
    // Legacy top-level array: wrap it into the object form.
    out = { namespace, queries: [...parsed, entry] }
  } else if (
    parsed !== null &&
    typeof parsed === 'object' &&
    Array.isArray((parsed as Record<string, unknown>).queries)
  ) {
    const obj = parsed as Record<string, unknown>
    out = {
      ...obj,
      namespace: (obj.namespace as string | undefined) || namespace,
      queries: [...(obj.queries as unknown[]), entry]
    }
  } else {
    out = { namespace, queries: [entry] }
  }
  try {
    mkdirSync(dirname(ledgerPath), { recursive: true })
    writeFileSync(ledgerPath, JSON.stringify(out, null, 2))
  } catch {
    // best-effort; never break a file touch for a ledger write
  }
}

interface RawFileRecallEntry {
  dedupKey?: string
  snippet: string
}

const subagentFileRecallCache = new Map<string, RawFileRecallEntry[]>()

function cacheSubagentFileRecall(
  key: string,
  entries: RawFileRecallEntry[]
): void {
  subagentFileRecallCache.delete(key)
  subagentFileRecallCache.set(key, entries)
  if (subagentFileRecallCache.size > PREFETCH_LRU_CAP) {
    const oldest = subagentFileRecallCache.keys().next().value
    if (oldest !== undefined) subagentFileRecallCache.delete(oldest)
  }
}

function formatAstEntryList(val: unknown, maxItems = 5): string {
  if (!Array.isArray(val)) return '[]'
  const arr = val.map((item) => {
    if (typeof item === 'string') return item
    if (typeof item === 'object' && item !== null) {
      const rec = item as Record<string, unknown>
      const symbol_name = asString(rec.symbol_name)
      const name = asString(rec.name)
      const file_path = asString(rec.file_path) ?? asString(rec.path)
      const id = asString(rec.id)
      if (symbol_name && file_path) return `${symbol_name}@${file_path}`
      return symbol_name ?? name ?? file_path ?? id ?? String(item)
    }
    return String(item)
  })
  const capped = arr.slice(0, maxItems)
  const suffix = arr.length > maxItems ? `,+${arr.length - maxItems}` : ''
  return `[${capped.join(',')}${suffix}]`
}

function parseRawFileRecallEntries(
  text: string,
  normalized: string
): RawFileRecallEntry[] {
  const entries: RawFileRecallEntry[] = []
  let body: Record<string, unknown> = {}
  try {
    body = asRecord(JSON.parse(text))
  } catch {
    body = {}
  }
  for (const hit of parseFileMemoryHits(text)) {
    entries.push({
      dedupKey: `${hit.id}:${hit.valid_from}`,
      snippet: formatFileMemorySnippet(hit)
    })
  }
  let astDetail = ''
  if (
    Array.isArray(body.related_memories) &&
    body.related_memories.length > 0
  ) {
    astDetail = `related_memories=${body.related_memories.length}(${formatAstEntryList(body.related_memories, 5)})`
  }
  if (body.ast_summary && typeof body.ast_summary === 'object') {
    const ast = body.ast_summary as Record<string, unknown>
    const parts: string[] = []
    if (typeof ast.blast_radius_count === 'number') {
      parts.push(`blast_radius_count=${ast.blast_radius_count}`)
    }
    if (ast.blast_radius !== undefined) {
      if (Array.isArray(ast.blast_radius)) {
        parts.push(`blast_radius=${formatAstEntryList(ast.blast_radius, 5)}`)
      } else if (
        typeof ast.blast_radius === 'number' ||
        typeof ast.blast_radius === 'string'
      ) {
        parts.push(`blast_radius=${String(ast.blast_radius)}`)
      }
    }
    for (const key of [
      'symbols',
      'callers',
      'callees',
      'imports',
      'importers'
    ]) {
      const val = ast[key]
      if (Array.isArray(val) && (val as unknown[]).length > 0) {
        parts.push(`${key}=${formatAstEntryList(val, 5)}`)
      } else if (val !== undefined && !Array.isArray(val)) {
        parts.push(`${key}=${String(val)}`)
      }
    }
    if (parts.length > 0) {
      astDetail += astDetail
        ? ` | ast_summary={${parts.join(', ')}}`
        : `ast_summary={${parts.join(', ')}}`
    }
  }
  if (astDetail) {
    entries.push({ snippet: `[NeuroStrata AST ${normalized}] ${astDetail}` })
  }
  return entries
}

/**
 * Recall governing memories for a touched file. LRU path dedup (no re-query within the
 * window) and injectedMemoryIds dedup (each memory injected once per session).
 */
export async function prefetchMemoriesForFile(
  namespace: string,
  filePath: string,
  opts?: { recordLedgerDir?: string; projectRoot?: string }
): Promise<string[]> {
  const normalized = filePath.trim()
  if (normalized === '') return []
  const key = `${namespace}:${normalized}`
  if (prefetchedPaths.has(key)) {
    touchPrefetchedPath(key)
    return []
  }
  if (!mcpClient) return []
  try {
    const text = await mcpClient.callTool('neurostrata_memories_for_file', {
      namespace,
      path: normalized,
      limit: 5
    })
    recordPrefetchedPath(key)
    // Persist conceptual bridges from the hits into active_session.json so
    // sessionContextHandler can inject them as [NeuroStrata Conceptual Bridges].
    const projectRoot = opts?.projectRoot ?? opts?.recordLedgerDir
    if (projectRoot) {
      try {
        let hits: unknown[] = []
        let topBridges: unknown[] = []
        try {
          const body = JSON.parse(text)
          hits = Array.isArray(body.memories ?? body.hits)
            ? (body.memories ?? body.hits)
            : []
          if (Array.isArray(body.conceptual_bridges)) {
            topBridges = body.conceptual_bridges
          }
        } catch {
          hits = []
        }
        const bridges = [
          ...extractConceptualBridges(hits),
          ...(topBridges as ConceptualBridge[])
        ]
        if (bridges.length > 0) {
          persistConceptualBridges(projectRoot, bridges)
        }
      } catch {
        // bridge persistence is best-effort
      }
    }
    try {
      recordLocalSessionQuery(
        namespace,
        'neurostrata_memories_for_file',
        normalized,
        opts?.recordLedgerDir
      )
    } catch {
      // ledger is best-effort
    }
    const entries = parseRawFileRecallEntries(text, normalized)
    cacheSubagentFileRecall(key, entries)
    const out: string[] = []
    const added: string[] = []
    for (const entry of entries) {
      if (entry.dedupKey) {
        if (injectedMemoryIds.has(entry.dedupKey)) continue
        injectedMemoryIds.add(entry.dedupKey)
        added.push(entry.dedupKey)
      }
      out.push(entry.snippet)
    }
    pendingPrefetchDeliveries.set(key, added)
    return capSnippets(out)
  } catch {
    return []
  }
}

/**
 * Mark a prefetch as delivered: its memories were handed to the caller, so the
 * dedup state stays and a later touch of the same file does not re-inject
 * them.
 */
export function confirmPrefetchDelivery(
  namespace: string,
  filePath: string
): void {
  const normalized = filePath.trim()
  if (!normalized) return
  pendingPrefetchDeliveries.delete(`${namespace}:${normalized}`)
}

/**
 * Roll back the dedup state of a prefetch whose recall was never delivered
 * (the guard denied the edit). The path is un-marked and the memory ids the
 * prefetch consumed are returned, so the allowed retry still sees the
 * governing rules.
 */
export function restorePrefetchDelivery(
  namespace: string,
  filePath: string
): void {
  const normalized = filePath.trim()
  if (!normalized) return
  const key = `${namespace}:${normalized}`
  const added = pendingPrefetchDeliveries.get(key)
  // Only a prefetch that is still pending was consumed but not delivered.
  // A path whose recall was already delivered keeps its dedup state.
  if (added === undefined) return
  for (const id of added) injectedMemoryIds.delete(id)
  pendingPrefetchDeliveries.delete(key)
  prefetchedPaths.delete(key)
}

async function recallMemoriesForSubagentFile(
  namespace: string,
  filePath: string,
  seenInPrompt: Set<string>
): Promise<string[]> {
  const normalized = filePath.trim()
  if (normalized === '') return []
  const key = `${namespace}:${normalized}`
  let entries = subagentFileRecallCache.get(key)
  if (!entries) {
    if (!mcpClient) return []
    try {
      const text = await mcpClient.callTool('neurostrata_memories_for_file', {
        namespace,
        path: normalized,
        limit: 5
      })
      try {
        recordLocalSessionQuery(
          namespace,
          'neurostrata_memories_for_file',
          normalized
        )
      } catch {
        // ledger is best-effort
      }
      entries = parseRawFileRecallEntries(text, normalized)
      cacheSubagentFileRecall(key, entries)
    } catch {
      return []
    }
  } else {
    cacheSubagentFileRecall(key, entries)
  }
  const out: string[] = []
  for (const entry of entries) {
    if (entry.dedupKey) {
      if (seenInPrompt.has(entry.dedupKey)) continue
      seenInPrompt.add(entry.dedupKey)
    }
    out.push(entry.snippet)
  }
  return capSnippets(out)
}

export function formatFileTouchRecallBlock(
  filePath: string,
  snippets: string[]
): string {
  return `[NeuroStrata Governing Memories for ${filePath}]\n${snippets.join('\n')}`
}

// ──────────────────────────────────────────────────────────────────────
// Subagent prompt injection (Enhancement 2 & 3)
// ──────────────────────────────────────────────────────────────────────

export const SUBAGENT_CONTEXT_MARKER = '### NeuroStrata Task & Graph Context'

// Read-only subagents (review / check / explore) have no NeuroStrata MCP tools
// available. Telling them to call task_update/memories_for_file produces
// InvalidToolCallsError churn, so their header omits the tool-call directive.
const READ_ONLY_SUBAGENT_AGENTS = new Set([
  'reviewer',
  'checker',
  'checker-free',
  'explore'
])

export function isReadOnlySubagentAgent(agentId?: string | null): boolean {
  if (!agentId) return false
  const normalized = String(agentId)
    .trim()
    .toLowerCase()
    .replace(/^.*[/:]/, '')
  return READ_ONLY_SUBAGENT_AGENTS.has(normalized)
}

/** Resolve the subagent agent id from the fields harnesses use to name it. */
export function resolveSubagentAgentId(
  target: Record<string, unknown>
): string | undefined {
  const nested = asRecord(target.input)
  const raw =
    target.agent ??
    target.subagent_type ??
    target.subagentType ??
    target.subagent ??
    target.agent_type ??
    nested.agent ??
    nested.subagent_type ??
    nested.subagent
  const id = asString(raw)
  return id && id.trim() ? id.trim() : undefined
}

/** Code-Mode harnesses (OpenCode V2) expose MCP via `execute` + tools.neurostrata.*. */
export function detectCodeMode(ctx?: any, cfg?: PluginConfig): boolean {
  if (
    ctx?.codeMode === true ||
    ctx?.tool?.codeMode === true ||
    (cfg as any)?.codeMode === true
  ) {
    return true
  }
  const harness = (
    asString(
      ctx?.harness ??
        ctx?.harnessName ??
        ctx?.adapter?.name ??
        ctx?.tool?.name ??
        activeHarnessName
    ) ?? ''
  ).toLowerCase()
  return (
    harness.includes('opencode') ||
    harness.includes('code-mode') ||
    harness.includes('codemode')
  )
}

export interface SubagentHeaderOptions {
  agentId?: string | null
  codeMode?: boolean
}

/**
 * Render a value inside a markdown backtick code span without allowing a
 * breakout: backticks close the span and newlines end its line, so both are
 * collapsed to single spaces. The namespace is not privileged input — it can
 * arrive from a committed `.NeuroStrata/session_queries.json`, plugin config,
 * or an environment variable — so every header interpolation goes through
 * here (issue #316).
 */
export function markdownCodeSpanValue(value: unknown): string {
  return String(value ?? '').replace(/[`\r\n\u2028\u2029]+/g, ' ')
}

function buildSubagentContextHeaderInternal(
  namespace: string,
  taskId: string | null,
  extraContext?: string,
  options?: SubagentHeaderOptions
): string {
  const taskLabel = taskId ?? 'unclaimed (claim a task first)'
  const extra =
    extraContext && extraContext.trim() ? `\n${extraContext.trim()}\n` : ''
  let lookupRule: string
  if (isReadOnlySubagentAgent(options?.agentId)) {
    lookupRule =
      '- Read-Only Subagent: no NeuroStrata MCP mutation tools are available; skip the graph-lookup and task_update directives (review/edit tools only).\n'
  } else if (options?.codeMode === true) {
    lookupRule =
      '- Mandatory Graph-First Lookup Rule: Before editing any file, check whether its path and governing memory IDs (`mem-...`) are already listed in the auto-injected context block in your prompt. For any file you touch that is NOT listed, call `tools.neurostrata.memories_for_file` (and `tools.neurostrata.task_update` after notes) via the `execute` tool, not bare MCP tool names.\n'
  } else {
    lookupRule =
      '- Mandatory Graph-First Lookup Rule: Before editing any file, check whether its path and governing memory IDs (`mem-...`) are already listed in the auto-injected context block in your prompt. For any file you touch that is NOT listed in that block, call `neurostrata_memories_for_file` first (or `neurostrata_search_memory` / `neurostrata_related`).\n'
  }
  return (
    `${SUBAGENT_CONTEXT_MARKER} (AUTO-INJECTED BY NEUROSTRATA PLUGIN)\n` +
    `- Namespace: \`${markdownCodeSpanValue(namespace)}\` | Active Task ID: \`${markdownCodeSpanValue(taskLabel)}\`\n` +
    lookupRule +
    extra +
    `\n`
  )
}

/**
 * Public header builder: sanitizes the two caller-supplied values before
 * rendering (issue #316).
 */
export function buildSubagentContextHeader(
  namespace: string,
  taskId: string | null,
  extraContext?: string,
  options?: SubagentHeaderOptions
): string {
  return buildSubagentContextHeaderInternal(
    namespace,
    taskId,
    extraContext,
    options
  )
}

export function injectSubagentPrompt(
  target: Record<string, unknown>,
  namespace: string,
  taskId: string | null,
  extraContext?: string,
  options?: SubagentHeaderOptions
): boolean {
  const raw = typeof target.prompt === 'string' ? target.prompt : null
  if (raw === null || raw.includes(SUBAGENT_CONTEXT_MARKER)) return false
  target.prompt =
    buildSubagentContextHeader(namespace, taskId, extraContext, options) + raw
  return true
}

export function extractRepoFilePaths(promptText: string): string[] {
  // Linear-time token scanner: match contiguous path-character runs, then
  // validate each candidate. Avoids polynomial backtracking of a nested
  // quantifier regex on adversarial input such as "a/".repeat(N) + "!".
  const tokenPattern = /[A-Za-z0-9._/-]+/g
  const out: string[] = []
  const seen = new Set<string>()
  let match: RegExpExecArray | null
  while ((match = tokenPattern.exec(promptText))) {
    const raw = match[0]
    let start = match.index
    // Skip runs immediately preceded by "://" (URL bodies).
    if (start >= 3 && promptText.slice(start - 3, start) === '://') continue
    let token = raw
    // Trim leading "./" or "/".
    for (;;) {
      if (token.startsWith('./')) {
        token = token.slice(2)
        start += 2
        continue
      }
      if (token.startsWith('/')) {
        token = token.slice(1)
        start += 1
        continue
      }
      break
    }
    // Trim trailing "." or "/".
    for (;;) {
      if (token.endsWith('.') || token.endsWith('/')) {
        token = token.slice(0, -1)
        continue
      }
      break
    }
    // A run such as "//example.com/foo/bar.ts" trimmed past its slashes now
    // begins right after "://", so re-check with the adjusted offset.
    if (start >= 3 && promptText.slice(start - 3, start) === '://') continue
    if (!token.includes('/')) continue
    const segments = token.split('/')
    if (segments.some((seg) => seg.length === 0 || seg === '.' || seg === '..'))
      continue
    const finalSegment = segments[segments.length - 1]
    const extMatch = /\.[A-Za-z0-9]{1,8}$/.exec(finalSegment)
    if (!extMatch) continue
    // Require at least one ASCII letter in the extension so generic file types
    // (.ttl, .lean, .jsonl, .rs, …) are extracted while numeric version
    // suffixes such as "release/v1.2" are ignored.
    const ext = extMatch[0].slice(1)
    if (!/[A-Za-z]/.test(ext)) continue
    if (!seen.has(token)) {
      seen.add(token)
      out.push(token)
    }
  }
  return out
}

// Cached daemon subagent prompt injections keyed by active task id.
export const cachedTaskInjection: Map<string, string | null> = new Map()
export const cachedSubagentPromptByTask = cachedTaskInjection

/**
 * Resolve the active task from the daemon (fail-soft). Caches activeTaskId,
 * activeSessionId and cachedTaskInjection.
 */
export async function resolveActiveTaskFromDaemon(
  namespace: string,
  preferredTaskId?: string | null
): Promise<{
  taskId: string | null
  sessionId: string | null
  promptInjection: string | null
}> {
  if (!mcpClient) {
    return {
      taskId: activeTaskId,
      sessionId: activeSessionId,
      promptInjection: null
    }
  }
  try {
    const text = await mcpClient.callTool('neurostrata_task_list', {
      namespace,
      status: 'in_progress'
    })
    let body: Record<string, unknown> = {}
    try {
      body = asRecord(JSON.parse(text))
    } catch {
      body = {}
    }
    const tasks = Array.isArray(body.tasks)
      ? body.tasks
      : Array.isArray(body.items)
        ? body.items
        : []
    const projectRoot = effectiveProjectRoot(config)
    let target: Record<string, unknown> | null = null
    let targetRank = -1
    let firstInProgress: Record<string, unknown> | null = null
    for (const item of tasks) {
      if (typeof item !== 'object' || item === null) continue
      const rec = item as Record<string, unknown>
      const recId = asString(rec.id)
      if (recId) {
        cachedTaskInjection.set(
          recId,
          asString(rec.subagent_prompt_injection) ?? null
        )
      }
      if (asString(rec.status) !== 'in_progress') continue
      if (!firstInProgress) firstInProgress = rec
      const claim = asRecord(rec.claim)
      const recSession =
        asString(rec.session_id) ?? asString(claim.session_id) ?? null
      const recWorktree =
        asString(rec.worktree) ?? asString(claim.worktree) ?? null
      // Strict specificity: an explicit preference wins, then the current
      // session+worktree, then session, then worktree, then any in_progress.
      // A task owned by another session must never hijack this session.
      let rank = 1
      if (preferredTaskId && recId === preferredTaskId) rank = 5
      else if (
        activeSessionId &&
        recSession === activeSessionId &&
        recWorktree &&
        recWorktree === projectRoot
      )
        rank = 4
      else if (activeSessionId && recSession === activeSessionId) rank = 3
      else if (recWorktree && recWorktree === projectRoot) rank = 2
      if (rank > targetRank) {
        target = rec
        targetRank = rank
      }
    }
    if (!target) target = firstInProgress
    if (
      !target &&
      tasks.length > 0 &&
      typeof tasks[0] === 'object' &&
      tasks[0] !== null
    ) {
      target = tasks[0] as Record<string, unknown>
    }
    if (target) {
      const taskId = asString(target.id)
      const claim = asRecord(target.claim)
      const sessionId =
        asString(target.session_id) ?? asString(claim.session_id) ?? null
      const promptInjection = asString(target.subagent_prompt_injection) ?? null
      if (taskId) {
        if (!preferredTaskId || taskId === preferredTaskId) {
          activeTaskId = taskId
          if (
            !activeSessionId ||
            sessionId === activeSessionId ||
            preferredTaskId
          ) {
            activeSessionId = sessionId
          }
        }
        cachedTaskInjection.set(taskId, promptInjection)
      }
      if (preferredTaskId && !cachedTaskInjection.has(preferredTaskId)) {
        cachedTaskInjection.set(preferredTaskId, promptInjection)
      }
      return {
        taskId: preferredTaskId ?? taskId ?? null,
        sessionId: sessionId ?? activeSessionId,
        promptInjection: preferredTaskId
          ? (cachedTaskInjection.get(preferredTaskId) ?? promptInjection)
          : promptInjection
      }
    }
    if (preferredTaskId && !cachedTaskInjection.has(preferredTaskId)) {
      cachedTaskInjection.set(preferredTaskId, null)
    }
  } catch {
    // fail-soft: daemon unreachable or "Unknown tool"
  }
  return {
    taskId: activeTaskId,
    sessionId: activeSessionId,
    promptInjection: null
  }
}

/**
 * Prefetch governing memories for repo file paths mentioned in a subagent prompt and
 * inject the daemon's subagent_prompt_injection plus any recalled snippets into the
 * target prompt. Idempotent.
 */
export async function enrichAndInjectSubagentPrompt(
  target: Record<string, unknown>,
  namespace: string,
  taskId?: string | null
): Promise<boolean> {
  if (
    typeof target.prompt !== 'string' ||
    target.prompt.includes(SUBAGENT_CONTEXT_MARKER)
  ) {
    return false
  }
  let effectiveTaskId: string | null = taskId ?? null
  let injection: string | null = null
  if (!effectiveTaskId) {
    const resolved = await resolveActiveTaskFromDaemon(namespace)
    effectiveTaskId = resolved.taskId
    injection = resolved.promptInjection
  } else if (!cachedTaskInjection.has(effectiveTaskId)) {
    const resolved = await resolveActiveTaskFromDaemon(
      namespace,
      effectiveTaskId
    )
    injection =
      cachedTaskInjection.get(effectiveTaskId) ??
      resolved.promptInjection ??
      null
  } else {
    injection = cachedTaskInjection.get(effectiveTaskId) ?? null
  }
  const headerOptions: SubagentHeaderOptions = {
    agentId: resolveSubagentAgentId(target),
    codeMode: detectCodeMode(pluginCtx, config)
  }
  const extraLines: string[] = []
  if (injection) extraLines.push(injection)
  const files = extractRepoFilePaths(target.prompt).slice(0, 5)
  const seenInPrompt = new Set<string>()
  for (const fp of files) {
    try {
      const snippets = await recallMemoriesForSubagentFile(
        namespace,
        fp,
        seenInPrompt
      )
      extraLines.push(...snippets)
    } catch {
      // prefetch failures never block subagent injection
    }
  }
  return injectSubagentPrompt(
    target,
    namespace,
    effectiveTaskId,
    extraLines.join('\n'),
    headerOptions
  )
}

// ──────────────────────────────────────────────────────────────────────
// Session / task tracking state
// ──────────────────────────────────────────────────────────────────────

let config: PluginConfig = DEFAULT_CONFIG
// Set by createHarnessPlugin(); lets core phrase subagent directives for the
// hosting harness (OpenCode is a Code-Mode harness: tools.neurostrata.* via execute).
let activeHarnessName: string | null = null
let mcpClient: NeuroStrataMCPClient | undefined = undefined
let pluginCtx: any | null = null

const TASK_MUTATION_TOOLS = new Set([
  'neurostrata_task_claim',
  'neurostrata_task_update',
  'neurostrata_task_complete'
])

const MEMORY_MUTATION_TOOLS = new Set([
  'neurostrata_archive_memory',
  'neurostrata_add_memory',
  'neurostrata_supersede_memory',
  'neurostrata_update_memory'
])

function trackNativeTaskToolCall(
  toolName: string,
  payloadArgs: Record<string, unknown>,
  outerInput?: Record<string, unknown>
): void {
  if (
    !TASK_MUTATION_TOOLS.has(toolName) &&
    !MEMORY_MUTATION_TOOLS.has(toolName)
  )
    return
  const sessionId =
    asString(payloadArgs.session_id) ??
    asString(outerInput?.session_id) ??
    asString(outerInput?.sessionID)
  if (sessionId) activeSessionId = sessionId
  const taskId = asString(payloadArgs.id)
  if (
    toolName === 'neurostrata_task_claim' ||
    toolName === 'neurostrata_task_complete' ||
    payloadArgs.note !== undefined
  ) {
    mutationsSinceNote = 0
  }
  if (MEMORY_MUTATION_TOOLS.has(toolName)) {
    cachedTaskInjection.clear()
    prefetchedPaths.clear()
    injectedMemoryIds.clear()
    subagentFileRecallCache.clear()
    return
  }
  if (toolName === 'neurostrata_task_complete') {
    if (activeTaskId) cachedTaskInjection.delete(activeTaskId)
    activeTaskId = null
  } else if (taskId) {
    if (
      toolName === 'neurostrata_task_claim' ||
      toolName === 'neurostrata_task_update' ||
      payloadArgs.title !== undefined
    ) {
      cachedTaskInjection.delete(taskId)
      if (activeTaskId) cachedTaskInjection.delete(activeTaskId)
    }
    activeTaskId = taskId
  }
}

export let activeSessionId: string | null = null
export let activeTaskId: string | null = null
export let mutationsSinceNote = 0

export function resetState(): void {
  activeSessionId = null
  activeTaskId = null
  mutationsSinceNote = 0
  cachedTaskInjection.clear()
  subagentFileRecallCache.clear()
  pendingPrefetchDeliveries.clear()
  prefetchedPaths.clear()
  injectedMemoryIds.clear()
  resetAllSessionEpochs()
}

export function resetClientForTesting(): void {
  mcpClient = undefined
  resetState()
}

export function getTrackingState() {
  return { activeTaskId, activeSessionId, mutationsSinceNote }
}

// ──────────────────────────────────────────────────────────────────────
// Daemon memory advisory (#140)
// ──────────────────────────────────────────────────────────────────────

/** Structured marker the daemon writes on its error/diagnostic channel when
 *  it reports buffer-pool / RSS pressure. Memory payloads and tool-result
 *  rows never carry it, so matching it can never misfire on user content. */
export const DAEMON_MEMORY_ADVICE_MARKER = 'NEUROSTRATA_ADVICE[daemon_memory]'

/** One advisory per session per window: repeated pressure is one message,
 *  not one per errored request. */
const ADVISORY_THROTTLE_MS = 10 * 60 * 1000

let advisoryLastShown = 0

export function resetAdvisoryStateForTesting(): void {
  advisoryLastShown = 0
}

/** Drain the live client's pending advisory (if any). Returns-and-clears. */
export function flushPendingDaemonAdvisory(): string | null {
  return mcpClient?.flushPendingAdvisory() ?? null
}

/** Pull the daemon memory advisory out of the error/diagnostic channel.
 * Only text carrying the marker is inspected — memory payloads and result
 * rows are never matched, so a memory that merely discusses
 * `NEUROSTRATA_BUFFER_POOL_MB` cannot synthesize an advisory. Returns the
 * guidance string once per 10-minute window, or `null` when already shown
 * or absent. */
export function extractDaemonMemoryAdvisory(
  errorChannelText: string | null | undefined,
  nowMs: number = Date.now()
): string | null {
  if (
    !errorChannelText ||
    !errorChannelText.includes(DAEMON_MEMORY_ADVICE_MARKER)
  ) {
    return null
  }
  if (
    advisoryLastShown > 0 &&
    nowMs - advisoryLastShown < ADVISORY_THROTTLE_MS
  ) {
    return null
  }
  advisoryLastShown = nowMs
  return [
    '[NeuroStrata — DAEMON MEMORY ADVISORY]',
    'The daemon reports buffer-pool or RSS pressure.',
    "Raise NEUROSTRATA_BUFFER_POOL_MB (or the daemon's configured buffer pool) and restart the daemon; repeated advisories are suppressed for 10 minutes."
  ].join(' ')
}

// ──────────────────────────────────────────────────────────────────────
// Epoch-tracked session hooks (context / compaction)
// ──────────────────────────────────────────────────────────────────────

export interface SessionEpochState {
  epoch: number
  baselineInjectedEpoch: number
  seenFiles: Set<string>
}

const sessionEpochs = new Map<string, SessionEpochState>()

function sessionEpochState(
  sessionId: string | null | undefined
): SessionEpochState {
  const sid = sessionId ?? '__default__'
  let state = sessionEpochs.get(sid)
  if (!state) {
    state = { epoch: 0, baselineInjectedEpoch: -1, seenFiles: new Set() }
    sessionEpochs.set(sid, state)
  }
  return state
}

/** Current epoch for a session (starts at 0, incremented by compaction). */
export function currentSessionEpoch(
  sessionId: string | null | undefined
): number {
  return sessionEpochState(sessionId).epoch
}

/** Increment the epoch and clear seen files so the next context hook re-injects. */
export function incrementSessionEpoch(
  sessionId: string | null | undefined
): void {
  const st = sessionEpochState(sessionId)
  st.epoch += 1
  st.seenFiles.clear()
  st.baselineInjectedEpoch = -1
}

export function sessionSeenFiles(
  sessionId: string | null | undefined
): Set<string> {
  return sessionEpochState(sessionId).seenFiles
}

export function resetSessionEpoch(sessionId: string | null | undefined): void {
  sessionEpochs.delete(sessionId ?? '__default__')
}

export function resetAllSessionEpochs(): void {
  sessionEpochs.clear()
}

/**
 * `session.hook("context")` handler. Reads `.NeuroStrata/active_session.json`
 * from the project root; when an active task exists and its baseline has not
 * been injected this epoch, injects `[NeuroStrata Active Context] ...` once.
 * Same-epoch repeat calls leave the event untouched.
 */
export async function sessionContextHandler(
  event: any,
  config: ResolvedConfig
): Promise<void> {
  const sessionId = asString(event?.sessionID ?? event?.session_id)
  const st = sessionEpochState(sessionId)
  const projectRoot = effectiveProjectRoot(config)

  let active: Record<string, unknown> | null = null
  const sessionPath = join(projectRoot, '.NeuroStrata', 'active_session.json')
  if (existsSync(sessionPath)) {
    try {
      active = asRecord(JSON.parse(readFileSync(sessionPath, 'utf8')))
    } catch {
      active = null
    }
  }

  const taskId = asString(active?.task_id)
  const goldenRules = Array.isArray(active?.golden_rules)
    ? (active.golden_rules as unknown[]).filter(
        (g): g is string => typeof g === 'string' && g.trim() !== ''
      )
    : []
  if (!active || (!taskId && goldenRules.length === 0)) return // nothing to baseline-inject

  if (st.baselineInjectedEpoch !== st.epoch) {
    st.baselineInjectedEpoch = st.epoch
    const ns = asString(active.namespace) ?? config.namespace
    const parts: string[] = []
    if (taskId) {
      const status = asString(active.status) ?? ''
      parts.push(
        `[NeuroStrata Active Context] Namespace: ${markdownCodeSpanValue(ns)} | Active Task: ${markdownCodeSpanValue(taskId)} [${markdownCodeSpanValue(status)}]`
      )
    }
    if (goldenRules.length > 0) {
      parts.push(
        `[NeuroStrata Golden Rules — ALWAYS ENFORCED]\n${goldenRules.map((g) => `  - ${g}`).join('\n')}`
      )
    }
    let extra = ''
    const injection = asString(active.subagent_prompt_injection)
    if (injection) extra += `\n${injection.trim()}`
    const bridges = active.conceptual_bridges
    if (bridges !== undefined && bridges !== null && Array.isArray(bridges)) {
      const formatted = bridges
        .map((b) => {
          if (typeof b === 'string') return b
          const bridge = b as Record<string, unknown>
          const type = bridge.memory_type ?? bridge.target_type ?? 'memory'
          const id = bridge.id ?? bridge.target_id
          const content = bridge.content ?? bridge.target_summary ?? ''
          const why = bridge.why_connected ?? bridge.path ?? ''
          return `[${type}] ${id}: ${content} (${why})`
        })
        .join('; ')
      extra += `\n[NeuroStrata Conceptual Bridges] ${formatted}`
    }
    const full = parts.join('\n') + extra
    if (typeof event.additionalContext === 'string') {
      event.additionalContext += `\n${full}`
    } else {
      event.additionalContext = full
    }
    if (Array.isArray(event?.context)) {
      event.context.push(full)
    }
  }
  // Same epoch: leave event.additionalContext untouched.
}

/**
 * `session.hook("compaction")` handler. Increments the session epoch and
 * clears seen files, so the very next context hook re-injects the active
 * task / rules baseline.
 */
export async function sessionCompactionHandler(
  event: any,
  config: ResolvedConfig
): Promise<void> {
  const sessionId = asString(event?.sessionID ?? event?.session_id)
  incrementSessionEpoch(sessionId)
}

// ──────────────────────────────────────────────────────────────────────
// Bug 5: execute.before / execute.after recall delivery
// ──────────────────────────────────────────────────────────────────────

export const pendingRecallBlocks: string[] = []
export const pendingRecallByCall = new Map<string, string>()

function extractRecallCallKey(
  primary?: Record<string, unknown>,
  secondary?: Record<string, unknown>
): string | undefined {
  for (const src of [primary, secondary]) {
    if (!src) continue
    const id =
      asString(src.callID) ??
      asString(src.call_id) ??
      asString(src.toolCallId) ??
      asString(src.tool_call_id) ??
      asString(src.id)
    if (id) return `call:${id}`
  }
  return undefined
}

function extractRecallSessionKey(
  primary?: Record<string, unknown>,
  secondary?: Record<string, unknown>
): string | undefined {
  for (const src of [primary, secondary]) {
    if (!src) continue
    const sid = asString(src.sessionID) ?? asString(src.session_id)
    if (sid) return `session:${sid}`
  }
  return undefined
}

function enqueuePendingRecall(
  block: string,
  outerInput?: Record<string, unknown>,
  innerArgs?: Record<string, unknown>
): void {
  if (pendingRecallBlocks.length === 0) {
    pendingRecallByCall.clear()
  }
  pendingRecallBlocks.push(block)
  const key =
    extractRecallCallKey(outerInput, innerArgs) ??
    extractRecallSessionKey(outerInput, innerArgs)
  if (key) {
    pendingRecallByCall.set(key, block)
  }
}

function consumePendingRecall(
  meta?: Record<string, unknown>,
  target?: Record<string, unknown>
): string | undefined {
  if (pendingRecallBlocks.length === 0) {
    pendingRecallByCall.clear()
  }
  const afterTool =
    asString(meta?.tool) ??
    asString(meta?.tool_name) ??
    asString(meta?.name) ??
    asString(target?.tool) ??
    asString(target?.tool_name)
  if (afterTool && !isFileTouchTool(afterTool)) {
    return undefined
  }

  const removeFromArray = (val: string) => {
    const idx = pendingRecallBlocks.indexOf(val)
    if (idx !== -1) pendingRecallBlocks.splice(idx, 1)
  }

  const callKey = extractRecallCallKey(meta, target)
  if (callKey) {
    const matched = pendingRecallByCall.get(callKey)
    if (matched !== undefined) {
      pendingRecallByCall.delete(callKey)
      removeFromArray(matched)
    }
    return matched
  }

  const sessionKey = extractRecallSessionKey(meta, target)
  if (sessionKey) {
    const matched = pendingRecallByCall.get(sessionKey)
    if (matched !== undefined) {
      pendingRecallByCall.delete(sessionKey)
      removeFromArray(matched)
      return matched
    }
    if (pendingRecallByCall.size > 0) {
      return undefined
    }
  }

  const shifted = pendingRecallBlocks.shift()
  if (shifted !== undefined) {
    for (const [k, v] of pendingRecallByCall.entries()) {
      if (v === shifted) {
        pendingRecallByCall.delete(k)
        break
      }
    }
  }
  return shifted
}

function extractPatchPath(raw: unknown): string | undefined {
  if (typeof raw !== 'string') return undefined
  const m = raw.match(
    /(?:^\*\*\*\s+(?:Update|Add|Delete)\s+File:\s*|^---\s+a\/|^\+\+\+\s+b\/)([^\r\n]+)/m
  )
  return m?.[1]?.trim()
}

function extractFilePath(
  args: Record<string, unknown>,
  altArgs?: Record<string, unknown>,
  rawPatch?: unknown
): string | undefined {
  return (
    asString(args.file_path) ??
    asString(args.filePath) ??
    asString(args.path) ??
    asString(args.notebook_path) ??
    extractPatchPath(args.patchText ?? args.patch ?? args.diff ?? args.input) ??
    extractPatchPath(rawPatch) ??
    (altArgs
      ? (asString(altArgs.file_path) ??
        asString(altArgs.filePath) ??
        asString(altArgs.path) ??
        asString(altArgs.notebook_path) ??
        extractPatchPath(
          altArgs.patchText ?? altArgs.patch ?? altArgs.diff ?? altArgs.input
        ))
      : undefined)
  )
}

/**
 * Tool-name extraction for execute.after events. OpenCode V2 passes
 * `event.name`; V1 / Claude / Hermes pass `event.tool` / `event.tool_name`.
 * `event.toolName` is checked first.
 */
export function extractToolName(event: any): string {
  return (
    asString(event?.toolName) ??
    asString(event?.tool) ??
    asString(event?.name) ??
    asString(event?.tool_name) ??
    asString(event?.action) ??
    asString(event?.command) ??
    ''
  )
}

/** Native NeuroStrata control-plane tools: never nudge on these. */
const NEUROSTRATA_NATIVE_TOOLS = new Set([
  'neurostrata_task_claim',
  'neurostrata_task_update',
  'neurostrata_task_complete',
  'neurostrata_add_memory',
  'neurostrata_archive_memory',
  'neurostrata_supersede_memory',
  'neurostrata_update_memory',
  'neurostrata_guard_validate',
  'neurostrata_memories_for_file',
  'neurostrata_locate'
])

/**
 * Read the active task_id from `.NeuroStrata/active_session.json` so the
 * plugin can forward it as parent_task_id on candidate submission, reaching
 * the server-side auto-commit gate (task_id.is_some()) from the live hook.
 */
function readActiveTaskId(projectRoot: string): string | null {
  const sessionPath = join(projectRoot, '.NeuroStrata', 'active_session.json')
  if (!existsSync(sessionPath)) return null
  try {
    const active = asRecord(JSON.parse(readFileSync(sessionPath, 'utf8')))
    return asString(active?.task_id ?? active?.active_task_id) || null
  } catch {
    return null
  }
}

const executeAfterHandler = async (
  output: any,
  secondArg?: any
): Promise<any> => {
  // The host may pass (input, output) or a single `output`/`event` argument
  // whose `.output` holds the real output. Resolve metadata and target object.
  const hasSecondObject =
    secondArg !== undefined &&
    secondArg !== null &&
    typeof secondArg === 'object'
  const meta = asRecord(output)
  let target = hasSecondObject ? secondArg : output
  if (
    !hasSecondObject &&
    target &&
    typeof target === 'object' &&
    target.output !== undefined &&
    typeof target.output === 'object'
  ) {
    target = target.output
  }
  const targetRec = asRecord(target)
  const event = target

  // Resolve the tool name from BOTH arguments (issue #311): in the
  // two-argument (input, output) form Code Mode hosts carry the name on the
  // input argument, while the output argument holds the result text.
  const toolName = extractToolName(event) || extractToolName(output)

  // OpenCode V2 envelope: { name, callID, args, status, result: { content, output, metadata } }
  // Read raw tool text in priority order. OpenCode v2.2.6 delivers
  // `event.result.content` as an array of content parts `[{ type: "text", text: ... }]`.
  const rawText =
    typeof event?.result?.content === 'string'
      ? event.result.content
      : Array.isArray(event?.result?.content)
        ? (event.result.content as any[])
            .filter(
              (p: any) => p && p.type === 'text' && typeof p.text === 'string'
            )
            .map((p: any) => p.text)
            .join('\n')
        : typeof event?.result?.output === 'string'
          ? event.result.output
          : typeof event?.output === 'string'
            ? event.output
            : typeof event?.result === 'string'
              ? event.result
              : typeof event?.response === 'string'
                ? event.response
                : typeof event?.content === 'string'
                  ? event.content
                  : ''

  const sessionId = asString(event?.sessionID ?? event?.session_id)
  const epoch = currentSessionEpoch(sessionId)

  const suffixParts: string[] = []

  // 1. Pending recall block delivered by execute.before (legacy V1 / Claude / Hermes).
  const pendingBlock =
    consumePendingRecall(meta, targetRec) ??
    (targetRec
      ? (targetRec.neurostrataContext as string | undefined)
      : undefined) ??
    (meta ? (meta.neurostrataContext as string | undefined) : undefined)
  if (pendingBlock) suffixParts.push(pendingBlock)

  // 2. Task-intent nudge on non-NeuroStrata-native tools. Read-only
  //  inspection output is not user or assistant speech, so it never nudges
  //  either (issue #311): the reminder shares the intake gate.
  if (
    toolName !== '' &&
    !NEUROSTRATA_NATIVE_TOOLS.has(toolName) &&
    !isReadOnlyInspectionTool(toolName) &&
    config.intentDetection.injectReminder
  ) {
    const signals = detectTaskIntentsLocal(rawText)
    if (signals.length > 0) {
      const markerList = signals.map((s) => s.marker).join(', ')
      suffixParts.push(
        `[NeuroStrata Plugin — TASK INTENT DETECTED]\nDetected intent markers: ${markerList} — consider creating a task to track this work.`
      )
    }
  }

  // 3. JIT file-touch recall (deduplicated per sessionId:epoch:normalizedPath).
  if (toolName !== '' && isFileTouchTool(toolName)) {
    const filePath =
      event?.args?.path ??
      event?.args?.filePath ??
      event?.input?.path ??
      event?.toolInput?.path
    if (filePath) {
      const fp = String(filePath)
      const normalizedPath = pathRelativeToProjectRoot(fp, config)
      const dedupKey = `${sessionId ?? '__no_session__'}:${epoch}:${normalizedPath}`
      const seen = sessionSeenFiles(sessionId)
      if (!seen.has(dedupKey)) {
        let snippets: string[] = []
        try {
          snippets = await prefetchMemoriesForFile(config.namespace, fp, {
            recordLedgerDir: config.projectRoot
          })
          // Only mark the path as seen once the prefetch has actually run;
          // a failed/empty prefetch leaves the path unmarked so a retry
          // re-queries the governing memories.
          seen.add(dedupKey)
        } catch {
          snippets = []
        }
        if (snippets.length > 0) {
          confirmPrefetchDelivery(config.namespace, fp)
          suffixParts.push(
            `[NeuroStrata File Context — ${normalizedPath}]\n${snippets.slice(0, 2).join('\n')}`
          )
        }
      }
    }
  }

  const legacyPrefix = pendingBlock ? `${pendingBlock}\n\n` : ''
  const newSuffix = suffixParts.slice(pendingBlock ? 1 : 0).join('\n\n')
  const combined = rawText
    ? `${legacyPrefix}${rawText}${newSuffix ? '\n\n' + newSuffix : ''}`
    : `${legacyPrefix}${newSuffix}`

  // Only mutate the event when there is actually something to prepend or
  // append; otherwise leave event.result.content / output / content untouched
  // (preserving the tool's original output, including V2 content arrays).
  const hasAnything = !!pendingBlock || newSuffix !== ''
  if (hasAnything && event && typeof event === 'object') {
    // OpenCode V2: mutate event.result.content in-place (and result.output).
    if (event.result && typeof event.result === 'object') {
      if (Array.isArray(event.result.content)) {
        const nonText = (event.result.content as any[]).filter(
          (p: any) => p && p.type !== 'text'
        )
        event.result.content = [...nonText, { type: 'text', text: combined }]
      } else {
        event.result.content = combined
      }
      if (typeof event.result.output === 'string')
        event.result.output = combined
    }
    // Preserve legacy top-level fields for V1 / Claude / Hermes harnesses.
    if (newSuffix !== '') {
      if (typeof event.additionalContext === 'string') {
        event.additionalContext +=
          (event.additionalContext.length ? '\n' : '') + newSuffix
        if (pendingBlock && !event.additionalContext.includes(pendingBlock)) {
          event.additionalContext = `${pendingBlock}\n\n${event.additionalContext}`
        }
      } else {
        event.additionalContext = suffixParts.join('\n\n')
      }
    } else if (pendingBlock && typeof event.additionalContext === 'string') {
      event.additionalContext = pendingBlock
    }
    if (typeof event.output === 'string') event.output = combined
    if (typeof event.content === 'string') event.content = combined
  }

  // Wave 4: extract task-intent obligations from the tool output text and
  // post them fail-soft to the intake ledger (never blocks the tool output).
  if (
    config.intentDetection.enabled &&
    rawText &&
    !isReadOnlyInspectionTool(toolName)
  ) {
    const activeTaskId = readActiveTaskId(effectiveProjectRoot(config))
    const candidates = extractTurnCandidates({
      role: 'assistant',
      content: rawText,
      turn_id: sessionId ?? undefined,
      source_tool: toolName || undefined,
      parent_task_id: activeTaskId ?? undefined
    })
    if (candidates.length > 0) {
      await postTurnCandidates(candidates, config.namespace)
    }
  }

  return target ?? output
}

const executeBeforeHandler = async (input: any): Promise<any> => {
  const toolName: string =
    input?.tool ??
    input?.tool_name ??
    input?.name ??
    input?.action ??
    input?.command ??
    ''
  const args = firstRecord(input?.input, input?.args, input)
  const altArgs = asRecord(input?.args)
  addSessionWorktree(toolName, args, effectiveProjectRoot(config))

  // Bug 3: OpenCode V2 Code Mode execution inspection.
  if (isCodeExecTool(toolName)) {
    const code = String(
      input?.input?.code ?? input?.args?.code ?? input?.code ?? ''
    )
    const inspection = inspectCodeModeExecution(code)
    if (inspection.id) {
      cachedTaskInjection.delete(inspection.id)
      activeTaskId = inspection.id
    }
    if (inspection.sessionId) activeSessionId = inspection.sessionId
    if (inspection.taskClaimed && !inspection.id) {
      // Variable id: the real task id is only known to the daemon. Drop the
      // stale id so the next check re-resolves it via activeSessionId.
      activeTaskId = null
    }
    if (
      inspection.taskClaimed ||
      inspection.taskCompleted ||
      inspection.note !== undefined
    ) {
      mutationsSinceNote = 0
    }
    if (inspection.taskCompleted) {
      if (activeTaskId) cachedTaskInjection.delete(activeTaskId)
      activeTaskId = null
    }
    if (inspection.memoryMutated) {
      cachedTaskInjection.clear()
      prefetchedPaths.clear()
      injectedMemoryIds.clear()
      subagentFileRecallCache.clear()
    }
    for (const fp of inspection.queriedFiles) {
      try {
        await prefetchMemoriesForFile(config.namespace, fp)
      } catch {
        // prefetch never blocks execution
      }
    }
  }

  // Track native task mutation tools (claim/update/complete) and memory mutations.
  trackNativeTaskToolCall(toolName, firstRecord(args, altArgs), asRecord(input))

  // Memory mutation tools (archive/add/update) invalidate all cached prompts and file recall.
  if (MEMORY_MUTATION_TOOLS.has(toolName)) {
    cachedTaskInjection.clear()
    prefetchedPaths.clear()
    injectedMemoryIds.clear()
    subagentFileRecallCache.clear()
  }

  // Enhancement 1: resolve active task from daemon before subagent injection & mutating guard.
  if (!activeTaskId && (isSubagentTool(toolName) || isMutatingTool(toolName))) {
    try {
      await resolveActiveTaskFromDaemon(config.namespace)
    } catch {
      // fail-soft
    }
  }

  // Enhancement 2 & 3: subagent tool prompt enrichment.
  if (isSubagentTool(toolName)) {
    const targets: Record<string, unknown>[] = []
    if (input?.input && typeof input.input === 'object') {
      targets.push(input.input as Record<string, unknown>)
    }
    if (input?.args && typeof input.args === 'object') {
      targets.push(input.args as Record<string, unknown>)
    }
    if (targets.length === 0 && input && typeof input === 'object') {
      targets.push(input as Record<string, unknown>)
    }
    for (const target of targets) {
      try {
        await enrichAndInjectSubagentPrompt(
          target,
          config.namespace,
          activeTaskId
        )
      } catch {
        injectSubagentPrompt(
          target,
          config.namespace,
          activeTaskId,
          undefined,
          {
            agentId: resolveSubagentAgentId(target),
            codeMode: detectCodeMode(pluginCtx, config)
          }
        )
      }
    }
  }

  // Bug 5: file-touch recall.
  let snippets: string[] = []
  let filePath: string | undefined
  if (isFileTouchTool(toolName)) {
    filePath = extractFilePath(args, altArgs, input?.input)
    if (filePath) {
      try {
        snippets = await prefetchMemoriesForFile(config.namespace, filePath)
      } catch {
        snippets = []
      }
    }
  }
  let block: string | undefined
  if (snippets.length > 0 && filePath) {
    block = formatFileTouchRecallBlock(filePath, snippets)
    input.additionalContext = block
    input.neurostrataContext = block
  }

  // Non-mutating file-touch with recalled snippets -> return the recall block.
  if (snippets.length > 0 && !isMutatingTool(toolName)) {
    if (block) enqueuePendingRecall(block, asRecord(input), args)
    if (filePath) confirmPrefetchDelivery(config.namespace, filePath)
    return { additionalContext: block, neurostrataContext: block, snippets }
  }

  // Mutating tool guard (fail closed). A provably read-only / verification
  // shell segment is NOT a mutation: tell the daemon so it does not apply the
  // Unpreflighted or Breath gates (Lock 2 completes the task before push, so
  // demanding an active task for `git push` would be an unbreakable Catch-22).
  if (isMutatingTool(toolName)) {
    const payload = JSON.stringify(args)
    // Same forwarding rule as the hook path: a raw patch string under
    // `input` is the analysable surface (#333/B2 exec bridge).
    const rawStringInput =
      typeof input?.input === 'string' && input.input.trim() !== ''
        ? input.input
        : null
    const guardPayload = rawStringInput ?? payload
    const shellExempt = isZeroActionExemptShellPayload(toolName, payload)
    if (config.strictMode) {
      const call: any = {
        name: toolName,
        args,
        session_id:
          asString(input?.session_id) ??
          asString(input?.sessionID) ??
          activeSessionId ??
          undefined
      }
      const projectRoot = effectiveProjectRoot(config)
      const cwd = asString(args.cwd) ?? asString(args.workdir) ?? projectRoot
      const session_id = call.session_id
      const active_task_id = asString(args.active_task_id) ?? activeTaskId
      const worktree = asString(args.worktree) ?? cwd
      if (!mcpClient) {
        throw new Error(
          `Guard validation unavailable (NeuroStrata core degraded): no MCP client`
        )
      }
      try {
        const text = await mcpClient.callTool('neurostrata_guard_validate', {
          namespace: config.namespace,
          session_id,
          active_task_id,
          worktree,
          action_type: toolName,
          payload: guardPayload,
          cwd,
          is_mutating: !shellExempt,
          use_sandbox: false,
          mutations_since_note: mutationsSinceNote
        })
        const result = parseGuardValidate(text)
        if (result.verdict === 'reject') {
          const reason =
            result.reasons.join('; ') ||
            result.constraints.join('; ') ||
            result.bucket
          throw new Error(`Zero-Action Start violation: ${reason}`)
        }
      } catch (err) {
        const message = String(err)
        // The recall block never reaches the caller on a rejection, so the
        // prefetch dedup state is rolled back: an allowed retry must still
        // see the governing rules.
        if (filePath) restorePrefetchDelivery(config.namespace, filePath)
        if (
          message.includes('Zero-Action Start violation:') ||
          message.includes('Guard rejected')
        )
          throw err
        throw new Error(
          `Guard validation unavailable (NeuroStrata core degraded): ${message}`
        )
      }
    }
    if (!shellExempt) mutationsSinceNote++
  }

  if (filePath) confirmPrefetchDelivery(config.namespace, filePath)
  if (block) {
    enqueuePendingRecall(block, asRecord(input), args)
    return { additionalContext: block, neurostrataContext: block, snippets }
  }
  return input
}

// ──────────────────────────────────────────────────────────────────────
// Plugin entry point
// ──────────────────────────────────────────────────────────────────────

let initializeResolveGuard = false

export async function initialize(ctx: any): Promise<void> {
  pluginCtx = ctx
  const projectRoot = resolveProjectRoot(ctx)
  const diskConfig = loadPluginConfigFromDisk(projectRoot)
  const inputConfig = asRecord(ctx?.config ?? ctx?.pluginConfig ?? {})
  // Host-supplied ctx.config wins over discovered disk config.
  config = resolveConfig({ ...diskConfig, ...inputConfig }, projectRoot)
  config.projectRoot = projectRoot
  const token = config.bearerToken ?? loadDaemonToken(config.tokenFile)
  mcpClient = new NeuroStrataMCPClient({
    endpoint: config.endpoint,
    token,
    timeoutMs: config.requestTimeoutMs
  })
  cachedTaskInjection.clear()
  subagentFileRecallCache.clear()

  setStatus({
    state: 'uninitialized',
    endpoint: config.endpoint,
    namespace: config.namespace,
    authenticated: token !== null,
    missingTools: [],
    lastError: undefined
  })

  const registerHook = async (
    name: string,
    handler: (input: any) => Promise<any>
  ) => {
    if (ctx?.tool?.hook && typeof ctx.tool.hook === 'function') {
      await ctx.tool.hook(name, handler)
    } else if (ctx?.hooks && typeof ctx.hooks[name] === 'function') {
      await ctx.hooks[name](handler)
    }
  }

  // Register execute.after FIRST, execute.before SECOND (tests overwrite registeredHook
  // on each ctx.tool.hook call and expect execute.before as the last registered hook).
  await registerHook('execute.after', executeAfterHandler)
  await registerHook('execute.before', executeBeforeHandler)

  // OpenCode V2 session hooks: context injects the active-task baseline once per
  // epoch; compaction bumps the epoch so the next context hook re-injects it.
  if (typeof ctx?.session?.hook === 'function') {
    ctx.session.hook('context', (event: any) =>
      sessionContextHandler(event, config)
    )
    ctx.session.hook('compaction', (event: any) =>
      sessionCompactionHandler(event, config)
    )
  }

  // Claude Code style hook events.
  if (ctx?.hooks?.PreToolUse && typeof ctx.hooks.PreToolUse === 'function') {
    await ctx.hooks.PreToolUse(async (event: Record<string, unknown>) =>
      handleHookEvent(event)
    )
  }

  // Fix 2: Register beforeToolExec, beforeToolCall, beforeCommand, and onTurnEnd hooks
  // (used by Codex / GitHub CLI hosts that route tool calls through these hook names).
  if (
    ctx?.hooks?.beforeToolExec &&
    typeof ctx.hooks.beforeToolExec === 'function'
  ) {
    await ctx.hooks.beforeToolExec(executeBeforeHandler)
  }
  if (
    ctx?.hooks?.beforeToolCall &&
    typeof ctx.hooks.beforeToolCall === 'function'
  ) {
    await ctx.hooks.beforeToolCall(executeBeforeHandler)
  }
  if (
    ctx?.hooks?.beforeCommand &&
    typeof ctx.hooks.beforeCommand === 'function'
  ) {
    await ctx.hooks.beforeCommand(executeBeforeHandler)
  }
  if (ctx?.hooks?.onTurnEnd && typeof ctx.hooks.onTurnEnd === 'function') {
    await ctx.hooks.onTurnEnd(onTurnEnd)
  }

  await negotiateCapabilities()

  console.error('[NeuroStrata] plugin initialized', {
    namespace: config.namespace,
    endpoint: config.endpoint,
    authenticated: status.authenticated,
    state: status.state,
    missingTools: status.missingTools
  })
}

async function negotiateCapabilities(): Promise<void> {
  if (!mcpClient) return
  try {
    const available = new Set(await mcpClient.listToolNames())
    const missing = REQUIRED_TOOLS.filter((t) => !available.has(t))
    setStatus({
      state: missing.length > 0 ? 'degraded' : 'ready',
      missingTools: missing,
      lastError: undefined
    })
    if (missing.length > 0) {
      console.error(
        'NeuroStrata core does not expose the tools this shim needs',
        { missing }
      )
    }
  } catch (err) {
    setStatus({ state: 'unavailable', lastError: String(err) })
    console.error('NeuroStrata core is unreachable; the shim is degraded', {
      error: String(err)
    })
  }
}

// ──────────────────────────────────────────────────────────────────────
// Bug 4: handleHookEvent (and alias handleClaudeHookEvent)
// ──────────────────────────────────────────────────────────────────────

export async function handleHookEvent(
  event: Record<string, unknown>
): Promise<Record<string, unknown>> {
  // Stateless CLI invocations (cli-hook.mjs): auto-initialize a default client so
  // handleHookEvent works even if initialize() was never called with a full host context.
  if (!mcpClient) {
    config = resolveConfig({})
    const token = loadDaemonToken(config.tokenFile)
    mcpClient = new NeuroStrataMCPClient({
      endpoint: config.endpoint,
      token,
      timeoutMs: config.requestTimeoutMs
    })
  }

  const hookEventName =
    asString(event.hook_event_name) ?? asString(event.eventName) ?? 'PreToolUse'
  const toolNameRaw =
    asString(event.tool_name) ?? asString(event.toolName) ?? ''
  const toolInputRaw = event.tool_input ?? event.input ?? event.args ?? {}
  const toolInput = firstRecord(toolInputRaw)

  if (hookEventName === 'PreToolUse') {
    // task_claim / get_snapshot get the session's project folder as
    // `worktree`; the allow below carries the changed input.
    addSessionWorktree(toolNameRaw, toolInput, effectiveProjectRoot(config))
    // Track native task mutation tools (claim/update/complete) for state bookkeeping.
    trackNativeTaskToolCall(
      toolNameRaw,
      firstRecord(toolInput),
      asRecord(event)
    )

    // Code execution: inspect OpenCode V2 Code Mode to update task/session state.
    if (isCodeExecTool(toolNameRaw)) {
      const code = String(
        toolInput.code ?? asRecord(toolInput.input)?.code ?? ''
      )
      const inspection = inspectCodeModeExecution(code)
      if (inspection.id) {
        cachedTaskInjection.delete(inspection.id)
        activeTaskId = inspection.id
      }
      if (inspection.sessionId) activeSessionId = inspection.sessionId
      if (inspection.taskClaimed && !inspection.id) {
        // Variable id: let the next check re-resolve from activeSessionId.
        activeTaskId = null
      }
      if (
        inspection.taskClaimed ||
        inspection.taskCompleted ||
        inspection.note !== undefined
      ) {
        mutationsSinceNote = 0
      }
      if (inspection.taskCompleted) {
        if (activeTaskId) cachedTaskInjection.delete(activeTaskId)
        activeTaskId = null
      }
      if (inspection.memoryMutated) {
        cachedTaskInjection.clear()
        prefetchedPaths.clear()
        injectedMemoryIds.clear()
        subagentFileRecallCache.clear()
      }
      for (const fp of inspection.queriedFiles) {
        try {
          await prefetchMemoriesForFile(config.namespace, fp)
        } catch {
          // prefetch never blocks execution
        }
      }
    }

    // Subagent tools: enrich & inject subagent prompt, return allow + updatedInput.
    if (isSubagentTool(toolNameRaw)) {
      await enrichAndInjectSubagentPrompt(
        toolInput,
        config.namespace,
        activeTaskId
      )
      return {
        continue: true,
        hookSpecificOutput: {
          hookEventName: 'PreToolUse',
          permissionDecision: 'allow',
          updatedInput: toolInput
        }
      }
    }

    // File-touch tools: prefetch governing memories, build recall block.
    let recallBlock: string | undefined
    let recalledFilePath: string | undefined
    if (isFileTouchTool(toolNameRaw)) {
      const filePath = extractFilePath(toolInput, undefined, toolInputRaw)
      if (filePath) {
        recalledFilePath = filePath
        try {
          const snippets = await prefetchMemoriesForFile(
            config.namespace,
            filePath
          )
          if (snippets.length > 0)
            recallBlock = formatFileTouchRecallBlock(filePath, snippets)
        } catch {
          // prefetch failures are fail-soft
        }
      }
    }

    // A denial never delivers the recall block, so the prefetch dedup state
    // has to be rolled back: an allowed retry must still see the governing
    // rules. The block is attached to the denial as well, so the agent reads
    // the rules that blocked it.
    const denyWithRecall = (reason: string) => {
      if (recalledFilePath)
        restorePrefetchDelivery(config.namespace, recalledFilePath)
      return {
        ...(recallBlock ? { systemMessage: recallBlock } : {}),
        hookSpecificOutput: {
          hookEventName: 'PreToolUse',
          permissionDecision: 'deny',
          permissionDecisionReason: reason,
          ...(recallBlock ? { additionalContext: recallBlock } : {})
        }
      }
    }

    // Mutating tools: resolve active task from daemon if unset, run guard_validate,
    // deny on reject or on guard-unavailable errors.
    if (isMutatingTool(toolNameRaw)) {
      if (!activeTaskId) {
        try {
          await resolveActiveTaskFromDaemon(config.namespace)
        } catch {
          // fail-soft
        }
      }
      const callArgs = firstRecord(toolInput)
      const payload = JSON.stringify(callArgs)
      // A raw-string tool input (Codex apply_patch) is discarded by
      // firstRecord; forward the text itself so the daemon can scan the
      // patch body instead of receiving "{}" (#333/B2).
      const guardPayload =
        typeof toolInputRaw === 'string' && toolInputRaw.trim() !== ''
          ? toolInputRaw
          : payload
      const shellExempt = isZeroActionExemptShellPayload(toolNameRaw, payload)
      if (config.strictMode) {
        const projectRoot = effectiveProjectRoot(config)
        const cwd =
          asString(callArgs.cwd) ?? asString(callArgs.workdir) ?? projectRoot
        const session_id =
          asString(callArgs.session_id) ??
          asString(event.session_id) ??
          activeSessionId ??
          undefined
        const active_task_id = asString(callArgs.active_task_id) ?? activeTaskId
        const worktree = asString(callArgs.worktree) ?? cwd
        try {
          if (!mcpClient) throw new Error('no MCP client')
          const text = await mcpClient.callTool('neurostrata_guard_validate', {
            namespace: config.namespace,
            session_id,
            active_task_id,
            worktree,
            action_type: toolNameRaw,
            payload: guardPayload,
            cwd,
            is_mutating: !shellExempt,
            use_sandbox: false,
            mutations_since_note: mutationsSinceNote
          })
          const result = parseGuardValidate(text)
          if (result.verdict === 'reject') {
            const reason =
              result.reasons.join('; ') ||
              result.constraints.join('; ') ||
              result.bucket
            return denyWithRecall(reason)
          }
        } catch (err) {
          const message = String(err)
          if (
            message.includes('Zero-Action Start violation:') ||
            message.includes('Guard rejected')
          ) {
            return denyWithRecall(message)
          }
          return denyWithRecall(`Guard unavailable: ${message}`)
        }
      }
      if (!shellExempt) mutationsSinceNote++
    }
    if (recalledFilePath)
      confirmPrefetchDelivery(config.namespace, recalledFilePath)
    return {
      continue: true,
      ...(recallBlock ? { systemMessage: recallBlock } : {}),
      hookSpecificOutput: {
        hookEventName: 'PreToolUse',
        permissionDecision: 'allow',
        updatedInput: toolInput,
        ...(recallBlock ? { additionalContext: recallBlock } : {})
      }
    }
  }

  if (['Stop', 'SubagentStop'].includes(hookEventName)) {
    const transcript =
      asString(event.transcript) ??
      asString(event.content) ??
      asString(event.last_assistant_message)
    if (hookEventName === 'Stop' && transcript) {
      await onTurnEnd({ role: 'assistant', content: transcript })
    }
    return {
      hookSpecificOutput: { hookEventName, permissionDecision: 'allow' }
    }
  }

  return { hookSpecificOutput: { hookEventName, permissionDecision: 'allow' } }
}

export const handleClaudeHookEvent = handleHookEvent

// ──────────────────────────────────────────────────────────────────────
// Turn hook + context plan
// ──────────────────────────────────────────────────────────────────────

export async function onTurnEnd(turn: {
  role: 'user' | 'assistant'
  content: string
}): Promise<void> {
  if (!config.intentDetection.enabled) return
  const text = turn.content
  if (text.includes('neurostrata_task_create')) return
  const signals = detectTaskIntentsLocal(text)
  if (signals.length === 0) return
  if (!config.intentDetection.injectReminder) return
  const markerList = signals.map((s) => s.marker).join(', ')
  console.error(
    `[NeuroStrata] Task intent detected (${markerList}) but no neurostrata_task_create in this turn. Consider creating a task to track this work.`
  )
}

export async function fetchContextPlan(opts: {
  namespace: string
  query?: string
  file_path?: string
  max_items?: number
  max_bytes?: number
  format?: 'json' | 'text'
}): Promise<string> {
  if (!mcpClient) throw new Error('NeuroStrata MCP client not initialized')
  return await mcpClient.callTool('neurostrata_context_plan', opts)
}

// ──────────────────────────────────────────────────────────────────────
// Adapter factory
// ──────────────────────────────────────────────────────────────────────

export function createHarnessPlugin(
  pluginId = 'neurostrata',
  harnessName = 'OpenCode'
) {
  activeHarnessName = harnessName
  return {
    id: pluginId,
    name: harnessName,
    setup: initialize,
    initialize,
    handleHookEvent,
    handleClaudeHookEvent,
    onTurnEnd,
    fetchContextPlan,
    adapterStatus
  }
}

export default createHarnessPlugin()
