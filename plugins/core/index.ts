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

// ──────────────────────────────────────────────────────────────────────
// Node built-ins only (ESM). Zero require(), zero external runtime deps.
// ──────────────────────────────────────────────────────────────────────

import {
  closeSync,
  existsSync,
  fstatSync,
  lstatSync,
  mkdirSync,
  openSync,
  readFileSync,
  readSync,
  realpathSync,
  renameSync,
  statSync,
  unlinkSync,
  writeFileSync
} from 'node:fs'
import { createHash } from 'node:crypto'
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
 *  3. process.env.NEUROSTRATA_NAMESPACE (trimmed, one quote pair stripped and
 *     validated by `usableNamespace`, the CLI's `usable_env_namespace` rule)
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
  const envNamespace = process.env.NEUROSTRATA_NAMESPACE
  const rawInputNamespace = asString(input.namespace)
  // A CLI hook runner copies NEUROSTRATA_NAMESPACE into config.namespace.
  // Validate that copy with the same rule as the env fallback, so an invalid
  // environment value cannot win before the fallback is reached (#400 finding).
  const inputNamespace =
    envNamespace !== undefined && rawInputNamespace === asString(envNamespace)
      ? usableNamespace(envNamespace)
      : rawInputNamespace
  const namespace =
    findRootMarker(resolve(cwd))?.namespace ??
    inputNamespace ??
    usableNamespace(envNamespace) ??
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

/** Harness configs already reported unreadable in this process. */
const warnedHarnessConfigs = new Set<string>()

function warnUnreadableHarnessConfig(path: string, reason: string): void {
  if (warnedHarnessConfigs.has(path)) return
  warnedHarnessConfigs.add(path)
  console.error(
    `[NeuroStrata] warning: the harness config ${path} could not be read (${reason}); the namespace is unknown and the hook fails closed`
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
      found.push(number ? number[0] : (text[k] ?? ''))
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
  // Strip quote runs by index: a quantified multi-character class (["']+)
  // is flagged as polynomial ReDoS by CodeQL even when linear in practice.
  const trimmed = raw.trim()
  let start = 0
  let end = trimmed.length
  while (start < end && (trimmed[start] === '"' || trimmed[start] === "'"))
    start++
  while (end > start && (trimmed[end - 1] === '"' || trimmed[end - 1] === "'"))
    end--
  const ns = trimmed.slice(start, end)
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

/** Canonical `child` inside canonical `parent`, compared by path components. */
function canonicalInside(child: string, parent: string): boolean {
  if (child === parent) return true
  const unix = parent.endsWith('/') ? parent : parent + '/'
  const win = parent.endsWith('\\') ? parent : parent + '\\'
  return child.startsWith(unix) || child.startsWith(win)
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
  const unreadable = (reason: string): MarkerRead => ({
    valid: false,
    path,
    reason
  })
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
  if (
    typeof doc.namespace !== 'string' ||
    !NAMESPACE_PATTERN.test(doc.namespace)
  ) {
    return unreadable('its namespace is missing or not a namespace')
  }
  if (
    sourceId !== undefined &&
    sourceId !== null &&
    typeof sourceId !== 'string'
  ) {
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
    const allowed = [mainTop]
    const mainCanonical = canonicalPath(mainTop) ?? mainTop
    let dir = mainTop
    for (const part of pathParts(relative(top, start))) {
      dir = join(dir, part)
      // Same guard as the Rust main_checkout_walk: a symlink/junction or a
      // folder that canonicalizes outside the main checkout belongs to
      // another project, so its markers must not enter this walk.
      let realDir: string | null = null
      try {
        const st = lstatSync(dir)
        if (st.isSymbolicLink() || !st.isDirectory()) break
        realDir = canonicalPath(dir)
      } catch {
        break
      }
      if (realDir === null || !canonicalInside(realDir, mainCanonical)) break
      if (existsSync(join(dir, '.git'))) break
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
const HARNESS_CONFIG_FILES: Record<string, { file: string[]; key: string[] }> =
  {
    claude: {
      file: ['.NeuroStrata', 'plugins', 'claude', 'neurostrata.json'],
      key: ['namespace']
    },
    opencode: {
      file: ['.opencode', 'neurostrata-plugin.json'],
      key: ['neurostrata', 'namespace']
    },
    codex: {
      file: ['.codex', 'plugins', 'neurostrata.json'],
      key: ['namespace']
    },
    hermes: {
      file: ['.hermes', 'plugins', 'neurostrata.json'],
      key: ['namespace']
    },
    openclaw: {
      file: ['.openclaw', 'plugins', 'neurostrata.json'],
      key: ['namespace']
    },
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
 * `<root>/.NeuroStrata/plugins/<harness>/`. The session's checkout is read
 * FIRST, then the installer's root: a shared runner must not answer for
 * another project (#378 F5). A config that exists but cannot be read or
 * parsed is reported and fails closed, never falling through to env/folder
 * discovery (#378 F9).
 */
export function readHarnessConfig(
  harness: string,
  projectDir: string,
  runnerDir?: string
): { namespace?: string } {
  const spec = HARNESS_CONFIG_FILES[harness]
  if (!spec) return {}
  const roots: Array<string | null> = []
  if (typeof projectDir === 'string' && projectDir !== '') {
    const start = canonicalThroughExisting(resolve(projectDir))
    const top = checkoutTop(start)
    roots.push(...(top === null ? [start] : [top, linkedMainCheckout(top)]))
  }
  // Session-checkout roots come first; the runner's install root is only a
  // last resort for a checkout with no config of its own (#378 F5).
  const sessionRootCount = roots.length
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
  // A config that exists in the session checkout but yields no usable
  // namespace must not fall through to the runner root: that would bind the
  // session to the runner's project (#400 finding).
  let sessionConfigFound = false
  for (let i = 0; i < roots.length; i++) {
    const root = roots[i]
    if (root === null) continue
    if (i >= sessionRootCount && sessionConfigFound) break
    const file = join(root, ...spec.file)
    let text: string
    try {
      text = readFileSync(file, 'utf8').replace(/^\uFEFF/, '')
    } catch (err) {
      // A missing config is normal; an existing one we cannot read is not:
      // guessing from env or the folder name would bind the wrong namespace.
      if ((err as { code?: string })?.code === 'ENOENT') continue
      const reason = String((err as Error)?.message ?? err)
      warnUnreadableHarnessConfig(file, reason)
      throw new Error(
        `the project's namespace is unknown because the harness config ${file} could not be read: ${reason}`
      )
    }
    let value: unknown
    try {
      value = JSON.parse(text)
    } catch (err) {
      const reason = `it is not valid JSON (${String((err as Error)?.message ?? err)})`
      warnUnreadableHarnessConfig(file, reason)
      throw new Error(
        `the project's namespace is unknown because the harness config ${file} could not be read: ${reason}`
      )
    }
    for (const key of spec.key) value = asRecord(value)[key]
    const namespace = usableNamespace(value)
    if (namespace) return { namespace }
    if (i < sessionRootCount) sessionConfigFound = true
  }
  return {}
}

/** How many SessionStart folders the Claude runner remembers. */
const CLAUDE_SESSION_DIRS_KEEP = 20

/**
 * True only when `stateDir` is a real `.NeuroStrata` directly under `root`,
 * every step a real directory (no link/junction), and `sessionPath` is absent
 * or a real file. A repo-controlled link can point into another project, so
 * SessionStart must not read or write session state through one (#378 F8).
 */
function safeProjectStateDir(
  root: string,
  stateDir: string,
  sessionPath: string
): boolean {
  try {
    if (lstatSync(root).isSymbolicLink()) return false
    let cursor = root
    for (const part of pathParts(relative(root, stateDir))) {
      cursor = join(cursor, part)
      const st = lstatSync(cursor)
      if (st.isSymbolicLink() || !st.isDirectory()) return false
    }
    const realRoot = canonicalPath(root)
    const realState = canonicalPath(stateDir)
    if (realRoot === null || realState === null) return false
    if (realState !== join(realRoot, '.NeuroStrata')) return false
    try {
      const st = lstatSync(sessionPath)
      if (st.isSymbolicLink() || !st.isFile()) return false
    } catch (err) {
      if ((err as { code?: string })?.code !== 'ENOENT') return false
    }
    return true
  } catch {
    return false
  }
}

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
  // Refuse a linked/non-project state directory before touching the file at
  // all: reading it can seed a session from another checkout (#378 F8).
  if (!safeProjectStateDir(root, stateDir, sessionPath)) return cwd
  let state: Record<string, unknown> = {}
  let unparsable = false
  try {
    const parsed: unknown = JSON.parse(readFileSync(sessionPath, 'utf8'))
    if (
      typeof parsed === 'object' &&
      parsed !== null &&
      !Array.isArray(parsed)
    ) {
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
const SESSION_SCOPED_TOOLS = [
  'neurostrata_task_claim',
  'neurostrata_get_snapshot'
]

/**
 * The bare `neurostrata_<tool>` name of a NeuroStrata MCP tool, however a
 * harness spells it (`mcp__<server>__neurostrata_task_claim` in Claude Code,
 * `<server>_neurostrata_task_claim` in OpenCode, `<server>.neurostrata_task_claim`).
 * Any other name comes back unchanged. Every check against a NeuroStrata
 * tool name goes through this, or the prefixed names real harnesses send
 * never match.
 */
export function canonicalToolName(toolName: string): string {
  const name = String(toolName ?? '')
  // The last `neurostrata_` that starts the name or follows a separator. A
  // string search, not a backtracking regex: the name comes from the host.
  const at = name.lastIndexOf('neurostrata_')
  if (at < 0 || (at > 0 && !'_.:'.includes(name[at - 1]))) return name
  const bare = name.slice(at)
  return /^neurostrata_[A-Za-z0-9]\w*$/.test(bare) ? bare : name
}

/**
 * Whether `toolName` is one of [SESSION_SCOPED_TOOLS], bare or as a harness
 * spells an MCP tool (see [canonicalToolName]).
 */
export function isSessionScopedTool(toolName: string): boolean {
  return SESSION_SCOPED_TOOLS.includes(canonicalToolName(toolName))
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
  if (args === null || typeof args !== 'object' || Array.isArray(args))
    return false
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
// Generational context-graph working set
// ──────────────────────────────────────────────────────────────────────

export const WORKING_SET_MAX_FRAME_BYTES = 4096
export const WORKING_SET_MAX_IDLE_TURNS = 3
export const WORKING_SET_STUB_MAX_BYTES = 192
export const WORKING_SET_MAX_ROOTS = 32
export const WORKING_SET_MAX_YOUNG_GEN = 64
export const WORKING_SET_MAX_STUBS = 128
export const WORKING_SET_MAX_TOOL_STUBS = 128
export const WORKING_SET_MAX_TOOL_DIGESTS = 128
export const WORKING_SET_MAX_EVENT_IDS = 128
export const WORKING_SET_MAX_INJECTION_KEYS = 128
export const WORKING_SET_MAX_ACTIVE_SESSION_BYTES = 1048576
export const TOOL_GC_INLINE_THRESHOLD_BYTES = 3072
export const TOOL_GC_HEAD_BYTES = 512
export const TOOL_GC_TAIL_BYTES = 512
export const TOOL_GC_STUB_MAX_BYTES = 512
export const WORKING_SET_SUMMARY_MAX_BYTES = 512
export const WORKING_SET_FRAME_OPEN = '[NeuroStrata Active Context]'
export const WORKING_SET_FRAME_CLOSE = '[NeuroStrata End Active Context]'
export const WORKING_SET_TIER0_MAX_BYTES = 2048
export const WORKING_SET_MAX_EDGES = 128
export const WORKING_SET_STRIDE_MESSAGES = 8
export const WORKING_SET_HOT_TAIL_MESSAGES = 12
export const WORKING_SET_TIER0_OPEN =
  '[NeuroStrata Golden Rules — IMMORTAL PREFIX]'
export const WORKING_SET_TIER0_CLOSE = '[NeuroStrata End Immortal Prefix]'
export const WORKING_SET_STRIDE_MARKER =
  '[NeuroStrata Quantized History Stride '

export type WorkingSetNodeKind =
  | 'task_header'
  | 'golden_rule'
  | 'file'
  | 'symbol'
  | 'memory'
  | 'ast_summary'
  | 'conceptual_bridge'
  | 'recall_text'
export type WorkingSetTier =
  | 'Immortal'
  | 'PinnedRoot'
  | 'YoungGen'
  | 'EvictedStub'
export type WorkingSetGcAction =
  | 'retain_full'
  | 'promote_to_young_gen'
  | 'demote_to_stub'
  | 'retain_stub'
export type WorkingSetCacheTier =
  | 'Tier0SystemPrefix'
  | 'Tier1QuantizedHistory'
  | 'Tier2TailDelta'
  | 'EvictedHeapStub'
export type WorkingSetCacheTierTag =
  | 'tier0_system_prefix'
  | 'tier1_quantized_history'
  | 'tier2_tail_delta'
  | 'evicted_heap_stub'
export type WorkingSetStrideAction =
  | 'retain_raw_message'
  | 'fold_into_quantized_stub'
  | 'invalid_message_index'
export type WorkingSetEdgeKind =
  | 'GOVERNS'
  | 'DEPENDS_ON'
  | 'CALLS'
  | 'IMPORTS'
  | 'CONCEPTUAL_BRIDGE'
  | 'EXTRACTED_FROM'
export interface WorkingSetEdge {
  from: string
  to: string
  kind: WorkingSetEdgeKind
  weight_bps: number
  last_touched_turn: number
}
export interface WorkingSetStrideInput {
  total_messages: number
  message_index: number
  hot_tail_count: number
  stride: number
  is_protected_system_or_pinned: boolean
}

export interface WorkingSetScope {
  projectRoot: string
  projectId: string
  namespace: string
  sessionId: string
  epoch: number
}
export interface WorkingSetTouch {
  id: string
  kind: Exclude<WorkingSetNodeKind, 'recall_text'>
  path?: string
  symbol?: string
  memoryId?: string
}
export interface WorkingSetExpansionRequest {
  id: string
  expandTool: string
  input: Record<string, unknown>
}
export interface WorkingSetNode {
  id: string
  kind: WorkingSetNodeKind
  label: string
  summary: string
  governing_ids: string[]
  byte_cost: number
  last_touched_turn: number
  expand_tool: string
  path?: string
  symbol?: string
  memory_type?: string
}
export interface WorkingSetStub {
  id: string
  kind: WorkingSetNodeKind
  pointer: string
  byte_cost: number
  last_touched_turn: number
  governing_ids: string[]
  path?: string
  memory_type?: string
  sha256?: string
}
export interface ToolOutputStub {
  id: string
  tool_name: string
  rel_path: string
  sha256: string
  bytes: number
  epoch: number
  last_touched_turn: number
  governing_ids: string[]
  pointer: string
  byte_cost: number
}
export interface ToolOutputDigest {
  id: string
  project_id: string
  path_key: string
  tool_name: string
  sha256: string
  bytes: number
  epoch: number
  last_touched_turn: number
  read_count: number
}
export interface WorkingSetState {
  version: 1
  project_id: string
  namespace: string
  session_id: string
  epoch: number
  turn_seq: number
  active_turn_id: string | null
  recent_event_ids: string[]
  max_frame_bytes: 4096
  max_idle_turns: number
  used_frame_bytes: number
  frame_fingerprint: string
  last_injected_fingerprint: string
  roots: WorkingSetNode[]
  young_gen: WorkingSetNode[]
  evicted_stubs: WorkingSetStub[]
  edges: WorkingSetEdge[]
  tool_output_stubs: ToolOutputStub[]
  tool_output_digests: ToolOutputDigest[]
}
export interface ActiveSessionState {
  namespace?: string
  session_id?: string
  task_id?: string
  status?: string
  golden_rules?: string[]
  conceptual_bridges?: unknown[]
  subagent_prompt_injection?: string
  claude_session_dirs?: Record<string, string>
  working_set?: WorkingSetState
  [key: string]: unknown
}
export type WorkingSetExpansion = (
  request: WorkingSetExpansionRequest
) => Promise<WorkingSetNode | WorkingSetStub | null>

const U32_MAX = 0xffffffff
const HEX64 = /^[0-9a-f]{64}$/
const HEX12 = /^[0-9a-f]{12}$/
const CONTROL_CHARS = /[\u0000-\u001f\u007f-\u009f]/u
const NODE_KINDS = new Set<WorkingSetNodeKind>([
  'task_header',
  'golden_rule',
  'file',
  'symbol',
  'memory',
  'ast_summary',
  'conceptual_bridge',
  'recall_text'
])
const EXPAND_TOOLS = new Set([
  'neurostrata_get_memory',
  'neurostrata_memories_for_file',
  'neurostrata_related',
  'neurostrata_get_snapshot'
])
const IMMORTAL_KINDS = new Set<WorkingSetNodeKind>([
  'task_header',
  'golden_rule'
])
const PINNED_KINDS = new Set<WorkingSetNodeKind>(['file', 'symbol'])
const YOUNG_KINDS = new Set<WorkingSetNodeKind>([
  'memory',
  'ast_summary',
  'conceptual_bridge'
])
const STATE_KEYS = [
  'version',
  'project_id',
  'namespace',
  'session_id',
  'epoch',
  'turn_seq',
  'active_turn_id',
  'recent_event_ids',
  'max_frame_bytes',
  'max_idle_turns',
  'used_frame_bytes',
  'frame_fingerprint',
  'last_injected_fingerprint',
  'roots',
  'young_gen',
  'evicted_stubs',
  'edges',
  'tool_output_stubs',
  'tool_output_digests'
]
const EDGE_KINDS = new Set<WorkingSetEdgeKind>([
  'GOVERNS',
  'DEPENDS_ON',
  'CALLS',
  'IMPORTS',
  'CONCEPTUAL_BRIDGE',
  'EXTRACTED_FROM'
])
const EDGE_KEYS = ['from', 'to', 'kind', 'weight_bps', 'last_touched_turn']
const ONE_HOP_EDGE_KINDS = new Set<WorkingSetEdgeKind>([
  'GOVERNS',
  'EXTRACTED_FROM',
  'CONCEPTUAL_BRIDGE'
])
const STRUCTURAL_EDGE_KINDS = new Set<WorkingSetEdgeKind>([
  'CALLS',
  'IMPORTS',
  'DEPENDS_ON'
])
const NODE_REQUIRED = [
  'id',
  'kind',
  'label',
  'summary',
  'governing_ids',
  'byte_cost',
  'last_touched_turn',
  'expand_tool'
]
const NODE_OPTIONAL = ['path', 'symbol', 'memory_type']
const STUB_REQUIRED = [
  'id',
  'kind',
  'pointer',
  'byte_cost',
  'last_touched_turn',
  'governing_ids'
]
const STUB_OPTIONAL = ['path', 'memory_type', 'sha256']
const TOOL_STUB_KEYS = [
  'id',
  'tool_name',
  'rel_path',
  'sha256',
  'bytes',
  'epoch',
  'last_touched_turn',
  'governing_ids',
  'pointer',
  'byte_cost'
]
const TOOL_DIGEST_KEYS = [
  'id',
  'project_id',
  'path_key',
  'tool_name',
  'sha256',
  'bytes',
  'epoch',
  'last_touched_turn',
  'read_count'
]

function wsRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null
}
function wsExactKeys(
  value: Record<string, unknown>,
  required: string[],
  optional: string[] = []
): boolean {
  const allowed = new Set([...required, ...optional])
  return (
    required.every((key) => Object.hasOwn(value, key)) &&
    Object.keys(value).every((key) => allowed.has(key))
  )
}
function wsU32(value: unknown): value is number {
  return (
    Number.isSafeInteger(value) &&
    (value as number) >= 0 &&
    (value as number) <= U32_MAX
  )
}
function wsText(
  value: unknown,
  maxBytes: number,
  singleLine = false
): value is string {
  return (
    typeof value === 'string' &&
    Buffer.byteLength(value) <= maxBytes &&
    !CONTROL_CHARS.test(value) &&
    (!singleLine || !/[\r\n]/u.test(value))
  )
}
function wsNormalizedText(text: string): string {
  return text.replace(/\r\n/g, '\n')
}
function wsEscapePointerToken(value: string): string {
  return value.replace(
    /[% ,|\[\]\r\n]/gu,
    (char) => `%${Buffer.from(char).toString('hex').toUpperCase()}`
  )
}
function wsEscapeFrameToken(value: string): string {
  return value.replace(
    /[%\[\]]/gu,
    (char) => `%${Buffer.from(char).toString('hex').toUpperCase()}`
  )
}
function wsTruncateUtf8(text: string, maxBytes: number): string {
  if (maxBytes <= 0) return ''
  let used = 0
  let out = ''
  for (const char of text) {
    const size = Buffer.byteLength(char)
    if (used + size > maxBytes) break
    out += char
    used += size
  }
  return out
}
function wsTailUtf8(text: string, maxBytes: number): string {
  if (maxBytes <= 0) return ''
  const chars = Array.from(text)
  let used = 0
  let start = chars.length
  while (start > 0) {
    const size = Buffer.byteLength(chars[start - 1])
    if (used + size > maxBytes) break
    start--
    used += size
  }
  return chars.slice(start).join('')
}
function wsSha(text: string): string {
  return createHash('sha256').update(text, 'utf8').digest('hex')
}
function wsCanonicalProjectRoot(projectRoot: string): string | null {
  try {
    const real = realpathSync.native(projectRoot)
    const normalized = real.replace(/\\/g, '/')
    return normalized.length > 1 ? normalized.replace(/\/+$/u, '') : normalized
  } catch {
    return null
  }
}
function wsProjectId(projectRoot: string): string | null {
  const canonical = wsCanonicalProjectRoot(projectRoot)
  return canonical === null ? null : wsSha(canonical)
}
function wsScope(value: WorkingSetScope): WorkingSetScope | null {
  const rec = wsRecord(value)
  if (!rec) return null
  const projectRoot = wsCanonicalProjectRoot(value.projectRoot)
  const projectId = wsProjectId(value.projectRoot)
  if (
    projectRoot === null ||
    projectId === null ||
    value.projectId !== projectId
  )
    return null
  if (!wsText(value.namespace, 128) || !wsText(value.sessionId, 256))
    return null
  if (!wsU32(value.epoch)) return null
  return {
    projectRoot,
    projectId,
    namespace: value.namespace,
    sessionId: value.sessionId,
    epoch: value.epoch
  }
}
function wsEmptyFingerprint(): string {
  return '0'.repeat(64)
}
function wsBlankState(scope: WorkingSetScope): WorkingSetState {
  return {
    version: 1,
    project_id: scope.projectId,
    namespace: scope.namespace,
    session_id: scope.sessionId,
    epoch: scope.epoch,
    turn_seq: 0,
    active_turn_id: null,
    recent_event_ids: [],
    max_frame_bytes: WORKING_SET_MAX_FRAME_BYTES,
    max_idle_turns: WORKING_SET_MAX_IDLE_TURNS,
    used_frame_bytes: 0,
    frame_fingerprint: wsEmptyFingerprint(),
    last_injected_fingerprint: wsEmptyFingerprint(),
    roots: [],
    young_gen: [],
    evicted_stubs: [],
    edges: [],
    tool_output_stubs: [],
    tool_output_digests: []
  }
}
function wsFallbackScope(value: WorkingSetScope): WorkingSetScope {
  return {
    projectRoot: resolve(value.projectRoot || process.cwd()),
    projectId: HEX64.test(value.projectId) ? value.projectId : '0'.repeat(64),
    namespace: wsText(value.namespace, 128) ? value.namespace : 'NeuroStrata',
    sessionId: wsText(value.sessionId, 256)
      ? value.sessionId
      : '__no_session__',
    epoch: wsU32(value.epoch) ? value.epoch : 0
  }
}
function wsNodeTier(
  node: WorkingSetNode,
  collection: 'roots' | 'young_gen'
): WorkingSetTier {
  if (collection === 'roots')
    return IMMORTAL_KINDS.has(node.kind) ? 'Immortal' : 'PinnedRoot'
  return 'YoungGen'
}
function wsCanonicalNodeLine(
  node: WorkingSetNode,
  collection: 'roots' | 'young_gen'
): string {
  const prefix = collection === 'roots' ? 'Root' : 'Young'
  return `[${prefix}] ${wsEscapeFrameToken(node.id)} | ${wsEscapeFrameToken(node.summary)} | expand:${wsEscapeFrameToken(node.expand_tool)}\n`
}
function wsTierRank(tier: WorkingSetTier): number {
  return tier === 'Immortal'
    ? 0
    : tier === 'PinnedRoot'
      ? 1
      : tier === 'YoungGen'
        ? 2
        : 3
}
function wsNodeCompare(
  a: WorkingSetNode,
  b: WorkingSetNode,
  aTier: WorkingSetTier,
  bTier: WorkingSetTier
): number {
  return (
    wsTierRank(aTier) - wsTierRank(bTier) ||
    b.last_touched_turn - a.last_touched_turn ||
    b.byte_cost - a.byte_cost ||
    (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)
  )
}
function wsStubCompare(a: WorkingSetStub, b: WorkingSetStub): number {
  return (
    b.last_touched_turn - a.last_touched_turn ||
    b.byte_cost - a.byte_cost ||
    (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)
  )
}
function wsToolCompare(
  a: Pick<ToolOutputDigest, 'id' | 'tool_name' | 'sha256'> & {
    path_key?: string
    rel_path?: string
  },
  b: Pick<ToolOutputDigest, 'id' | 'tool_name' | 'sha256'> & {
    path_key?: string
    rel_path?: string
  }
): number {
  return (
    (a.path_key ?? a.rel_path ?? '').localeCompare(
      b.path_key ?? b.rel_path ?? ''
    ) ||
    a.tool_name.localeCompare(b.tool_name) ||
    a.sha256.localeCompare(b.sha256) ||
    a.id.localeCompare(b.id)
  )
}
function wsVisibleIds(value: unknown): string[] | null {
  if (!Array.isArray(value) || value.length > 8) return null
  const ids = value.map((id) => (typeof id === 'string' ? id.trim() : ''))
  if (ids.some((id) => !wsText(id, 256))) return null
  const sorted = [...new Set(ids)].sort()
  return sorted.length === value.length &&
    sorted.every((id, i) => id === value[i])
    ? sorted
    : null
}
function wsRelPath(value: unknown): value is string {
  return (
    wsText(value, 512) &&
    !value.startsWith('/') &&
    !value.includes('\\') &&
    !/^[A-Za-z]:/u.test(value) &&
    value
      .split('/')
      .every((part) => part !== '' && part !== '.' && part !== '..')
  )
}
function wsValidateNode(
  value: unknown,
  collection: 'roots' | 'young_gen'
): WorkingSetNode | null {
  const rec = wsRecord(value)
  if (!rec || !wsExactKeys(rec, NODE_REQUIRED, NODE_OPTIONAL)) return null
  if (!wsText(rec.id, 1024) || !NODE_KINDS.has(rec.kind as WorkingSetNodeKind))
    return null
  if (
    !wsText(rec.label, 128, true) ||
    !wsText(rec.summary, WORKING_SET_SUMMARY_MAX_BYTES, true)
  )
    return null
  const governing_ids = wsVisibleIds(rec.governing_ids)
  if (
    governing_ids === null ||
    !wsU32(rec.byte_cost) ||
    rec.byte_cost < 1 ||
    !wsU32(rec.last_touched_turn)
  )
    return null
  if (typeof rec.expand_tool !== 'string' || !EXPAND_TOOLS.has(rec.expand_tool))
    return null
  if (Object.hasOwn(rec, 'path') && !wsRelPath(rec.path)) return null
  if (Object.hasOwn(rec, 'symbol') && !wsText(rec.symbol, 256, true))
    return null
  if (Object.hasOwn(rec, 'memory_type') && !wsText(rec.memory_type, 64, true))
    return null
  const kind = rec.kind as WorkingSetNodeKind
  if (
    collection === 'roots' &&
    !IMMORTAL_KINDS.has(kind) &&
    !PINNED_KINDS.has(kind)
  )
    return null
  if (collection === 'young_gen' && !YOUNG_KINDS.has(kind)) return null
  const node: WorkingSetNode = {
    id: rec.id,
    kind,
    label: rec.label,
    summary: rec.summary,
    governing_ids,
    byte_cost: rec.byte_cost,
    last_touched_turn: rec.last_touched_turn,
    expand_tool: rec.expand_tool,
    ...(Object.hasOwn(rec, 'path') ? { path: rec.path as string } : {}),
    ...(Object.hasOwn(rec, 'symbol') ? { symbol: rec.symbol as string } : {}),
    ...(Object.hasOwn(rec, 'memory_type')
      ? { memory_type: rec.memory_type as string }
      : {})
  }
  if (
    node.byte_cost !== Buffer.byteLength(wsCanonicalNodeLine(node, collection))
  )
    return null
  return node
}
function wsValidateStub(value: unknown): WorkingSetStub | null {
  const rec = wsRecord(value)
  if (!rec || !wsExactKeys(rec, STUB_REQUIRED, STUB_OPTIONAL)) return null
  if (!wsText(rec.id, 1024) || !NODE_KINDS.has(rec.kind as WorkingSetNodeKind))
    return null
  if (
    !wsText(rec.pointer, WORKING_SET_STUB_MAX_BYTES, true) ||
    !wsU32(rec.byte_cost) ||
    !wsU32(rec.last_touched_turn)
  )
    return null
  const governing_ids = wsVisibleIds(rec.governing_ids)
  if (
    governing_ids === null ||
    rec.byte_cost !== Buffer.byteLength(`${rec.pointer}\n`)
  )
    return null
  if (Object.hasOwn(rec, 'path') && !wsRelPath(rec.path)) return null
  if (Object.hasOwn(rec, 'memory_type') && !wsText(rec.memory_type, 64, true))
    return null
  if (Object.hasOwn(rec, 'sha256') && !HEX12.test(rec.sha256 as string))
    return null
  const kind = rec.kind as WorkingSetNodeKind
  if (
    (kind === 'file' || kind === 'symbol') &&
    !HEX12.test(rec.sha256 as string)
  )
    return null
  return {
    id: rec.id,
    kind,
    pointer: rec.pointer,
    byte_cost: rec.byte_cost,
    last_touched_turn: rec.last_touched_turn,
    governing_ids,
    ...(Object.hasOwn(rec, 'path') ? { path: rec.path as string } : {}),
    ...(Object.hasOwn(rec, 'memory_type')
      ? { memory_type: rec.memory_type as string }
      : {}),
    ...(Object.hasOwn(rec, 'sha256') ? { sha256: rec.sha256 as string } : {})
  }
}
function wsNormalizeToolName(value: string): string | null {
  const tool = value.trim().toLowerCase()
  return /^[a-z][a-z0-9_]*$/u.test(tool) ? tool : null
}
function wsValidateToolStub(value: unknown): ToolOutputStub | null {
  const rec = wsRecord(value)
  if (!rec || !wsExactKeys(rec, TOOL_STUB_KEYS)) return null
  if (typeof rec.id !== 'string' || !/^tool:[0-9a-f]{64}$/u.test(rec.id))
    return null
  if (wsNormalizeToolName(rec.tool_name as string) !== rec.tool_name)
    return null
  if (!wsRelPath(rec.rel_path) && rec.rel_path !== '-') return null
  if (
    !HEX12.test(rec.sha256 as string) ||
    !wsU32(rec.bytes) ||
    !wsU32(rec.epoch) ||
    !wsU32(rec.last_touched_turn)
  )
    return null
  const governing_ids = wsVisibleIds(rec.governing_ids)
  if (
    governing_ids === null ||
    !wsText(rec.pointer, TOOL_GC_STUB_MAX_BYTES, true) ||
    !wsU32(rec.byte_cost)
  )
    return null
  if (rec.byte_cost !== Buffer.byteLength(`${rec.pointer}\n`)) return null
  return rec as unknown as ToolOutputStub
}
function wsValidateDigest(value: unknown): ToolOutputDigest | null {
  const rec = wsRecord(value)
  if (!rec || !wsExactKeys(rec, TOOL_DIGEST_KEYS)) return null
  if (typeof rec.id !== 'string' || !/^tool:[0-9a-f]{64}$/u.test(rec.id))
    return null
  if (!HEX64.test(rec.project_id as string)) return null
  if (!wsRelPath(rec.path_key) && rec.path_key !== '-') return null
  if (wsNormalizeToolName(rec.tool_name as string) !== rec.tool_name)
    return null
  if (
    !HEX12.test(rec.sha256 as string) ||
    !wsU32(rec.bytes) ||
    !wsU32(rec.epoch) ||
    !wsU32(rec.last_touched_turn)
  )
    return null
  if (!wsU32(rec.read_count) || rec.read_count < 1) return null
  return rec as unknown as ToolOutputDigest
}
function wsCanonicalNodeId(value: unknown): value is string {
  return (
    wsText(value, 1024) &&
    /^(?:task|rule|file|symbol|mem|bridge|text|ast):/u.test(value as string)
  )
}
function wsValidateEdge(value: unknown): WorkingSetEdge | null {
  const rec = wsRecord(value)
  if (!rec || !wsExactKeys(rec, EDGE_KEYS)) return null
  if (
    !wsCanonicalNodeId(rec.from) ||
    !wsCanonicalNodeId(rec.to) ||
    !EDGE_KINDS.has(rec.kind as WorkingSetEdgeKind) ||
    !wsU32(rec.weight_bps) ||
    rec.weight_bps > 10000 ||
    !wsU32(rec.last_touched_turn)
  )
    return null
  return {
    from: rec.from,
    to: rec.to,
    kind: rec.kind as WorkingSetEdgeKind,
    weight_bps: rec.weight_bps,
    last_touched_turn: rec.last_touched_turn
  }
}
function wsEdgeCompare(a: WorkingSetEdge, b: WorkingSetEdge): number {
  return (
    b.last_touched_turn - a.last_touched_turn ||
    b.weight_bps - a.weight_bps ||
    (a.from < b.from ? -1 : a.from > b.from ? 1 : 0) ||
    (a.kind < b.kind ? -1 : a.kind > b.kind ? 1 : 0) ||
    (a.to < b.to ? -1 : a.to > b.to ? 1 : 0)
  )
}
function wsEdgeKey(edge: WorkingSetEdge): string {
  return `${edge.from}\u0000${edge.kind}\u0000${edge.to}`
}
function wsSameOrder<T>(
  values: T[],
  sorted: T[],
  compare: (a: T, b: T) => number
): boolean {
  if (values.length !== sorted.length) return false
  for (let i = 0; i < values.length; i++)
    if (compare(values[i], sorted[i]) !== 0) return false
  return true
}
function wsValidateState(
  value: unknown,
  scope: WorkingSetScope
): WorkingSetState | null {
  const rec = wsRecord(value)
  if (
    !rec ||
    !wsExactKeys(
      rec,
      STATE_KEYS.filter((key) => key !== 'edges'),
      ['edges']
    )
  )
    return null
  if (
    rec.version !== 1 ||
    rec.project_id !== scope.projectId ||
    rec.namespace !== scope.namespace ||
    rec.session_id !== scope.sessionId ||
    rec.epoch !== scope.epoch
  )
    return null
  if (
    !wsU32(rec.turn_seq) ||
    !wsU32(rec.used_frame_bytes) ||
    rec.used_frame_bytes > WORKING_SET_MAX_FRAME_BYTES
  )
    return null
  if (
    rec.max_frame_bytes !== WORKING_SET_MAX_FRAME_BYTES ||
    !wsU32(rec.max_idle_turns) ||
    rec.max_idle_turns > 30
  )
    return null
  if (
    !HEX64.test(rec.frame_fingerprint as string) ||
    !HEX64.test(rec.last_injected_fingerprint as string)
  )
    return null
  if (rec.active_turn_id !== null && !wsText(rec.active_turn_id, 128))
    return null
  if (
    !Array.isArray(rec.recent_event_ids) ||
    rec.recent_event_ids.length > WORKING_SET_MAX_EVENT_IDS
  )
    return null
  if (
    rec.recent_event_ids.some((id) => !wsText(id, 256)) ||
    new Set(rec.recent_event_ids).size !== rec.recent_event_ids.length
  )
    return null
  if (
    !Array.isArray(rec.roots) ||
    !Array.isArray(rec.young_gen) ||
    !Array.isArray(rec.evicted_stubs) ||
    (Object.hasOwn(rec, 'edges') && !Array.isArray(rec.edges)) ||
    !Array.isArray(rec.tool_output_stubs) ||
    !Array.isArray(rec.tool_output_digests)
  )
    return null
  const rawEdges = (Object.hasOwn(rec, 'edges') ? rec.edges : []) as unknown[]
  if (rawEdges.length > WORKING_SET_MAX_EDGES) return null
  const roots = rec.roots.map((node) => wsValidateNode(node, 'roots'))
  const young = rec.young_gen.map((node) => wsValidateNode(node, 'young_gen'))
  const stubs = rec.evicted_stubs.map(wsValidateStub)
  const edges = rawEdges.map(wsValidateEdge)
  const toolStubs = rec.tool_output_stubs.map(wsValidateToolStub)
  const digests = rec.tool_output_digests.map(wsValidateDigest)
  if (
    [...roots, ...young].some((node) => node === null) ||
    stubs.some((stub) => stub === null) ||
    edges.some((edge) => edge === null) ||
    toolStubs.some((stub) => stub === null) ||
    digests.some((digest) => digest === null)
  )
    return null
  const rootNodes = roots as WorkingSetNode[]
  const youngNodes = young as WorkingSetNode[]
  const stubNodes = stubs as WorkingSetStub[]
  const edgeNodes = edges as WorkingSetEdge[]
  const toolStubNodes = toolStubs as ToolOutputStub[]
  const digestNodes = digests as ToolOutputDigest[]
  const allIds = [...rootNodes, ...youngNodes, ...stubNodes].map(
    (node) => node.id
  )
  if (new Set(allIds).size !== allIds.length) return null
  if (
    new Set(edgeNodes.map(wsEdgeKey)).size !== edgeNodes.length ||
    !wsSameOrder(edgeNodes, [...edgeNodes].sort(wsEdgeCompare), wsEdgeCompare)
  )
    return null
  if (
    !wsSameOrder(
      rootNodes,
      [...rootNodes].sort((a, b) =>
        wsNodeCompare(a, b, wsNodeTier(a, 'roots'), wsNodeTier(b, 'roots'))
      ),
      (a, b) =>
        wsNodeCompare(a, b, wsNodeTier(a, 'roots'), wsNodeTier(b, 'roots'))
    )
  )
    return null
  if (
    !wsSameOrder(
      youngNodes,
      [...youngNodes].sort((a, b) =>
        wsNodeCompare(a, b, 'YoungGen', 'YoungGen')
      ),
      (a, b) => wsNodeCompare(a, b, 'YoungGen', 'YoungGen')
    )
  )
    return null
  if (
    !wsSameOrder(stubNodes, [...stubNodes].sort(wsStubCompare), wsStubCompare)
  )
    return null
  if (
    !wsSameOrder(
      toolStubNodes,
      [...toolStubNodes].sort((a, b) => wsToolCompare(a, b)),
      (a, b) => wsToolCompare(a, b)
    )
  )
    return null
  if (
    !wsSameOrder(
      digestNodes,
      [...digestNodes].sort(wsToolCompare),
      wsToolCompare
    )
  )
    return null
  if (
    rootNodes.filter((node) => !IMMORTAL_KINDS.has(node.kind)).length >
      WORKING_SET_MAX_ROOTS ||
    youngNodes.length > WORKING_SET_MAX_YOUNG_GEN ||
    stubNodes.length > WORKING_SET_MAX_STUBS ||
    toolStubNodes.length > WORKING_SET_MAX_TOOL_STUBS ||
    digestNodes.length > WORKING_SET_MAX_TOOL_DIGESTS
  )
    return null
  const digestIds = new Set(digestNodes.map((digest) => digest.id))
  if (
    toolStubNodes.length !== digestNodes.length ||
    toolStubNodes.some((stub) => !digestIds.has(stub.id))
  )
    return null
  if (
    digestNodes.some(
      (digest) =>
        digest.project_id !== scope.projectId || digest.epoch !== scope.epoch
    ) ||
    toolStubNodes.some((stub) => stub.epoch !== scope.epoch)
  )
    return null
  return {
    version: 1,
    project_id: scope.projectId,
    namespace: scope.namespace,
    session_id: scope.sessionId,
    epoch: scope.epoch,
    turn_seq: rec.turn_seq,
    active_turn_id: rec.active_turn_id,
    recent_event_ids: [...rec.recent_event_ids],
    max_frame_bytes: WORKING_SET_MAX_FRAME_BYTES,
    max_idle_turns: rec.max_idle_turns,
    used_frame_bytes: rec.used_frame_bytes,
    frame_fingerprint: rec.frame_fingerprint as string,
    last_injected_fingerprint: rec.last_injected_fingerprint as string,
    roots: rootNodes,
    young_gen: youngNodes,
    evicted_stubs: stubNodes,
    edges: edgeNodes,
    tool_output_stubs: toolStubNodes,
    tool_output_digests: digestNodes
  }
}
function wsNormalizeState(
  value: unknown,
  scope: WorkingSetScope
): WorkingSetState {
  return wsValidateState(value, scope) ?? wsBlankState(scope)
}
function wsReadBounded(path: string): string | null {
  let fd: number | null = null
  try {
    fd = openSync(path, 'r')
    const stat = fstatSync(fd)
    if (!stat.isFile() || stat.size > WORKING_SET_MAX_ACTIVE_SESSION_BYTES)
      return null
    const buffer = Buffer.alloc(WORKING_SET_MAX_ACTIVE_SESSION_BYTES + 1)
    let read = 0
    while (read < buffer.length) {
      const count = readSync(fd, buffer, read, buffer.length - read, read)
      if (count === 0) break
      read += count
    }
    if (read > WORKING_SET_MAX_ACTIVE_SESSION_BYTES) return null
    return buffer.subarray(0, read).toString('utf8')
  } catch {
    return null
  } finally {
    if (fd !== null)
      try {
        closeSync(fd)
      } catch {
        /* best effort */
      }
  }
}
const warnedWorkingSetParents = new Set<string>()
function wsReadParent(scope: WorkingSetScope): {
  path: string
  status: 'absent' | 'valid' | 'invalid'
  parent: Record<string, unknown> | null
} {
  const stateDir = join(scope.projectRoot, '.NeuroStrata')
  const path = join(stateDir, 'active_session.json')
  if (
    !isDirectory(stateDir) ||
    !safeProjectStateDir(scope.projectRoot, stateDir, path)
  ) {
    return { path, status: 'absent', parent: null }
  }
  if (!existsSync(path)) return { path, status: 'absent', parent: null }
  const text = wsReadBounded(path)
  let parent: Record<string, unknown> | null = null
  if (text !== null) {
    try {
      parent = wsRecord(JSON.parse(text))
    } catch {
      parent = null
    }
  }
  if (parent === null) {
    if (!warnedWorkingSetParents.has(path)) {
      warnedWorkingSetParents.add(path)
      console.error(
        `[NeuroStrata] warning: ignoring invalid active session state at ${path}`
      )
    }
    return { path, status: 'invalid', parent: null }
  }
  return { path, status: 'valid', parent }
}

/** Mirrors the first-match Rust/Lean working-set decision table exactly. */
export function evaluateWorkingSetEviction(input: {
  tier: WorkingSetTier
  turn_age: number
  max_idle_turns: number
  node_bytes: number
  projected_frame_bytes: number
  max_frame_bytes: number
  is_re_referenced: boolean
}): WorkingSetGcAction {
  if (input.tier === 'Immortal') return 'retain_full'
  if (input.tier === 'EvictedStub') {
    return input.is_re_referenced &&
      input.projected_frame_bytes <= input.max_frame_bytes
      ? 'promote_to_young_gen'
      : 'retain_stub'
  }
  if (input.tier === 'YoungGen') {
    return (!input.is_re_referenced && input.turn_age > input.max_idle_turns) ||
      input.projected_frame_bytes > input.max_frame_bytes
      ? 'demote_to_stub'
      : 'retain_full'
  }
  return !input.is_re_referenced &&
    input.turn_age > 0 &&
    input.projected_frame_bytes > input.max_frame_bytes
    ? 'demote_to_stub'
    : 'retain_full'
}

/** Rust `WorkingSetCacheTier::as_tag` mirror. */
export function workingSetCacheTierAsTag(
  tier: WorkingSetCacheTier
): WorkingSetCacheTierTag {
  switch (tier) {
    case 'Tier0SystemPrefix':
      return 'tier0_system_prefix'
    case 'Tier1QuantizedHistory':
      return 'tier1_quantized_history'
    case 'Tier2TailDelta':
      return 'tier2_tail_delta'
    case 'EvictedHeapStub':
      return 'evicted_heap_stub'
  }
}

/** Mirrors `evaluate_working_set_cache_tier` exactly. Tier 1 is state-only. */
export function evaluateWorkingSetCacheTier(
  tier: WorkingSetTier,
  hasSystemChannel: boolean
): WorkingSetCacheTier {
  if (tier === 'Immortal')
    return hasSystemChannel ? 'Tier0SystemPrefix' : 'Tier2TailDelta'
  if (tier === 'EvictedStub') return 'EvictedHeapStub'
  return 'Tier2TailDelta'
}

/** Mirrors `compute_working_set_quantized_frontier` exactly. */
export function computeWorkingSetQuantizedFrontier(
  totalMessages: number,
  hotTailCount: number,
  stride: number
): number {
  if (stride === 0 || totalMessages <= hotTailCount) return 0
  return Math.floor((totalMessages - hotTailCount) / stride) * stride
}

/** Mirrors `evaluate_working_set_stride_fold` exactly (first match wins). */
export function evaluateWorkingSetStrideFold(
  input: WorkingSetStrideInput
): WorkingSetStrideAction {
  if (input.message_index >= input.total_messages)
    return 'invalid_message_index'
  if (
    input.is_protected_system_or_pinned ||
    input.stride === 0 ||
    input.total_messages <= input.hot_tail_count
  )
    return 'retain_raw_message'
  return input.message_index <
    computeWorkingSetQuantizedFrontier(
      input.total_messages,
      input.hot_tail_count,
      input.stride
    )
    ? 'fold_into_quantized_stub'
    : 'retain_raw_message'
}

/** Load and strictly validate the reconstructible local L1 cache. */
export async function loadWorkingSet(
  scopeInput: WorkingSetScope
): Promise<WorkingSetState> {
  const active = wsScope(scopeInput)
  if (active === null) return wsBlankState(wsFallbackScope(scopeInput))
  const clean = wsBlankState(active)
  const loaded = wsReadParent(active)
  if (
    loaded.status !== 'valid' ||
    !Object.hasOwn(loaded.parent!, 'working_set')
  )
    return clean
  const raw = loaded.parent!.working_set
  const exact = wsValidateState(raw, active)
  if (exact !== null) return exact
  const persisted = wsRecord(raw)
  if (
    !persisted ||
    persisted.project_id !== active.projectId ||
    persisted.namespace !== active.namespace ||
    persisted.session_id !== active.sessionId ||
    !wsU32(persisted.epoch) ||
    persisted.epoch < active.epoch
  )
    return clean
  const adoptedScope = { ...active, epoch: persisted.epoch }
  const adopted = wsValidateState(raw, adoptedScope)
  if (adopted === null) return clean
  adopted.tool_output_stubs = []
  adopted.tool_output_digests = []
  adopted.recent_event_ids = []
  adopted.frame_fingerprint = wsEmptyFingerprint()
  adopted.last_injected_fingerprint = wsEmptyFingerprint()
  return adopted
}

/** Merge only working_set into a valid parent session file, best effort. */
export async function persistWorkingSet(
  scopeInput: WorkingSetScope,
  stateInput: WorkingSetState
): Promise<void> {
  const scope = wsScope(scopeInput)
  if (scope === null) return
  const state = wsValidateState(stateInput, scope)
  if (state === null) return
  const current = wsReadParent(scope)
  if (current.status === 'invalid') return
  const parent = current.parent ?? {}
  parent.working_set = state
  writeJsonAtomically(current.path, parent)
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

function wsStateScope(state: WorkingSetState): WorkingSetScope {
  return {
    projectRoot: '',
    project_id: state.project_id,
    projectId: state.project_id,
    namespace: state.namespace,
    sessionId: state.session_id,
    epoch: state.epoch
  } as WorkingSetScope & { project_id: string }
}
function wsCloneState(state: WorkingSetState): WorkingSetState {
  const cloneNode = (node: WorkingSetNode): WorkingSetNode => ({
    ...node,
    governing_ids: [...node.governing_ids]
  })
  const cloneStub = (stub: WorkingSetStub): WorkingSetStub => ({
    ...stub,
    governing_ids: [...stub.governing_ids]
  })
  return {
    ...state,
    recent_event_ids: [...state.recent_event_ids],
    roots: state.roots.map(cloneNode),
    young_gen: state.young_gen.map(cloneNode),
    evicted_stubs: state.evicted_stubs.map(cloneStub),
    edges: (state.edges ?? []).map((edge) => ({ ...edge })),
    tool_output_stubs: state.tool_output_stubs.map((stub) => ({
      ...stub,
      governing_ids: [...stub.governing_ids]
    })),
    tool_output_digests: state.tool_output_digests.map((digest) => ({
      ...digest
    }))
  }
}
function wsMakeStub(node: WorkingSetNode): WorkingSetStub | null {
  const path = wsEscapePointerToken(node.path ?? '-')
  let pointer: string
  if (node.kind === 'memory') {
    pointer = `[GCStub ${wsEscapePointerToken(node.id)} | ${wsEscapePointerToken(node.memory_type ?? 'memory')} | governs:${path} | expand:neurostrata_get_memory]`
  } else if (node.kind === 'file' || node.kind === 'symbol') {
    const identity =
      node.kind === 'symbol' && node.symbol
        ? node.id.startsWith('symbol:')
          ? node.id
          : `symbol:${node.path ?? '-'}#${node.symbol}`
        : node.id
    pointer = `[GCStub ${wsEscapePointerToken(identity)} | sha256:${wsSha(node.id).slice(0, 12)} | expand:neurostrata_memories_for_file]`
  } else {
    pointer = `[GCStub ${wsEscapePointerToken(node.id)} | ${wsEscapePointerToken(node.kind)} | governs:${path} | expand:${wsEscapePointerToken(node.expand_tool)}]`
  }
  const byte_cost = Buffer.byteLength(`${pointer}\n`)
  if (byte_cost > WORKING_SET_STUB_MAX_BYTES) return null
  return {
    id: node.id,
    kind: node.kind,
    pointer,
    byte_cost,
    last_touched_turn: node.last_touched_turn,
    governing_ids: [...node.governing_ids],
    ...(node.path ? { path: node.path } : {}),
    ...(node.memory_type ? { memory_type: node.memory_type } : {}),
    ...(node.kind === 'file' || node.kind === 'symbol'
      ? { sha256: wsSha(node.id).slice(0, 12) }
      : {})
  }
}
function wsDemote(
  state: WorkingSetState,
  node: WorkingSetNode,
  collection: 'roots' | 'young_gen'
): boolean {
  const stub = wsMakeStub(node)
  if (stub === null || stub.byte_cost >= node.byte_cost) return false
  if (collection === 'roots')
    state.roots = state.roots.filter((entry) => entry.id !== node.id)
  else state.young_gen = state.young_gen.filter((entry) => entry.id !== node.id)
  const prior = state.evicted_stubs.find((entry) => entry.id === node.id)
  if (prior && prior.kind !== node.kind) return false
  state.evicted_stubs = [
    ...state.evicted_stubs.filter((entry) => entry.id !== node.id),
    stub
  ]
  return true
}
function wsCapToolPairs(state: WorkingSetState): void {
  const byId = new Map(
    state.tool_output_digests.map((digest) => [digest.id, digest])
  )
  const pairs = [...byId.values()]
    .sort(
      (a, b) =>
        a.last_touched_turn - b.last_touched_turn || a.id.localeCompare(b.id)
    )
    .slice(-WORKING_SET_MAX_TOOL_DIGESTS)
  const ids = new Set(pairs.map((digest) => digest.id))
  state.tool_output_digests = pairs.sort(wsToolCompare)
  state.tool_output_stubs = state.tool_output_stubs
    .filter((stub) => ids.has(stub.id))
    .sort((a, b) => wsToolCompare(a, b))
}
function wsSortAndCap(state: WorkingSetState): void {
  state.roots.sort((a, b) =>
    wsNodeCompare(a, b, wsNodeTier(a, 'roots'), wsNodeTier(b, 'roots'))
  )
  const immortal = state.roots.filter((node) => IMMORTAL_KINDS.has(node.kind))
  const pinned = state.roots.filter((node) => !IMMORTAL_KINDS.has(node.kind))
  while (pinned.length > WORKING_SET_MAX_ROOTS) {
    const node = pinned.pop()!
    if (!wsDemote(state, node, 'roots'))
      state.roots = state.roots.filter((entry) => entry.id !== node.id)
  }
  state.roots = [...immortal, ...pinned].sort((a, b) =>
    wsNodeCompare(a, b, wsNodeTier(a, 'roots'), wsNodeTier(b, 'roots'))
  )
  state.young_gen.sort((a, b) => wsNodeCompare(a, b, 'YoungGen', 'YoungGen'))
  while (state.young_gen.length > WORKING_SET_MAX_YOUNG_GEN) {
    const node = state.young_gen.pop()!
    if (!wsDemote(state, node, 'young_gen'))
      state.young_gen = state.young_gen.filter((entry) => entry.id !== node.id)
  }
  state.evicted_stubs.sort(wsStubCompare)
  if (state.evicted_stubs.length > WORKING_SET_MAX_STUBS)
    state.evicted_stubs = state.evicted_stubs.slice(0, WORKING_SET_MAX_STUBS)
  wsCapToolPairs(state)
  state.recent_event_ids = state.recent_event_ids.slice(
    0,
    WORKING_SET_MAX_EVENT_IDS
  )
}
function wsChargedBytes(state: WorkingSetState): number {
  const fullBytes = [...state.roots, ...state.young_gen].reduce(
    (sum, node) => sum + node.byte_cost,
    0
  )
  const stubBytes =
    state.evicted_stubs.reduce((sum, stub) => sum + stub.byte_cost, 0) +
    state.tool_output_stubs.reduce((sum, stub) => sum + stub.byte_cost, 0)
  return fullBytes + Math.min(Math.floor(state.max_frame_bytes / 2), stubBytes)
}

/** Apply TTL and deterministic pressure demotion without changing immortal nodes. */
export function sweepWorkingSet(stateInput: WorkingSetState): WorkingSetState {
  const state = wsCloneState(stateInput)
  for (const node of [...state.young_gen]) {
    const age = Math.max(0, state.turn_seq - node.last_touched_turn)
    const current = node.last_touched_turn === state.turn_seq
    const action = evaluateWorkingSetEviction({
      tier: 'YoungGen',
      turn_age: age,
      max_idle_turns: state.max_idle_turns,
      node_bytes: node.byte_cost,
      projected_frame_bytes: wsChargedBytes(state),
      max_frame_bytes: state.max_frame_bytes,
      is_re_referenced: current
    })
    if (action === 'demote_to_stub') wsDemote(state, node, 'young_gen')
  }
  let charged = wsChargedBytes(state)
  if (charged > state.max_frame_bytes) {
    const candidates = [
      ...state.young_gen.map((node) => ({ node, tier: 'YoungGen' as const })),
      ...state.roots
        .filter(
          (node) =>
            !IMMORTAL_KINDS.has(node.kind) &&
            node.last_touched_turn !== state.turn_seq
        )
        .map((node) => ({ node, tier: 'PinnedRoot' as const }))
    ].sort(
      (a, b) =>
        wsTierRank(a.tier) - wsTierRank(b.tier) ||
        Math.max(0, state.turn_seq - b.node.last_touched_turn) -
          Math.max(0, state.turn_seq - a.node.last_touched_turn) ||
        b.node.byte_cost - a.node.byte_cost ||
        (a.node.id < b.node.id ? -1 : a.node.id > b.node.id ? 1 : 0)
    )
    for (const candidate of candidates) {
      if (charged <= state.max_frame_bytes) break
      const age = Math.max(0, state.turn_seq - candidate.node.last_touched_turn)
      const action = evaluateWorkingSetEviction({
        tier: candidate.tier,
        turn_age: age,
        max_idle_turns: state.max_idle_turns,
        node_bytes: candidate.node.byte_cost,
        projected_frame_bytes: charged,
        max_frame_bytes: state.max_frame_bytes,
        is_re_referenced: candidate.node.last_touched_turn === state.turn_seq
      })
      if (action !== 'demote_to_stub') continue
      const collection = candidate.tier === 'YoungGen' ? 'young_gen' : 'roots'
      const before = charged
      if (wsDemote(state, candidate.node, collection))
        charged = wsChargedBytes(state)
      if (charged >= before) continue
    }
  }
  const liveIds = new Set(
    [...state.roots, ...state.young_gen, ...state.evicted_stubs].map(
      (node) => node.id
    )
  )
  state.edges = state.edges
    .filter((edge) => liveIds.has(edge.from) && liveIds.has(edge.to))
    .sort(wsEdgeCompare)
    .slice(0, WORKING_SET_MAX_EDGES)
  wsSortAndCap(state)
  return state
}

/** Record typed graph edges with deterministic dedup, ordering, and cap. */
export function recordWorkingSetEdges(
  stateInput: WorkingSetState,
  edges: Array<
    Partial<WorkingSetEdge> & Pick<WorkingSetEdge, 'from' | 'to' | 'kind'>
  >
): WorkingSetState {
  const state = wsCloneState(stateInput)
  const seen = new Set<string>()
  const collected: WorkingSetEdge[] = []
  for (const edge of [...edges, ...(state.edges ?? [])]) {
    if (!wsCanonicalNodeId(edge.from) || !wsCanonicalNodeId(edge.to)) continue
    if (!EDGE_KINDS.has(edge.kind)) continue
    const weight = wsU32(edge.weight_bps) ? (edge.weight_bps as number) : 10000
    if (weight > 10000) continue
    const last = wsU32(edge.last_touched_turn)
      ? (edge.last_touched_turn as number)
      : state.turn_seq
    if (!wsU32(last)) continue
    const candidate: WorkingSetEdge = {
      from: edge.from,
      to: edge.to,
      kind: edge.kind,
      weight_bps: weight,
      last_touched_turn: last
    }
    const key = wsEdgeKey(candidate)
    if (seen.has(key)) continue
    seen.add(key)
    collected.push(candidate)
  }
  state.edges = collected.sort(wsEdgeCompare).slice(0, WORKING_SET_MAX_EDGES)
  return state
}

function wsTouchForId(
  state: WorkingSetState,
  id: string
): WorkingSetTouch | null {
  const node =
    state.roots.find((entry) => entry.id === id) ??
    state.young_gen.find((entry) => entry.id === id)
  if (node) {
    if (node.kind === 'file' || node.kind === 'symbol')
      return {
        id,
        kind: node.kind,
        ...(node.path ? { path: node.path } : {}),
        ...(node.symbol ? { symbol: node.symbol } : {})
      }
    if (node.kind === 'recall_text') return null
    if (node.kind === 'memory')
      return { id, kind: 'memory', memoryId: id.slice(4) }
    return { id, kind: node.kind }
  }
  const stub = state.evicted_stubs.find((entry) => entry.id === id)
  if (stub) {
    if (stub.kind === 'recall_text') return null
    if (stub.kind === 'file' || stub.kind === 'symbol')
      return {
        id,
        kind: stub.kind,
        ...(stub.path ? { path: stub.path } : {})
      }
    if (stub.kind === 'memory')
      return { id, kind: 'memory', memoryId: id.slice(4) }
    return { id, kind: stub.kind }
  }
  if (id.startsWith('file:')) return { id, kind: 'file', path: id.slice(5) }
  if (id.startsWith('symbol:')) {
    const body = id.slice(7)
    const hash = body.indexOf('#')
    return hash === -1
      ? { id, kind: 'symbol', path: body }
      : {
          id,
          kind: 'symbol',
          path: body.slice(0, hash),
          symbol: body.slice(hash + 1)
        }
  }
  if (id.startsWith('mem:'))
    return { id, kind: 'memory', memoryId: id.slice(4) }
  return null
}

function wsIncidentNeighbors(
  state: WorkingSetState,
  id: string,
  kinds: Set<WorkingSetEdgeKind>
): string[] {
  const neighbors = new Set<string>()
  for (const edge of state.edges) {
    if (!kinds.has(edge.kind)) continue
    if (edge.from === id) neighbors.add(edge.to)
    else if (edge.to === id) neighbors.add(edge.from)
  }
  return [...neighbors]
}

/**
 * Deterministic 1-hop normative and 2-hop structural-to-normative spreading.
 * A visited key terminates cycles on both hops; the 128-edge cap bounds work.
 */
function wsSpreadActivationIds(
  state: WorkingSetState,
  touchedIds: string[]
): { activatedIds: string[]; touches: WorkingSetTouch[] } {
  const candidates = touchedIds.filter((id) => {
    const kind = wsTouchForId(state, id)?.kind
    return kind === 'file' || kind === 'symbol' || kind === 'memory'
  })
  const activated = new Set<string>()
  const touches = new Map<string, WorkingSetTouch>()
  const add = (id: string): void => {
    if (id === '' || activated.has(id)) return
    activated.add(id)
    const touch = wsTouchForId(state, id)
    if (touch) touches.set(id, touch)
  }
  for (const u of candidates)
    for (const neighbor of wsIncidentNeighbors(state, u, ONE_HOP_EDGE_KINDS))
      add(neighbor)
  const visited = new Set<string>()
  for (const u of candidates) {
    const kind = wsTouchForId(state, u)?.kind
    if (kind !== 'file' && kind !== 'symbol') continue
    for (const w of wsIncidentNeighbors(state, u, STRUCTURAL_EDGE_KINDS)) {
      if (w === u) continue
      for (const m of wsIncidentNeighbors(
        state,
        w,
        new Set<WorkingSetEdgeKind>(['GOVERNS'])
      )) {
        const key = `${u}\u0000${w}\u0000${m}`
        if (visited.has(key)) continue
        visited.add(key)
        add(m)
      }
    }
  }
  return { activatedIds: [...activated].sort(), touches: [...touches.values()] }
}

function wsNormalizeTouchPath(raw: string, projectRoot: string): string | null {
  const spelling = raw.replace(/\\/g, '/')
  if (CONTROL_CHARS.test(spelling)) return null
  const absolute = resolve(projectRoot, spelling)
  const rel = relative(projectRoot, absolute).replace(/\\/g, '/')
  return wsRelPath(rel) ? rel : null
}
function wsTouchNode(
  touch: WorkingSetTouch,
  turn: number,
  projectRoot: string
): WorkingSetNode | null {
  if (!wsText(touch.id, 1024) || !NODE_KINDS.has(touch.kind)) return null
  const path =
    touch.path === undefined
      ? undefined
      : wsNormalizeTouchPath(touch.path, projectRoot)
  if (touch.path !== undefined && path === null) return null
  if (touch.symbol !== undefined && !wsText(touch.symbol, 256, true))
    return null
  const label = wsTruncateUtf8(
    touch.symbol ?? path ?? touch.memoryId ?? touch.id,
    128
  )
  const summary = wsTruncateUtf8(
    touch.symbol
      ? `${path ?? '-'}#${touch.symbol}`
      : (path ?? touch.memoryId ?? touch.id),
    WORKING_SET_SUMMARY_MAX_BYTES
  )
  const collection =
    IMMORTAL_KINDS.has(touch.kind) || PINNED_KINDS.has(touch.kind)
      ? 'roots'
      : 'young_gen'
  const expand_tool =
    touch.kind === 'task_header' || touch.kind === 'golden_rule'
      ? 'neurostrata_get_snapshot'
      : touch.kind === 'file' || touch.kind === 'symbol'
        ? 'neurostrata_memories_for_file'
        : touch.kind === 'conceptual_bridge' || touch.kind === 'ast_summary'
          ? 'neurostrata_related'
          : 'neurostrata_get_memory'
  const node: WorkingSetNode = {
    id: touch.id,
    kind: touch.kind,
    label,
    summary,
    governing_ids: [],
    byte_cost: 1,
    last_touched_turn: turn,
    expand_tool,
    ...(path ? { path } : {}),
    ...(touch.symbol ? { symbol: touch.symbol } : {})
  }
  node.byte_cost = Buffer.byteLength(wsCanonicalNodeLine(node, collection))
  return node
}
function wsExpansionInput(
  stub: WorkingSetStub,
  namespace: string
): Record<string, unknown> {
  if (stub.kind === 'file' || stub.kind === 'symbol')
    return { namespace, path: stub.path ?? '-' }
  if (stub.kind === 'memory')
    return {
      namespace,
      id: stub.id.startsWith('mem:') ? stub.id.slice(4) : stub.id
    }
  return { namespace, id: stub.id }
}
function wsExpansionTool(stub: WorkingSetStub): string {
  if (stub.kind === 'file' || stub.kind === 'symbol')
    return 'neurostrata_memories_for_file'
  if (stub.kind === 'memory') return 'neurostrata_get_memory'
  return 'neurostrata_related'
}

/** Advance one deterministic turn, mark touches, and retry fitting page faults. */
export async function advanceWorkingSetTurn(
  stateInput: WorkingSetState,
  namespace: string,
  sessionId: string,
  projectRoot: string,
  touched: WorkingSetTouch[],
  expand: WorkingSetExpansion
): Promise<WorkingSetState> {
  const scope = wsScope({
    projectRoot,
    projectId: wsProjectId(projectRoot) ?? '',
    namespace,
    sessionId,
    epoch: stateInput.epoch
  })
  if (
    scope === null ||
    stateInput.project_id !== scope.projectId ||
    stateInput.namespace !== namespace ||
    stateInput.session_id !== sessionId
  )
    return wsCloneState(stateInput)
  const state = wsCloneState(stateInput)
  if (state.turn_seq === U32_MAX) {
    state.turn_seq = 0
    state.active_turn_id = null
    state.recent_event_ids = []
    state.frame_fingerprint = wsEmptyFingerprint()
    state.last_injected_fingerprint = wsEmptyFingerprint()
    workingSetInjectionFingerprints.delete(workingSetInjectionKey(scope))
    for (const node of [...state.roots, ...state.young_gen])
      node.last_touched_turn = 0
    for (const item of [
      ...state.evicted_stubs,
      ...state.edges,
      ...state.tool_output_stubs,
      ...state.tool_output_digests
    ])
      item.last_touched_turn = 0
  } else {
    state.turn_seq++
  }
  const turn = state.turn_seq
  const touches = new Map<string, WorkingSetTouch>()
  for (const touch of touched)
    if (wsText(touch.id, 1024)) touches.set(touch.id, touch)
  const activation = wsSpreadActivationIds(state, [...touches.keys()])
  for (const touch of activation.touches)
    if (!touches.has(touch.id)) touches.set(touch.id, touch)
  for (const touch of touches.values()) {
    const full =
      state.roots.find((node) => node.id === touch.id) ??
      state.young_gen.find((node) => node.id === touch.id)
    if (full) {
      if (full.kind !== touch.kind) continue
      full.last_touched_turn = turn
      continue
    }
    const prior = state.evicted_stubs.find((stub) => stub.id === touch.id)
    if (prior) {
      if (prior.kind !== touch.kind) continue
      prior.last_touched_turn = turn
      try {
        const expanded = await expand({
          id: prior.id,
          expandTool: wsExpansionTool(prior),
          input: wsExpansionInput(prior, namespace)
        })
        const candidate =
          expanded === null
            ? null
            : wsValidateNode(
                expanded,
                YOUNG_KINDS.has(expanded.kind) ? 'young_gen' : 'roots'
              )
        if (
          candidate === null ||
          candidate.id !== prior.id ||
          candidate.kind !== prior.kind
        )
          continue
        const projectedFull =
          wsChargedBytes(state) - prior.byte_cost + candidate.byte_cost
        if (
          evaluateWorkingSetEviction({
            tier: 'EvictedStub',
            turn_age: 0,
            max_idle_turns: state.max_idle_turns,
            node_bytes: candidate.byte_cost,
            projected_frame_bytes: projectedFull,
            max_frame_bytes: state.max_frame_bytes,
            is_re_referenced: true
          }) !== 'promote_to_young_gen'
        )
          continue
        candidate.last_touched_turn = turn
        state.evicted_stubs = state.evicted_stubs.filter(
          (stub) => stub.id !== prior.id
        )
        if (
          IMMORTAL_KINDS.has(candidate.kind) ||
          PINNED_KINDS.has(candidate.kind)
        )
          state.roots.push(candidate)
        else state.young_gen.push(candidate)
      } catch {
        // Expansion is advisory; a denied or unavailable page fault stays resident.
      }
      continue
    }
    const node = wsTouchNode(touch, turn, projectRoot)
    if (node === null) continue
    if (IMMORTAL_KINDS.has(node.kind) || PINNED_KINDS.has(node.kind))
      state.roots.push(node)
    else state.young_gen.push(node)
  }
  return sweepWorkingSet(state)
}
function wsEligibleTool(toolName: string): string | null {
  const normalized = wsNormalizeToolName(toolName)
  if (normalized === null) return null
  const approved = new Set(
    [...READ_FILE_TOOLS].map((tool) => tool.toLowerCase())
  )
  return approved.has(normalized) ? normalized : null
}
function wsToolPath(
  filePath: string | null,
  toolName: string,
  projectRoot?: string
): string | null {
  if (toolName === 'grep' || toolName === 'glob' || toolName === 'mcp_glob')
    return '-'
  if (filePath === null || filePath.trim() === '') return null
  return projectRoot === undefined
    ? wsRelPath(filePath.trim())
      ? filePath.trim()
      : null
    : wsNormalizeTouchPath(filePath, projectRoot)
}
function wsGoverningIds(value: string[]): string[] | null {
  if (!Array.isArray(value) || value.length > 8) return null
  const ids = value.map((id) => (typeof id === 'string' ? id.trim() : ''))
  if (ids.some((id) => !wsText(id, 256))) return null
  return [...new Set(ids)].sort()
}
function wsToolPointer(
  path: string,
  sha256: string,
  bytes: number,
  governingIds: string[]
): string | null {
  const base = `[NeuroStrata GC Stub -> path=${wsEscapePointerToken(path)} | sha256=${sha256} | bytes=${bytes}`
  const withIds = `${base} | governing_ids=[${governingIds.map(wsEscapePointerToken).join(',')}]]`
  const pointer =
    Buffer.byteLength(withIds) <= TOOL_GC_STUB_MAX_BYTES
      ? withIds
      : `${base} | governing_ids=[]]`
  return Buffer.byteLength(pointer) <= TOOL_GC_STUB_MAX_BYTES ? pointer : null
}
function wsOutputSummary(raw: string): { head: string; tail: string } {
  const normalized = wsNormalizedText(raw)
  const bytes = Buffer.byteLength(normalized)
  if (bytes <= 1024) return { head: normalized, tail: '' }
  return {
    head: wsTruncateUtf8(normalized, TOOL_GC_HEAD_BYTES),
    tail: wsTailUtf8(normalized, TOOL_GC_TAIL_BYTES)
  }
}
function wsDehydratedText(raw: string, pointer: string): string {
  const { head, tail } = wsOutputSummary(raw)
  return [
    '[NeuroStrata GC Summary — head]',
    head,
    '[NeuroStrata GC Summary — tail]',
    tail,
    pointer,
    ''
  ].join('\n')
}

/** Pure size-based dehydration; duplicate history belongs to the scoped helper. */
export function dehydrateToolOutput(
  rawText: string,
  toolName: string,
  filePath: string | null,
  governingIds: string[]
): { text: string; dehydrated: boolean; digest?: ToolOutputDigest } {
  const tool = wsEligibleTool(toolName)
  const path = tool === null ? null : wsToolPath(filePath, tool)
  const ids = wsGoverningIds(governingIds)
  if (
    tool === null ||
    path === null ||
    ids === null ||
    typeof rawText !== 'string'
  )
    return { text: rawText, dehydrated: false }
  const normalized = wsNormalizedText(rawText)
  const bytes = Buffer.byteLength(normalized)
  if (bytes <= TOOL_GC_INLINE_THRESHOLD_BYTES)
    return { text: rawText, dehydrated: false }
  const sha256 = wsSha(normalized).slice(0, 12)
  const pointer = wsToolPointer(path, sha256, bytes, ids)
  if (pointer === null) return { text: rawText, dehydrated: false }
  return { text: wsDehydratedText(rawText, pointer), dehydrated: true }
}
function wsRecordToolOutput(
  state: WorkingSetState,
  scope: WorkingSetScope,
  rawText: string,
  toolName: string,
  filePath: string | null,
  governingIds: string[]
): { text: string; dehydrated: boolean; digest?: ToolOutputDigest } {
  const tool = wsEligibleTool(toolName)
  const path =
    tool === null ? null : wsToolPath(filePath, tool, scope.projectRoot)
  const ids = wsGoverningIds(governingIds)
  if (
    tool === null ||
    path === null ||
    ids === null ||
    typeof rawText !== 'string'
  )
    return { text: rawText, dehydrated: false }
  const normalized = wsNormalizedText(rawText)
  const bytes = Buffer.byteLength(normalized)
  const sha256 = wsSha(normalized).slice(0, 12)
  const pointer = wsToolPointer(path, sha256, bytes, ids)
  if (pointer === null) return { text: rawText, dehydrated: false }
  const id = `tool:${wsSha(`${scope.projectId}\n${scope.namespace}\n${scope.sessionId}\n${scope.epoch}\n${tool}\n${path}\n${sha256}`)}`
  const repeated = state.tool_output_digests.some(
    (digest) => digest.path_key === path && digest.epoch === scope.epoch
  )
  const prior = state.tool_output_digests.find((digest) => digest.id === id)
  const digest: ToolOutputDigest = {
    id,
    project_id: scope.projectId,
    path_key: path,
    tool_name: tool,
    sha256,
    bytes,
    epoch: scope.epoch,
    last_touched_turn: state.turn_seq,
    read_count: prior ? Math.min(U32_MAX, prior.read_count + 1) : 1
  }
  const stub: ToolOutputStub = {
    id,
    tool_name: tool,
    rel_path: path,
    sha256,
    bytes,
    epoch: scope.epoch,
    last_touched_turn: state.turn_seq,
    governing_ids: ids,
    pointer,
    byte_cost: Buffer.byteLength(`${pointer}\n`)
  }
  state.tool_output_digests = [
    ...state.tool_output_digests.filter((entry) => entry.id !== id),
    digest
  ]
  state.tool_output_stubs = [
    ...state.tool_output_stubs.filter((entry) => entry.id !== id),
    stub
  ]
  wsCapToolPairs(state)
  const dehydrated = repeated || bytes > TOOL_GC_INLINE_THRESHOLD_BYTES
  return {
    text: dehydrated ? wsDehydratedText(rawText, pointer) : rawText,
    dehydrated,
    digest
  }
}

/** Scoped duplicate-aware read dehydration with digest/stub pair persistence. */
export async function dehydrateRepeatedReadOutput(
  scopeInput: WorkingSetScope,
  rawText: string,
  toolName: string,
  filePath: string | null,
  governingIds: string[]
): Promise<{ text: string; dehydrated: boolean; digest?: ToolOutputDigest }> {
  const scope = wsScope(scopeInput)
  if (scope === null) return { text: rawText, dehydrated: false }
  const state = await loadWorkingSet(scope)
  const result = wsRecordToolOutput(
    state,
    scope,
    rawText,
    toolName,
    filePath,
    governingIds
  )
  const swept = sweepWorkingSet(state)
  await persistWorkingSet(scope, swept)
  return result
}
function wsRecallStub(
  state: WorkingSetState,
  block: string,
  memoryIds: string[]
): { text: string; changed: boolean } {
  const normalized = wsNormalizedText(block)
  const ids = wsGoverningIds(memoryIds)
  if (ids === null) return { text: block, changed: false }
  const resident = new Set(
    [...state.roots, ...state.young_gen, ...state.evicted_stubs].map(
      (node) => node.id
    )
  )
  const allResident =
    ids.length > 0 && ids.every((id) => resident.has(`mem:${id}`))
  const duplicateText =
    ids.length === 0 &&
    state.evicted_stubs.some((stub) => stub.id === `text:${wsSha(normalized)}`)
  if (
    Buffer.byteLength(normalized) <= TOOL_GC_INLINE_THRESHOLD_BYTES &&
    !allResident &&
    !duplicateText
  )
    return { text: block, changed: false }
  const pointers: string[] = []
  const additions: WorkingSetStub[] = []
  if (ids.length > 0) {
    for (const id of ids) {
      const node: WorkingSetNode = {
        id: `mem:${id}`,
        kind: 'memory',
        label: id,
        summary: '',
        governing_ids: [id],
        byte_cost: WORKING_SET_STUB_MAX_BYTES + 1,
        last_touched_turn: state.turn_seq,
        expand_tool: 'neurostrata_get_memory',
        memory_type: 'memory'
      }
      const stub = wsMakeStub(node)
      if (stub === null) continue
      pointers.push(stub.pointer)
      additions.push(stub)
    }
  } else {
    const sha = wsSha(normalized)
    const pointer = `[GCStub text:${sha} | bytes=${Buffer.byteLength(normalized)} | expand:none]`
    const stub: WorkingSetStub = {
      id: `text:${sha}`,
      kind: 'recall_text',
      pointer,
      byte_cost: Buffer.byteLength(`${pointer}\n`),
      last_touched_turn: state.turn_seq,
      governing_ids: []
    }
    pointers.push(pointer)
    additions.push(stub)
  }
  if (pointers.length === 0) return { text: block, changed: false }
  for (const stub of additions)
    state.evicted_stubs = [
      ...state.evicted_stubs.filter((entry) => entry.id !== stub.id),
      stub
    ]
  const { head, tail } = wsOutputSummary(block)
  return {
    text: [
      '[NeuroStrata GC Summary — head]',
      head,
      '[NeuroStrata GC Summary — tail]',
      tail,
      ...pointers,
      ''
    ].join('\n'),
    changed: true
  }
}

function wsHeaderValue(value: unknown, maxBytes: number): string {
  const text = typeof value === 'string' ? value : ''
  const clean = text.replace(CONTROL_CHARS, '')
  if (Buffer.byteLength(clean) > maxBytes) return wsSha(clean)
  return wsEscapeFrameToken(clean || '-')
}
function wsRenderNodeLine(
  node: WorkingSetNode,
  collection: 'roots' | 'young_gen',
  summary = node.summary
): string {
  const prefix = collection === 'roots' ? 'Root' : 'Young'
  return `[${prefix}] ${wsEscapeFrameToken(node.id)} | ${wsEscapeFrameToken(summary)} | expand:${wsEscapeFrameToken(node.expand_tool)}\n`
}
function wsFitNodeLine(
  node: WorkingSetNode,
  collection: 'roots' | 'young_gen',
  available: number
): { line: string; node: WorkingSetNode } | null {
  const full = wsRenderNodeLine(node, collection)
  if (Buffer.byteLength(full) <= available) return { line: full, node }
  if (IMMORTAL_KINDS.has(node.kind)) return null
  const fixed = Buffer.byteLength(wsRenderNodeLine(node, collection, ''))
  if (fixed >= available) return null
  const summary = wsTruncateUtf8(node.summary, available - fixed)
  if (summary === '') return null
  const shortened = { ...node, summary }
  shortened.byte_cost = Buffer.byteLength(
    wsCanonicalNodeLine(shortened, collection)
  )
  return { line: wsRenderNodeLine(shortened, collection), node: shortened }
}
function wsGoldenProjection(node: WorkingSetNode, summary: string): string {
  return `  - ${wsEscapeFrameToken(node.id)} | ${wsEscapeFrameToken(summary)}\n`
}

/** Render one bounded, marker-safe frame; state nodes are never demoted here. */
export function renderWorkingSetFrame(
  state: WorkingSetState,
  activeSession: Record<string, unknown>
): { text: string; usedBytes: number; fingerprint: string } {
  const close = `${WORKING_SET_FRAME_CLOSE}\n`
  const header = `${WORKING_SET_FRAME_OPEN} Namespace: ${wsHeaderValue(activeSession.namespace ?? state.namespace, 128)} | Active Task: ${wsHeaderValue(activeSession.task_id ?? activeSession.active_task_id, 128)} [${wsHeaderValue(activeSession.status, 32)}] | Epoch: ${state.epoch} | Turn: ${state.turn_seq}\n`
  const goldenHeading = '[NeuroStrata Golden Rules — ALWAYS ENFORCED]\n'
  const workingHeading = '[NeuroStrata Working Set]\n'
  const roots = [...state.roots].sort((a, b) =>
    wsNodeCompare(a, b, wsNodeTier(a, 'roots'), wsNodeTier(b, 'roots'))
  )
  const young = [...state.young_gen].sort((a, b) =>
    wsNodeCompare(a, b, 'YoungGen', 'YoungGen')
  )
  const golden = roots.filter((node) => node.kind === 'golden_rule')
  if (golden.length === 0 && Array.isArray(activeSession.golden_rules)) {
    for (const value of activeSession.golden_rules) {
      if (typeof value !== 'string' || value.trim() === '') continue
      const summary = wsNormalizedText(value).trim()
      const id = `rule:${wsSha(summary)}`
      const node: WorkingSetNode = {
        id,
        kind: 'golden_rule',
        label: id,
        summary: wsTruncateUtf8(summary, WORKING_SET_SUMMARY_MAX_BYTES),
        governing_ids: [],
        byte_cost: 1,
        last_touched_turn: state.turn_seq,
        expand_tool: 'neurostrata_get_snapshot'
      }
      node.byte_cost = Buffer.byteLength(wsCanonicalNodeLine(node, 'roots'))
      golden.push(node)
    }
  }
  const immortal = [
    ...golden.map((node) => ({
      node,
      line: wsGoldenProjection(node, node.summary)
    })),
    ...roots
      .filter((node) => node.kind === 'task_header')
      .map((node) => ({ node, line: wsRenderNodeLine(node, 'roots') }))
  ]
  const baseBytes = Buffer.byteLength(
    header + goldenHeading + workingHeading + close
  )
  const goldenLines: string[] = []
  const workingLines: string[] = []
  if (
    immortal.every(
      ({ line }) => Buffer.byteLength(line) <= WORKING_SET_MAX_FRAME_BYTES
    ) &&
    baseBytes +
      immortal.reduce((sum, entry) => sum + Buffer.byteLength(entry.line), 0) <=
      WORKING_SET_MAX_FRAME_BYTES
  ) {
    goldenLines.push(
      ...immortal
        .filter((entry) => entry.node.kind === 'golden_rule')
        .map((entry) => entry.line)
    )
    workingLines.push(
      ...immortal
        .filter((entry) => entry.node.kind === 'task_header')
        .map((entry) => entry.line)
    )
    const candidates: Array<{
      line: string
      node?: WorkingSetNode
      collection?: 'roots' | 'young_gen'
    }> = [
      ...roots
        .filter((node) => !IMMORTAL_KINDS.has(node.kind))
        .map((node) => ({
          line: wsRenderNodeLine(node, 'roots'),
          node,
          collection: 'roots' as const
        })),
      ...young.map((node) => ({
        line: wsRenderNodeLine(node, 'young_gen'),
        node,
        collection: 'young_gen' as const
      })),
      ...[...state.evicted_stubs]
        .sort(wsStubCompare)
        .map((stub) => ({ line: `[Stub] ${stub.pointer}\n` })),
      ...[...state.tool_output_stubs]
        .sort(wsToolCompare)
        .map((stub) => ({ line: `[ToolStub] ${stub.pointer}\n` }))
    ]
    let used =
      baseBytes +
      goldenLines.reduce((sum, line) => sum + Buffer.byteLength(line), 0) +
      workingLines.reduce((sum, line) => sum + Buffer.byteLength(line), 0)
    for (const candidate of candidates) {
      const available =
        WORKING_SET_MAX_FRAME_BYTES - used - Buffer.byteLength(close)
      if (candidate.node && candidate.collection) {
        const fitted = wsFitNodeLine(
          candidate.node,
          candidate.collection,
          available
        )
        if (fitted) {
          workingLines.push(fitted.line)
          used += Buffer.byteLength(fitted.line)
        }
      } else if (Buffer.byteLength(candidate.line) <= available) {
        workingLines.push(candidate.line)
        used += Buffer.byteLength(candidate.line)
      }
    }
  } else if (golden.length > 0) {
    const availableBytes = WORKING_SET_MAX_FRAME_BYTES - baseBytes
    const taskHeaders = immortal.filter(
      (entry) => entry.node.kind === 'task_header'
    )
    const minimumGolden = golden.map((node) => wsGoldenProjection(node, ''))
    const taskHeaderBytes = taskHeaders.reduce(
      (sum, entry) => sum + Buffer.byteLength(entry.line),
      0
    )
    const minimumBytes =
      taskHeaderBytes +
      minimumGolden.reduce((sum, line) => sum + Buffer.byteLength(line), 0)
    let shortenedGolden: string[] | null = null
    if (
      taskHeaders.every(
        ({ line }) => Buffer.byteLength(line) <= WORKING_SET_MAX_FRAME_BYTES
      ) &&
      minimumGolden.every(
        (line) => Buffer.byteLength(line) <= WORKING_SET_MAX_FRAME_BYTES
      ) &&
      minimumBytes <= availableBytes
    ) {
      let perRuleBudget = Math.floor(
        (availableBytes - minimumBytes) / golden.length
      )
      while (perRuleBudget >= 0) {
        const candidate = golden.map((node) =>
          wsGoldenProjection(node, wsTruncateUtf8(node.summary, perRuleBudget))
        )
        const candidateBytes =
          taskHeaderBytes +
          candidate.reduce((sum, line) => sum + Buffer.byteLength(line), 0)
        if (
          candidate.every(
            (line) => Buffer.byteLength(line) <= WORKING_SET_MAX_FRAME_BYTES
          ) &&
          candidateBytes <= availableBytes
        ) {
          shortenedGolden = candidate
          break
        }
        perRuleBudget--
      }
    }
    if (shortenedGolden !== null) {
      goldenLines.push(...shortenedGolden)
      workingLines.push(...taskHeaders.map((entry) => entry.line))
    } else {
      const idsHash = wsSha(
        golden
          .map((node) => node.id)
          .sort()
          .join('\n')
      )
      goldenLines.push(
        `[NeuroStrata Golden Rule Manifest] count=${golden.length} | ids_sha256=${idsHash} | expand:neurostrata_get_snapshot\n`
      )
    }
  }
  let text =
    header +
    goldenHeading +
    goldenLines.join('') +
    workingHeading +
    workingLines.join('') +
    close
  if (Buffer.byteLength(text) > WORKING_SET_MAX_FRAME_BYTES) {
    text = `${WORKING_SET_FRAME_OPEN}\n${goldenHeading}${workingHeading}${close}`
  }
  return { text, usedBytes: Buffer.byteLength(text), fingerprint: wsSha(text) }
}

function wsGoldenRules(
  state: WorkingSetState,
  activeSession: Record<string, unknown>
): Array<{ id: string; summary: string }> {
  const fromState = state.roots
    .filter((node) => node.kind === 'golden_rule')
    .map((node) => ({ id: node.id, summary: node.summary }))
  if (fromState.length > 0 || !Array.isArray(activeSession.golden_rules))
    return fromState
  const rules: Array<{ id: string; summary: string }> = []
  for (const value of activeSession.golden_rules) {
    if (typeof value !== 'string' || value.trim() === '') continue
    const summary = wsNormalizedText(value).trim()
    rules.push({ id: `rule:${wsSha(summary)}`, summary })
  }
  return rules
}

/** Tier 0: immortal Golden Rules only; zero volatile counters or task fields. */
export function renderWorkingSetTier0Prefix(
  state: WorkingSetState,
  activeSession?: ActiveSessionState | null
): { text: string; usedBytes: number; fingerprint: string } {
  const parent = wsRecord(activeSession) ?? {}
  const open = `${WORKING_SET_TIER0_OPEN}\n`
  const close = `${WORKING_SET_TIER0_CLOSE}\n`
  const rules = wsGoldenRules(state, parent)
  const projections = rules.map((rule) =>
    wsGoldenProjection(
      { id: rule.id } as WorkingSetNode,
      wsTruncateUtf8(rule.summary, WORKING_SET_SUMMARY_MAX_BYTES)
    )
  )
  let body = projections.join('')
  if (Buffer.byteLength(open + body + close) > WORKING_SET_TIER0_MAX_BYTES) {
    const idsHash = wsSha(
      rules
        .map((rule) => rule.id)
        .sort()
        .join('\n')
    )
    body = `[NeuroStrata Golden Rule Manifest] count=${rules.length} | ids_sha256=${idsHash} | expand:neurostrata_get_snapshot\n`
    while (
      Buffer.byteLength(open + body + close) > WORKING_SET_TIER0_MAX_BYTES &&
      body.length > 0
    )
      body = wsTruncateUtf8(body, Buffer.byteLength(body) - 1)
  }
  const text = open + body + close
  return { text, usedBytes: Buffer.byteLength(text), fingerprint: wsSha(text) }
}

/** Tier 2: volatile task/roots/young/edges/stubs; never invalidates tier 0. */
export function renderWorkingSetDeltaFrame(
  state: WorkingSetState,
  activeSession?: ActiveSessionState | null,
  options?: { omitImmortalRules?: boolean; maxBytes?: number }
): { text: string; usedBytes: number; fingerprint: string } {
  const parent = wsRecord(activeSession) ?? {}
  const close = `${WORKING_SET_FRAME_CLOSE}\n`
  const header = `${WORKING_SET_FRAME_OPEN} Namespace: ${wsHeaderValue(parent.namespace ?? state.namespace, 128)} | Active Task: ${wsHeaderValue(parent.task_id ?? parent.active_task_id, 128)} [${wsHeaderValue(parent.status, 32)}] | Epoch: ${state.epoch} | Turn: ${state.turn_seq}\n`
  const tier0Bytes = renderWorkingSetTier0Prefix(state, activeSession).usedBytes
  const maxAllowed = Math.max(0, WORKING_SET_MAX_FRAME_BYTES - tier0Bytes)
  const budget = Math.min(
    wsU32(options?.maxBytes) ? (options!.maxBytes as number) : maxAllowed,
    maxAllowed
  )
  const goldenHeading = '[NeuroStrata Golden Rules — ALWAYS ENFORCED]\n'
  const workingHeading = '[NeuroStrata Working Set]\n'
  const goldenLines = options?.omitImmortalRules
    ? []
    : wsGoldenRules(state, parent).map((rule) =>
        wsGoldenProjection(
          { id: rule.id } as WorkingSetNode,
          wsTruncateUtf8(rule.summary, WORKING_SET_SUMMARY_MAX_BYTES)
        )
      )
  const baseBytes =
    Buffer.byteLength(header + workingHeading + close) +
    (options?.omitImmortalRules ? 0 : Buffer.byteLength(goldenHeading)) +
    goldenLines.reduce((sum, line) => sum + Buffer.byteLength(line), 0)
  const candidates: string[] = [
    ...state.roots
      .filter((node) => !IMMORTAL_KINDS.has(node.kind))
      .map((node) => wsRenderNodeLine(node, 'roots')),
    ...state.young_gen.map((node) => wsRenderNodeLine(node, 'young_gen')),
    ...[...state.edges]
      .sort(wsEdgeCompare)
      .map(
        (edge) =>
          `[Edge] ${wsEscapeFrameToken(edge.from)} -${edge.kind}-> ${wsEscapeFrameToken(edge.to)}\n`
      ),
    ...[...state.evicted_stubs]
      .sort(wsStubCompare)
      .map((stub) => `[Stub] ${stub.pointer}\n`),
    ...[...state.tool_output_stubs]
      .sort((a, b) => wsToolCompare(a, b))
      .map((stub) => `[ToolStub] ${stub.pointer}\n`)
  ]
  let used = baseBytes
  const workingLines: string[] = []
  for (const line of candidates) {
    if (used + Buffer.byteLength(line) + Buffer.byteLength(close) > budget)
      continue
    workingLines.push(line)
    used += Buffer.byteLength(line)
  }
  let text =
    header +
    (options?.omitImmortalRules ? '' : goldenHeading) +
    goldenLines.join('') +
    workingHeading +
    workingLines.join('') +
    close
  if (Buffer.byteLength(text) > budget) {
    text = `${header}${workingHeading}${close}`
    if (Buffer.byteLength(text) > budget) text = wsTruncateUtf8(text, budget)
  }
  return { text, usedBytes: Buffer.byteLength(text), fingerprint: wsSha(text) }
}

/** Deterministic compaction checkpoint; replaces a lossy host LLM summary. */
export function renderWorkingSetCompactionSummary(
  state: WorkingSetState,
  activeSession?: ActiveSessionState | null
): string {
  const parent = wsRecord(activeSession) ?? {}
  const rules = wsGoldenRules(state, parent)
  const lines = [
    '[NeuroStrata Compaction Summary]',
    ...rules.map((rule) => `[Rules] ${rule.id} ${rule.summary}`),
    `[Task] ${asString(parent.task_id ?? parent.active_task_id) ?? '-'} ${asString(parent.status) ?? '-'}`,
    `[Roots] ${state.roots
      .filter((node) => !IMMORTAL_KINDS.has(node.kind))
      .map((node) => node.id)
      .join(',')}`,
    `[Edges] ${state.edges
      .map((edge) => `${edge.from} -${edge.kind}-> ${edge.to}`)
      .join(',')}`,
    `[Restore] stubs=${state.evicted_stubs.length + state.tool_output_stubs.length} heap=LadybugDB`,
    WORKING_SET_FRAME_CLOSE
  ]
  return wsTruncateUtf8(lines.join('\n'), WORKING_SET_SUMMARY_MAX_BYTES)
}

function wsPartText(value: unknown): string {
  if (typeof value === 'string') return value
  const rec = wsRecord(value)
  return typeof rec?.text === 'string' ? rec.text : ''
}
function wsMessageText(value: unknown): string {
  const direct = wsPartText(value)
  if (direct !== '') return direct
  const rec = wsRecord(value)
  const content = rec?.content
  if (typeof content === 'string') return content
  if (Array.isArray(content))
    return content.map((part) => wsPartText(part)).join('\n')
  return ''
}
function wsMessageIsProtected(value: unknown): boolean {
  const rec = wsRecord(value)
  if (rec?.role === 'system' || rec?.role === 'developer') return true
  const text = wsMessageText(value)
  return (
    text.includes(WORKING_SET_TIER0_OPEN) ||
    text.includes(WORKING_SET_TIER0_CLOSE) ||
    text.includes(WORKING_SET_FRAME_OPEN)
  )
}

/** Tier 1: fold pre-frontier history into stride-stable deterministic stubs. */
export function projectWorkingSetMessages(
  messages: unknown[],
  state: WorkingSetState,
  options?: { hotTailCount?: number; stride?: number }
): {
  messages: unknown[]
  quantizedFrontier: number
  foldedCount: number
} {
  if (!Array.isArray(messages))
    return { messages, quantizedFrontier: 0, foldedCount: 0 }
  const hotTailCount = wsU32(options?.hotTailCount)
    ? (options!.hotTailCount as number)
    : WORKING_SET_HOT_TAIL_MESSAGES
  const stride = wsU32(options?.stride)
    ? (options!.stride as number)
    : WORKING_SET_STRIDE_MESSAGES
  const frontier = computeWorkingSetQuantizedFrontier(
    messages.length,
    hotTailCount,
    stride
  )
  if (frontier === 0)
    return { messages: [...messages], quantizedFrontier: 0, foldedCount: 0 }
  const roots = state.roots.length
  const stubs = state.evicted_stubs.length + state.tool_output_stubs.length
  const projected: unknown[] = []
  for (let start = 0; start < frontier; start += stride) {
    const end = Math.min(start + stride, frontier)
    let folded = 0
    const retained: unknown[] = []
    for (let index = start; index < end; index++) {
      if (wsMessageIsProtected(messages[index])) retained.push(messages[index])
      else folded++
    }
    if (folded > 0)
      projected.push(
        `${WORKING_SET_STRIDE_MARKER}${start}..${end}] folded_messages=${folded} | heap=LadybugDB | working_set_roots=${roots} | stubs=${stubs}`
      )
    projected.push(...retained)
  }
  return {
    messages: [...projected, ...messages.slice(frontier)],
    quantizedFrontier: frontier,
    foldedCount: frontier
  }
}

/** Replace or append the immutable-prefix system part in place. */
export function upsertWorkingSetSystemPrefix(
  system: unknown[],
  tier0Text: string
): boolean {
  if (!Array.isArray(system)) return false
  let index = -1
  for (let i = 0; i < system.length; i++) {
    if (wsPartText(system[i]).includes(WORKING_SET_TIER0_OPEN)) {
      index = i
      break
    }
  }
  if (index !== -1) {
    if (wsPartText(system[index]) === tier0Text) return false
    const record = wsRecord(system[index])
    system[index] =
      record && typeof record.text === 'string'
        ? { ...record, text: tier0Text }
        : { type: 'text', text: tier0Text }
    return true
  }
  system.push({ type: 'text', text: tier0Text })
  return true
}

/** Attach or update the volatile delta on the tail without touching prefix. */
export function upsertWorkingSetMessageTailDelta(
  messages: unknown[],
  deltaText: string
): boolean {
  if (!Array.isArray(messages)) return false
  let index = -1
  for (let i = 0; i < messages.length; i++) {
    if (wsMessageText(messages[i]).includes(WORKING_SET_FRAME_OPEN)) {
      index = i
      break
    }
  }
  if (index !== -1) {
    if (wsMessageText(messages[index]) === deltaText) return false
    const record = wsRecord(messages[index])
    messages[index] =
      record && typeof record.content === 'string'
        ? { ...record, content: deltaText }
        : typeof messages[index] === 'string'
          ? deltaText
          : { role: 'user', content: deltaText }
    return true
  }
  messages.push({ role: 'user', content: deltaText })
  return true
}

function wsRegex(pattern: RegExp): RegExp {
  return new RegExp(pattern.source, 'g')
}
function wsReplaceCompleteFrames(
  text: string,
  replacement: string
): { text: string; found: boolean } {
  const openTag = '[NeuroStrata Active Context]'
  const closeTag = '[NeuroStrata End Active Context]'
  const result: string[] = []
  let i = 0
  let found = false
  while (i < text.length) {
    const openIdx = text.indexOf(openTag, i)
    if (openIdx === -1) {
      result.push(text.slice(i))
      break
    }
    const afterOpen = text.slice(openIdx + openTag.length)
    const closeIdx = afterOpen.indexOf(closeTag)
    if (closeIdx === -1) {
      result.push(text.slice(i))
      break
    }
    const frameEnd = openIdx + openTag.length + closeIdx + closeTag.length
    const afterClose = text.slice(frameEnd)
    const newlineMatch = afterClose.match(/^\r?\n/)
    const newlineLen = newlineMatch ? newlineMatch[0].length : 0
    result.push(text.slice(i, openIdx))
    result.push(replacement)
    found = true
    i = frameEnd + newlineLen
  }
  return { text: result.join(''), found }
}

function wsStripFrames(text: string): { text: string; found: boolean } {
  const replaced = wsReplaceCompleteFrames(text, '')
  if (!replaced.found) return { text: replaced.text, found: false }
  const legacyOpen = '[NeuroStrata '
  let i = 0
  const legacyLines: string[] = []
  while (i < replaced.text.length) {
    const openIdx = replaced.text.indexOf(legacyOpen, i)
    if (openIdx === -1) {
      legacyLines.push(replaced.text.slice(i))
      break
    }
    const afterOpen = replaced.text.slice(openIdx + legacyOpen.length)
    const closeBrIdx = afterOpen.indexOf(']')
    if (closeBrIdx === -1) {
      legacyLines.push(replaced.text.slice(i))
      break
    }
    const header = replaced.text.slice(
      openIdx,
      openIdx + legacyOpen.length + closeBrIdx + 1
    )
    const afterCloseBr = replaced.text.slice(
      openIdx + legacyOpen.length + closeBrIdx + 1
    )
    if (afterCloseBr.startsWith('\n') || afterCloseBr === '') {
      legacyLines.push(header)
      i = openIdx + legacyOpen.length + closeBrIdx + 1
      if (replaced.text[i] === '\n') i++
      continue
    }
    const nextOpen = replaced.text.indexOf(legacyOpen, openIdx + 1)
    const nextEnd = replaced.text.indexOf('\n', openIdx)
    const endIdx =
      nextOpen === -1
        ? nextEnd === -1
          ? replaced.text.length
          : nextEnd
        : Math.min(nextOpen, nextEnd)
    legacyLines.push('')
    i = endIdx
  }
  const found = replaced.found || legacyLines.some((l) => l !== '')
  return { text: legacyLines.join('\n'), found }
}

/** Upsert one mutable frame using context[], content, then additionalContext. */
export function upsertWorkingSetFrame(sink: unknown, frame: string): boolean {
  const rec = wsRecord(sink)
  if (!rec) return false
  if (Array.isArray(rec.context)) {
    const kept: unknown[] = []
    let replaced = false
    let changed = false
    for (const value of rec.context) {
      if (typeof value !== 'string') {
        kept.push(value)
        continue
      }
      if (value === frame) {
        if (replaced) {
          changed = true
          continue
        }
        replaced = true
        kept.push(frame)
        continue
      }
      const stripped = wsStripFrames(value)
      if (!stripped.found) {
        kept.push(value)
        continue
      }
      changed = true
      if (!replaced) {
        kept.push(
          stripped.text
            ? `${stripped.text}${stripped.text.endsWith('\n') ? '' : '\n'}${frame}`
            : frame
        )
        replaced = true
      }
    }
    if (!replaced) {
      kept.unshift(frame)
      changed = true
    }
    if (changed) rec.context = kept
    return changed
  }
  for (const key of ['content', 'additionalContext']) {
    if (typeof rec[key] !== 'string') continue
    const old = rec[key] as string
    if (old === frame) return false
    const complete = wsReplaceCompleteFrames(old, frame)
    if (complete.found) {
      rec[key] = complete.text
    } else {
      const openIdx = old.indexOf(WORKING_SET_FRAME_OPEN)
      if (openIdx !== -1) {
        rec[key] = old.slice(0, openIdx) + frame
      } else {
        rec[key] = `${frame}${old}`
      }
    }
    return true
  }
  return false
}

function wsActiveParent(scope: WorkingSetScope): Record<string, unknown> {
  const loaded = wsReadParent(scope)
  return loaded.status === 'valid' && loaded.parent ? loaded.parent : {}
}
function wsRememberEvent(state: WorkingSetState, eventId?: string): boolean {
  if (!eventId || !wsText(eventId, 256)) return true
  if (state.recent_event_ids.includes(eventId)) return false
  state.recent_event_ids = [eventId, ...state.recent_event_ids].slice(
    0,
    WORKING_SET_MAX_EVENT_IDS
  )
  return true
}
function wsSeedActive(
  state: WorkingSetState,
  parent: Record<string, unknown>
): void {
  const taskId = asString(parent.task_id ?? parent.active_task_id)
  if (!taskId || !wsText(taskId, 1024)) return
  const id = `task:${taskId}`
  if (state.roots.some((node) => node.id === id)) return
  const node = wsTouchNode({ id, kind: 'task_header' }, state.turn_seq, '')
  if (node) state.roots.push(node)
}
export function wsSyncBridges(
  state: WorkingSetState,
  parent: Record<string, unknown>
): void {
  if (!Array.isArray(parent.conceptual_bridges)) return
  for (const value of parent.conceptual_bridges.slice(0, 4)) {
    const bridge = wsRecord(value)
    const target =
      typeof value === 'string'
        ? value.trim()
        : asString(bridge?.target_id ?? bridge?.id)
    if (!target) continue
    const id = target.startsWith('bridge:') ? target : `bridge:${target}`
    if (
      state.young_gen.some((node) => node.id === id) ||
      state.evicted_stubs.some((stub) => stub.id === id) ||
      state.roots.some((node) => node.id === id)
    )
      continue
    const summary =
      typeof value === 'string'
        ? value.trim()
        : (asString(bridge?.target_summary ?? bridge?.content) ?? target)
    const node = wsTouchNode(
      { id, kind: 'conceptual_bridge', memoryId: target },
      state.turn_seq,
      ''
    )
    if (!node) continue
    node.summary = wsTruncateUtf8(summary, WORKING_SET_SUMMARY_MAX_BYTES)
    node.byte_cost = Buffer.byteLength(wsCanonicalNodeLine(node, 'young_gen'))
    state.young_gen.push(node)
  }
}
async function wsExpansion(
  request: WorkingSetExpansionRequest
): Promise<WorkingSetNode | null> {
  if (!mcpClient) return null
  try {
    const text = await mcpClient.callTool(request.expandTool, request.input)
    const body = wsRecord(JSON.parse(text))
    const value = wsRecord(body?.memory) ?? wsRecord(body?.node) ?? body
    if (!value) return null
    const id = asString(value.id)
    const kind = asString(value.kind) as WorkingSetNodeKind | undefined
    if (!id || !kind || !NODE_KINDS.has(kind) || kind === 'recall_text')
      return null
    const summary = asString(value.summary) ?? asString(value.content) ?? id
    const collection =
      IMMORTAL_KINDS.has(kind) || PINNED_KINDS.has(kind) ? 'roots' : 'young_gen'
    const expand_tool =
      kind === 'file' || kind === 'symbol'
        ? 'neurostrata_memories_for_file'
        : kind === 'task_header' || kind === 'golden_rule'
          ? 'neurostrata_get_snapshot'
          : kind === 'ast_summary' || kind === 'conceptual_bridge'
            ? 'neurostrata_related'
            : 'neurostrata_get_memory'
    const node: WorkingSetNode = {
      id,
      kind,
      label: wsTruncateUtf8(asString(value.label) ?? id, 128),
      summary: wsTruncateUtf8(summary, WORKING_SET_SUMMARY_MAX_BYTES),
      governing_ids:
        wsGoverningIds(
          Array.isArray(value.governing_ids)
            ? (value.governing_ids as string[])
            : []
        ) ?? [],
      byte_cost: 1,
      last_touched_turn: 0,
      expand_tool,
      ...(typeof value.path === 'string' ? { path: value.path } : {}),
      ...(typeof value.symbol === 'string' ? { symbol: value.symbol } : {}),
      ...(typeof value.memory_type === 'string'
        ? { memory_type: value.memory_type }
        : {})
    }
    node.byte_cost = Buffer.byteLength(wsCanonicalNodeLine(node, collection))
    return wsValidateNode(node, collection)
  } catch {
    return null
  }
}
function wsFinalize(
  state: WorkingSetState,
  scope: WorkingSetScope,
  parent: Record<string, unknown>
): {
  state: WorkingSetState
  frame: string
  fingerprint: string
  usedBytes: number
} {
  wsSeedActive(state, parent)
  wsSyncBridges(state, parent)
  const swept = sweepWorkingSet(state)
  const rendered = renderWorkingSetFrame(swept, parent)
  swept.used_frame_bytes = rendered.usedBytes
  swept.frame_fingerprint = rendered.fingerprint
  return { state: swept, frame: rendered.text, ...rendered }
}

/** Observe or advance one native turn, deduplicating native event/turn identity. */
export async function runWorkingSetTurnBoundary(
  scopeInput: WorkingSetScope,
  eventId?: string,
  turnId?: string
): Promise<{
  frame: string
  fingerprint: string
  changed: boolean
  duplicateEvent?: boolean
}> {
  const scope = wsScope(scopeInput)
  if (!scope)
    return { frame: '', fingerprint: wsEmptyFingerprint(), changed: false }
  const before = await loadWorkingSet(scope)
  const parent = wsActiveParent(scope)
  const state = before
  if (!wsRememberEvent(state, eventId)) {
    const rendered = renderWorkingSetFrame(state, parent)
    return {
      frame: rendered.text,
      fingerprint: rendered.fingerprint,
      changed: false,
      duplicateEvent: true
    }
  }
  const hasTurnId = !!turnId && wsText(turnId, 128)
  const newTurn = hasTurnId && turnId !== state.active_turn_id
  if (hasTurnId ? !newTurn : !eventId) {
    if (eventId) {
      await persistWorkingSet(scope, state)
    }
    const rendered = renderWorkingSetFrame(state, parent)
    return {
      frame: rendered.text,
      fingerprint: rendered.fingerprint,
      changed: false
    }
  }
  const advanced = await advanceWorkingSetTurn(
    state,
    scope.namespace,
    scope.sessionId,
    scope.projectRoot,
    [],
    wsExpansion
  )
  advanced.active_turn_id = newTurn ? turnId! : advanced.active_turn_id
  const final = wsFinalize(advanced, scope, parent)
  const changed =
    final.fingerprint !== before.frame_fingerprint ||
    final.state.turn_seq !== before.turn_seq
  if (
    before.last_injected_fingerprint !== wsEmptyFingerprint() &&
    renderWorkingSetFrame({ ...final.state, turn_seq: before.turn_seq }, parent)
      .fingerprint === before.last_injected_fingerprint
  ) {
    final.state.last_injected_fingerprint = final.fingerprint
    rememberWorkingSetInjection(scope, final.fingerprint)
  }
  await persistWorkingSet(scope, final.state)
  return { frame: final.frame, fingerprint: final.fingerprint, changed }
}

async function runWorkingSetContextCycleInternal(
  scopeInput: WorkingSetScope,
  markInjected: boolean
): Promise<{ frame: string; fingerprint: string; changed: boolean }> {
  const scope = wsScope(scopeInput)
  if (!scope)
    return { frame: '', fingerprint: wsEmptyFingerprint(), changed: false }
  const state = await loadWorkingSet(scope)
  const parent = wsActiveParent(scope)
  const final = wsFinalize(state, scope, parent)
  const remembered = workingSetInjectionFingerprints.get(
    workingSetInjectionKey(scope)
  )
  const changed =
    final.fingerprint !== state.frame_fingerprint ||
    (final.fingerprint !== state.last_injected_fingerprint &&
      final.fingerprint !== remembered)
  if (!changed)
    return {
      frame: final.frame,
      fingerprint: final.fingerprint,
      changed: false
    }
  if (markInjected) final.state.last_injected_fingerprint = final.fingerprint
  await persistWorkingSet(scope, final.state)
  return { frame: final.frame, fingerprint: final.fingerprint, changed: true }
}

/** Render current state without advancing turns or marking touches. */
export async function runWorkingSetContextCycle(
  scopeInput: WorkingSetScope,
  eventId?: string
): Promise<{ frame: string; fingerprint: string; changed: boolean }> {
  return runWorkingSetContextCycleInternal(scopeInput, true)
}

/** Linear, bounded extraction of explicit prompt anchors (no model parsing). */
function wsPromptAnchors(
  promptText: string,
  projectRoot: string
): WorkingSetTouch[] {
  if (typeof promptText !== 'string' || promptText.length > 16384) return []
  const tokens: string[] = []
  let token = ''
  for (const char of promptText) {
    const split =
      /\s/u.test(char) ||
      char === '`' ||
      char === '"' ||
      char === "'" ||
      char === '(' ||
      char === ')' ||
      char === ',' ||
      char === ';'
    if (split) {
      if (token !== '') {
        tokens.push(token)
        token = ''
      }
    } else token += char
    if (tokens.length >= 64) break
  }
  if (token !== '' && tokens.length < 64) tokens.push(token)
  const touches = new Map<string, WorkingSetTouch>()
  const add = (touch: WorkingSetTouch | null): void => {
    if (touch && wsText(touch.id, 1024) && !touches.has(touch.id))
      touches.set(touch.id, touch)
  }
  for (const raw of tokens) {
    let value = raw
    while (value.length > 0 && '.,[]{}<>'.includes(value[0]))
      value = value.slice(1)
    while (value.length > 0 && '.,[]{}<>:'.includes(value[value.length - 1]))
      value = value.slice(0, -1)
    if (value.startsWith('mem:')) {
      add({ id: value, kind: 'memory', memoryId: value.slice(4) })
      continue
    }
    if (value.startsWith('path:') || value.startsWith('file:')) {
      const rel = wsNormalizeTouchPath(
        value.slice(value.indexOf(':') + 1),
        projectRoot
      )
      if (rel !== null) add({ id: `file:${rel}`, kind: 'file', path: rel })
      continue
    }
    if (value.startsWith('symbol:')) {
      const body = value.slice(7)
      const hash = body.indexOf('#')
      if (hash > 0) {
        const rel = wsNormalizeTouchPath(body.slice(0, hash), projectRoot)
        const symbol = body.slice(hash + 1)
        if (rel !== null && wsText(symbol, 256, true))
          add({
            id: `symbol:${rel}#${symbol}`,
            kind: 'symbol',
            path: rel,
            symbol
          })
      }
      continue
    }
    if (value.includes('/') && !value.includes('://')) {
      const rel = wsNormalizeTouchPath(value, projectRoot)
      if (rel !== null) add({ id: `file:${rel}`, kind: 'file', path: rel })
    }
  }
  return [...touches.values()].slice(0, 32)
}

/** Pre-turn activation: mark anchors, spread edges, and persist L1 state. */
export async function runWorkingSetPromptCycle(input: {
  scope: WorkingSetScope
  promptText: string
  turnId?: string
  eventId?: string
}): Promise<{
  frame: string
  fingerprint: string
  changed: boolean
  activatedIds: string[]
}> {
  const empty = {
    frame: '',
    fingerprint: wsEmptyFingerprint(),
    changed: false,
    activatedIds: []
  }
  const scope = wsScope(input.scope)
  if (!scope) return empty
  const before = await loadWorkingSet(scope)
  if (input.eventId && !wsRememberEvent(before, input.eventId)) return empty
  const newTurn =
    !!input.turnId &&
    wsText(input.turnId, 128) &&
    input.turnId !== before.active_turn_id
  if (input.turnId && !newTurn) return empty
  const touches = wsPromptAnchors(input.promptText, scope.projectRoot)
  const activation = wsSpreadActivationIds(
    before,
    touches.map((touch) => touch.id)
  )
  const state = await advanceWorkingSetTurn(
    before,
    scope.namespace,
    scope.sessionId,
    scope.projectRoot,
    touches,
    wsExpansion
  )
  if (newTurn) state.active_turn_id = input.turnId!
  const parent = wsActiveParent(scope)
  const final = wsFinalize(state, scope, parent)
  const changed =
    final.fingerprint !== before.frame_fingerprint ||
    final.state.turn_seq !== before.turn_seq
  await persistWorkingSet(scope, final.state)
  return {
    frame: final.frame,
    fingerprint: final.fingerprint,
    changed,
    activatedIds: activation.activatedIds
  }
}

/** Mark deterministic file/symbol/memory touches without advancing the turn. */
export async function markWorkingSetTouches(
  scopeInput: WorkingSetScope,
  touched: WorkingSetTouch[],
  eventId?: string
): Promise<void> {
  const scope = wsScope(scopeInput)
  if (!scope) return
  const state = await loadWorkingSet(scope)
  if (!wsRememberEvent(state, eventId)) return
  const parent = wsActiveParent(scope)
  for (const touch of touched) {
    const normPath =
      touch.path === undefined
        ? null
        : wsNormalizeTouchPath(touch.path, scope.projectRoot)
    const existing =
      state.roots.find(
        (node) =>
          node.id === touch.id ||
          (normPath !== null &&
            node.kind === touch.kind &&
            node.path === normPath &&
            node.symbol === touch.symbol)
      ) ??
      state.young_gen.find(
        (node) =>
          node.id === touch.id ||
          (normPath !== null &&
            node.kind === touch.kind &&
            node.path === normPath &&
            node.symbol === touch.symbol)
      )
    if (existing) {
      if (existing.kind !== touch.kind) continue
      existing.last_touched_turn = state.turn_seq
      continue
    }
    const stubIdx = state.evicted_stubs.findIndex(
      (stub) =>
        stub.id === touch.id ||
        (normPath !== null &&
          stub.kind === touch.kind &&
          stub.path === normPath)
    )
    if (stubIdx !== -1) {
      const prior = state.evicted_stubs[stubIdx]
      if (prior.kind !== touch.kind) continue
      const node = wsTouchNode(touch, state.turn_seq, scope.projectRoot)
      if (!node) continue
      node.governing_ids = [...prior.governing_ids]
      if (prior.memory_type) node.memory_type = prior.memory_type
      const collection =
        IMMORTAL_KINDS.has(node.kind) || PINNED_KINDS.has(node.kind)
          ? 'roots'
          : 'young_gen'
      node.byte_cost = Buffer.byteLength(wsCanonicalNodeLine(node, collection))
      state.evicted_stubs.splice(stubIdx, 1)
      if (collection === 'roots') state.roots.push(node)
      else state.young_gen.push(node)
      continue
    }
    const node = wsTouchNode(touch, state.turn_seq, scope.projectRoot)
    if (!node) continue
    if (IMMORTAL_KINDS.has(node.kind) || PINNED_KINDS.has(node.kind))
      state.roots.push(node)
    else state.young_gen.push(node)
  }
  const final = wsFinalize(state, scope, parent)
  await persistWorkingSet(scope, final.state)
}

/** Shared tool cycle: observe touches, dehydrate text, and return one frame. */
export async function runWorkingSetToolCycle(input: {
  scope: WorkingSetScope
  turnId?: string
  eventId?: string
  toolName: string
  toolInput: unknown
  rawText: string | null
  outputIsText: boolean
  isError: boolean
  advanceTurn: boolean
}): Promise<{
  replacementText: string | null
  frame: string
  fingerprint: string
  dehydrated: boolean
  changed: boolean
}> {
  const scope = wsScope(input.scope)
  if (!scope)
    return {
      replacementText: null,
      frame: '',
      fingerprint: wsEmptyFingerprint(),
      dehydrated: false,
      changed: false
    }
  const current = await loadWorkingSet(scope)
  const beforeFingerprint = current.frame_fingerprint
  const beforeInjected = current.last_injected_fingerprint
  if (
    input.eventId &&
    wsText(input.eventId, 256) &&
    current.recent_event_ids.includes(input.eventId)
  ) {
    const rendered = renderWorkingSetFrame(current, wsActiveParent(scope))
    return {
      replacementText: null,
      frame: rendered.text,
      fingerprint: rendered.fingerprint,
      dehydrated: false,
      changed: false
    }
  }
  const args = asRecord(input.toolInput) ?? {}
  const filePath = asString(
    args.file_path ?? args.filePath ?? args.path ?? args.notebook_path
  )
  const touched: WorkingSetTouch[] = []
  if (filePath && isFileTouchTool(input.toolName))
    touched.push({
      id: `file:${filePath.replace(/\\/g, '/')}`,
      kind: 'file',
      path: filePath
    })
  if (input.advanceTurn) {
    const boundary = await runWorkingSetTurnBoundary(
      scope,
      input.eventId,
      input.turnId
    )
    if (!boundary.duplicateEvent) {
      await markWorkingSetTouches(scope, touched)
    }
  } else {
    await markWorkingSetTouches(scope, touched, input.eventId)
  }
  let replacementText: string | null = null
  let dehydrated = false
  if (
    !input.isError &&
    input.outputIsText &&
    typeof input.rawText === 'string'
  ) {
    const result = await dehydrateRepeatedReadOutput(
      scope,
      input.rawText,
      input.toolName,
      filePath ?? null,
      []
    )
    dehydrated = result.dehydrated
    if (dehydrated) replacementText = result.text
  }
  const state = await loadWorkingSet(scope)
  const parent = wsActiveParent(scope)
  const final = wsFinalize(state, scope, parent)
  const changed =
    final.fingerprint !== beforeFingerprint &&
    final.fingerprint !== beforeInjected
  if (changed && !input.advanceTurn) {
    final.state.last_injected_fingerprint = final.fingerprint
    rememberWorkingSetInjection(scope, final.fingerprint)
  }
  await persistWorkingSet(scope, final.state)
  return {
    replacementText,
    frame: final.frame,
    fingerprint: final.fingerprint,
    dehydrated,
    changed
  }
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
 * `NEUROSTRATA_BUFFER_POOL_BYTES` cannot synthesize an advisory. Returns the
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
    "Raise NEUROSTRATA_BUFFER_POOL_BYTES (or the daemon's configured store.buffer_pool_bytes) and restart the daemon; repeated advisories are suppressed for 10 minutes."
  ].join(' ')
}

const WS_RECALL_EXCLUDED_HEADINGS = new Set([
  'AST',
  'Governing',
  'File',
  'Active',
  'End',
  'Plugin',
  'Working',
  'GC',
  'Tool'
])

function isAsciiRecallKind(kind: string): boolean {
  if (!kind || WS_RECALL_EXCLUDED_HEADINGS.has(kind)) return false
  const first = kind.charCodeAt(0)
  const firstAlpha =
    (first >= 65 && first <= 90) || (first >= 97 && first <= 122)
  if (!firstAlpha) return false
  for (let i = 1; i < kind.length; i++) {
    const code = kind.charCodeAt(i)
    const ok =
      (code >= 65 && code <= 90) ||
      (code >= 97 && code <= 122) ||
      (code >= 48 && code <= 57) ||
      code === 95 ||
      code === 45
    if (!ok) return false
  }
  return true
}

function splitFirstToken(input: string): [string, string] {
  const trimmed = input.trim()
  for (let i = 0; i < trimmed.length; i++) {
    const ch = trimmed.charCodeAt(i)
    if (ch === 32 || ch === 9 || ch === 10 || ch === 13) {
      return [trimmed.slice(0, i), trimmed.slice(i + 1).trim()]
    }
  }
  return [trimmed, '']
}

export function wsRecallMemoryIds(block: string): string[] {
  const ids: string[] = []
  const prefix = '[NeuroStrata'
  let cursor = 0
  while (cursor < block.length) {
    const open = block.indexOf(prefix, cursor)
    if (open < 0) break
    const afterPrefix = open + prefix.length
    const nextChar = block.charCodeAt(afterPrefix)
    if (nextChar !== 32 && nextChar !== 9) {
      cursor = afterPrefix
      continue
    }
    const close = block.indexOf(']', afterPrefix + 1)
    if (close < 0) break
    const newline = block.indexOf('\n', afterPrefix + 1)
    if (newline >= 0 && newline < close) {
      cursor = newline + 1
      continue
    }
    const inner = block.slice(afterPrefix + 1, close).trim()
    cursor = close + 1
    if (!inner || inner.includes('—') || inner.includes('|')) continue
    const [firstToken, afterFirst] = splitFirstToken(inner)
    let rest = ''
    if (firstToken === 'Golden') {
      const [secondToken, afterSecond] = splitFirstToken(afterFirst)
      if (secondToken !== 'Rule') continue
      rest = afterSecond
    } else if (isAsciiRecallKind(firstToken)) {
      rest = afterFirst
    } else {
      continue
    }
    const validFromIdx = rest.indexOf('(valid_from')
    if (validFromIdx >= 0) {
      rest = rest.slice(0, validFromIdx).trim()
    }
    if (!rest || rest === 'Manifest') continue
    const [memId] = splitFirstToken(rest)
    if (memId && !memId.startsWith('(')) ids.push(memId)
  }
  return [...new Set(ids)].slice(0, 8)
}

async function dehydrateWorkingSetRecall(
  scope: WorkingSetScope,
  block: string
): Promise<string> {
  if (!block) return block
  try {
    const state = await loadWorkingSet(scope)
    const result = wsRecallStub(state, block, wsRecallMemoryIds(block))
    if (!result.changed) return block
    await persistWorkingSet(scope, sweepWorkingSet(state))
    return result.text
  } catch {
    return block
  }
}

function workingSetHookScope(
  config: ResolvedConfig,
  sessionId: string | null | undefined
): WorkingSetScope | null {
  if (!sessionId) return null
  const projectRoot = effectiveProjectRoot(config)
  const projectId = wsProjectId(projectRoot)
  if (projectId === null) return null
  const scope: WorkingSetScope = {
    projectRoot,
    projectId,
    namespace: config.namespace,
    sessionId,
    epoch: currentSessionEpoch(sessionId)
  }
  return wsScope(scope)
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
const workingSetInjectionFingerprints = new Map<string, string>()

function workingSetInjectionKey(scope: WorkingSetScope): string {
  return JSON.stringify([
    scope.projectId,
    scope.namespace,
    scope.sessionId,
    scope.epoch
  ])
}

function rememberWorkingSetInjection(
  scope: WorkingSetScope,
  fingerprint: string
): void {
  const key = workingSetInjectionKey(scope)
  workingSetInjectionFingerprints.delete(key)
  workingSetInjectionFingerprints.set(key, fingerprint)
  while (
    workingSetInjectionFingerprints.size > WORKING_SET_MAX_INJECTION_KEYS
  ) {
    const oldest = workingSetInjectionFingerprints.keys().next().value
    if (oldest === undefined) break
    workingSetInjectionFingerprints.delete(oldest)
  }
}

function forgetWorkingSetInjections(
  sessionId: string | null | undefined
): void {
  const sid = sessionId ?? '__default__'
  for (const key of workingSetInjectionFingerprints.keys()) {
    const scope = JSON.parse(key) as [string, string, string, number]
    if (scope[2] === sid) workingSetInjectionFingerprints.delete(key)
  }
}

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
  forgetWorkingSetInjections(sessionId)
}

export function sessionSeenFiles(
  sessionId: string | null | undefined
): Set<string> {
  return sessionEpochState(sessionId).seenFiles
}

export function resetSessionEpoch(sessionId: string | null | undefined): void {
  sessionEpochs.delete(sessionId ?? '__default__')
  forgetWorkingSetInjections(sessionId)
}

export function resetAllSessionEpochs(): void {
  sessionEpochs.clear()
  workingSetInjectionFingerprints.clear()
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
  const scope = workingSetHookScope(config, sessionId)
  if (!scope || !event || typeof event !== 'object') return
  const loaded = wsReadParent(scope)
  if (loaded.status !== 'valid' || !loaded.parent) return
  const active = loaded.parent
  const taskId = asString(active.task_id ?? active.active_task_id)
  const goldenRules = Array.isArray(active.golden_rules)
    ? active.golden_rules.filter(
        (g): g is string => typeof g === 'string' && g.trim() !== ''
      )
    : []
  const bridges = Array.isArray(active.conceptual_bridges)
    ? active.conceptual_bridges
    : []
  if (!taskId && goldenRules.length === 0 && bridges.length === 0) return

  try {
    const cycle = await runWorkingSetContextCycle(scope)
    if (!cycle.changed) return
    if (Array.isArray(event.system) && Array.isArray(event.messages)) {
      const state = await loadWorkingSet(scope)
      const tier0 = renderWorkingSetTier0Prefix(state, active)
      const delta = renderWorkingSetDeltaFrame(state, active, {
        omitImmortalRules: true
      })
      upsertWorkingSetSystemPrefix(event.system, tier0.text)
      event.messages = projectWorkingSetMessages(event.messages, state).messages
      upsertWorkingSetMessageTailDelta(event.messages, delta.text)
    } else {
      if (
        !Array.isArray(event.context) &&
        typeof event.content !== 'string' &&
        typeof event.additionalContext !== 'string'
      ) {
        event.additionalContext = ''
      }
      if (!upsertWorkingSetFrame(event, cycle.frame)) return
    }
    sessionEpochState(sessionId).baselineInjectedEpoch = scope.epoch
    rememberWorkingSetInjection(scope, cycle.fingerprint)
  } catch {
    // Working-set injection is advisory; never block the host context hook.
  }
}

/** `session.hook("prompt")` handler: activate anchors before the model call. */
export async function sessionPromptHandler(
  event: any,
  config: ResolvedConfig
): Promise<{ changed: boolean; activatedIds: string[] }> {
  const idle = { changed: false, activatedIds: [] }
  const sessionId = asString(event?.sessionID ?? event?.session_id)
  const scope = workingSetHookScope(config, sessionId)
  if (!scope || !event || typeof event !== 'object') return idle
  try {
    const promptText = wsMessageText(
      event.prompt ?? event.text ?? event.message ?? event.userPrompt ?? event
    )
    const cycle = await runWorkingSetPromptCycle({
      scope,
      promptText,
      turnId: asString(event.turnID ?? event.turnId ?? event.turn_id),
      eventId: asString(event.eventID ?? event.eventId ?? event.id)
    })
    return { changed: cycle.changed, activatedIds: cycle.activatedIds }
  } catch {
    return idle
  }
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
  const scope = workingSetHookScope(config, sessionId)
  if (!scope || wsReadParent(scope).status !== 'valid') return
  try {
    await runWorkingSetContextCycleInternal(scope, false)
    if (event && typeof event === 'object') {
      const parent = wsActiveParent(scope)
      event.result = {
        summary: renderWorkingSetCompactionSummary(
          await loadWorkingSet(scope),
          parent
        )
      }
    }
  } catch {
    // Compaction remains best-effort; the next context hook can retry.
  }
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
    // Codex CLI's PreToolUse hook sends apply_patch as {"command": "<patch>"}.
    extractPatchPath(args.command) ??
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
  'neurostrata_locate',
  'neurostrata_task_get'
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
  const toolName = canonicalToolName(
    extractToolName(event) || extractToolName(output)
  )

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

  const sessionId = asString(
    event?.sessionID ?? event?.session_id ?? meta?.sessionID ?? meta?.session_id
  )
  const epoch = currentSessionEpoch(sessionId)
  const scope = workingSetHookScope(config, sessionId)
  const resultContent = event?.result?.content
  const outputIsText = Array.isArray(resultContent)
    ? resultContent.every(
        (part: any) =>
          part && part.type === 'text' && typeof part.text === 'string'
      )
    : [
        event?.result?.output,
        event?.output,
        event?.result,
        event?.response,
        event?.content
      ].some((value) => typeof value === 'string')
  const nativeEventId = asString(
    event?.callID ??
      event?.call_id ??
      event?.toolCallId ??
      event?.tool_call_id ??
      event?.tool_use_id ??
      meta?.callID ??
      meta?.call_id ??
      meta?.toolCallId ??
      meta?.tool_call_id ??
      meta?.tool_use_id
  )
  const workingSetEventId = nativeEventId
    ? `opencode:execute.after:${nativeEventId}`
    : undefined
  const toolInput = asRecord(
    event?.args ??
      event?.input ??
      event?.toolInput ??
      meta?.args ??
      meta?.input ??
      meta?.toolInput
  )
  const isError =
    event?.status === 'error' ||
    event?.isError === true ||
    targetRec?.isError === true ||
    asRecord(event?.result).isError === true
  let replacementText: string | null = null
  if (scope) {
    try {
      const cycle = await runWorkingSetToolCycle({
        scope,
        turnId: asString(
          event?.turnID ?? event?.turn_id ?? meta?.turnID ?? meta?.turn_id
        ),
        eventId: workingSetEventId,
        toolName,
        toolInput,
        rawText: outputIsText ? rawText : null,
        outputIsText,
        isError,
        advanceTurn: true
      })
      replacementText = cycle.replacementText
    } catch {
      // Working-set collection is advisory and must not block tool execution.
    }
  }

  const suffixParts: string[] = []

  // 1. Pending recall block delivered by execute.before (legacy V1 / Claude / Hermes).
  let pendingBlock =
    consumePendingRecall(meta, targetRec) ??
    (targetRec
      ? (targetRec.neurostrataContext as string | undefined)
      : undefined) ??
    (meta ? (meta.neurostrataContext as string | undefined) : undefined)
  if (pendingBlock && scope)
    pendingBlock = await dehydrateWorkingSetRecall(scope, pendingBlock)
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
          let block = `[NeuroStrata File Context — ${normalizedPath}]\n${snippets.slice(0, 2).join('\n')}`
          if (scope) block = await dehydrateWorkingSetRecall(scope, block)
          suffixParts.push(block)
        }
      }
    }
  }

  const legacyPrefix = pendingBlock ? `${pendingBlock}\n\n` : ''
  const newSuffix = suffixParts.slice(pendingBlock ? 1 : 0).join('\n\n')
  const outputText = replacementText ?? rawText
  const combined = outputText
    ? `${legacyPrefix}${outputText}${newSuffix ? '\n\n' + newSuffix : ''}`
    : `${legacyPrefix}${newSuffix}`

  // Only mutate the event when there is actually something to prepend or
  // append; otherwise leave event.result.content / output / content untouched
  // (preserving the tool's original output, including V2 content arrays).
  const hasAnything =
    !!pendingBlock || newSuffix !== '' || replacementText !== null
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
    if (typeof event.result === 'string') event.result = combined
    if (typeof event.response === 'string') event.response = combined
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
  const toolName: string = canonicalToolName(
    input?.tool ??
      input?.tool_name ??
      input?.name ??
      input?.action ??
      input?.command ??
      ''
  )
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
        await prefetchMemoriesForFile(config.namespace, fp, {
          recordLedgerDir: config.projectRoot,
          projectRoot: config.projectRoot
        })
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
        snippets = await prefetchMemoriesForFile(config.namespace, filePath, {
          recordLedgerDir: config.projectRoot,
          projectRoot: config.projectRoot
        })
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
    ctx.session.hook('prompt', (event: any) =>
      sessionPromptHandler(event, config)
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
  const toolNameRaw = canonicalToolName(
    asString(event.tool_name) ?? asString(event.toolName) ?? ''
  )
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
      // Codex rejects `continue` on PreToolUse replies; the allow decision
      // alone is the contract. `continue` stays on Stop replies below.
      return {
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
    // Stop takes no permission decision. Codex CLI rejects a Stop reply that
    // carries one and reports the hook as failed; `continue: true` is valid
    // for both Claude Code and Codex (and matches the hermes/openclaw shims).
    return { continue: true }
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
