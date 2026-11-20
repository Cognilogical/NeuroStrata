// Builds the layouts of root-marker-cases.json for the plugin tests (core,
// hermes, openclaw); the Rust test root_marker::tests::fixture_cases_match
// builds the same layouts. The format is documented in the fixture's
// "_comment".

import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync
} from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const HERE = dirname(fileURLToPath(import.meta.url))

export const ROOT_MARKER_CASES = JSON.parse(
  readFileSync(join(HERE, 'root-marker-cases.json'), 'utf8')
).cases

/** The fixture's `via` spelling for each `findRootMarker` answer. */
export const VIA_NAMES = { own: 'own_checkout', main: 'main_checkout', nogit: 'no_git' }

/** `rel` (with `/`) under `base`, in this host's separators. */
export function under(base, rel) {
  return rel.split('/').reduce((path, part) => join(path, part), base)
}

/** A fresh temporary folder in its canonical spelling. */
export function canonicalTempDir(prefix) {
  return realpathSync.native(mkdtempSync(join(tmpdir(), prefix)))
}

/** Whether `dir` sits inside a git checkout (a no-git case cannot run there). */
export function insideGit(dir) {
  let current = dir
  while (true) {
    if (existsSync(join(current, '.git'))) return true
    const next = dirname(current)
    if (next === current) return false
    current = next
  }
}

export function writeMarker(dir, namespace) {
  mkdirSync(join(dir, '.NeuroStrata'), { recursive: true })
  writeFileSync(
    join(dir, '.NeuroStrata', 'config.json'),
    JSON.stringify({ version: 1, namespace })
  )
}

export function buildLayout(base, entries) {
  for (const entry of entries) {
    const space = entry.indexOf(' ')
    const rel = space < 0 ? entry : entry.slice(0, space)
    const kind = space < 0 ? null : entry.slice(space + 1)
    const path = under(base, rel)
    if (kind === null) {
      mkdirSync(path, { recursive: true })
    } else if (kind.startsWith('marker:')) {
      writeMarker(path, kind.slice('marker:'.length))
    } else if (kind.startsWith('git-file:')) {
      mkdirSync(path, { recursive: true })
      writeFileSync(join(path, '.git'), `gitdir: ${kind.slice('git-file:'.length)}\n`)
    } else if (kind.startsWith('commondir:')) {
      mkdirSync(path, { recursive: true })
      writeFileSync(join(path, 'commondir'), `${kind.slice('commondir:'.length)}\n`)
    } else if (kind.startsWith('neurostrata-link:')) {
      mkdirSync(path, { recursive: true })
      // A junction on Windows needs no privilege; it takes an absolute target.
      symlinkSync(
        under(base, kind.slice('neurostrata-link:'.length)),
        join(path, '.NeuroStrata'),
        process.platform === 'win32' ? 'junction' : 'dir'
      )
    } else if (kind === 'git-dir') {
      mkdirSync(join(path, '.git'), { recursive: true })
    } else if (kind === 'neurostrata-dir') {
      mkdirSync(join(path, '.NeuroStrata'), { recursive: true })
    } else if (kind === 'file') {
      mkdirSync(dirname(path), { recursive: true })
      writeFileSync(path, '')
    } else {
      throw new Error(`unknown layout kind ${kind} in ${entry}`)
    }
  }
}

/**
 * Runs every fixture case through `findRootMarker` and returns the
 * mismatches as strings (empty when all match). No-git cases are skipped
 * when the temporary folder sits inside a checkout.
 */
export function fixtureMismatches(findRootMarker) {
  const problems = []
  for (const c of ROOT_MARKER_CASES) {
    const base = canonicalTempDir('ns-root-marker-')
    let got
    try {
      buildLayout(base, c.layout)
      if (c.expect.via === 'no_git' && insideGit(base)) continue
      got = findRootMarker(under(base, c.query))
    } finally {
      rmSync(base, { recursive: true, force: true })
    }
    if (c.expect.root === null) {
      if (got !== null) problems.push(`${c.name}: expected none, got ${JSON.stringify(got)}`)
      continue
    }
    const want = {
      root: under(base, c.expect.root),
      namespace: c.expect.namespace,
      via: c.expect.via
    }
    const have = got && { root: got.root, namespace: got.namespace, via: VIA_NAMES[got.via] }
    if (JSON.stringify(have) !== JSON.stringify(want)) {
      problems.push(`${c.name}: expected ${JSON.stringify(want)}, got ${JSON.stringify(have)}`)
    }
  }
  return problems
}
