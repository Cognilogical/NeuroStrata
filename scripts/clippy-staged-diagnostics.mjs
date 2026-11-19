#!/usr/bin/env node
// Clippy diagnostic filter module.
// Exports pure findStagedDiagnostics(messages, root, stagedPaths) — no I/O.
// Invokes main() only when executed as CLI entry point.
// Usage from test import: import { findStagedDiagnostics } from "./clippy-staged-diagnostics.mjs" (named export)

import { fileURLToPath } from 'node:url'
import path from 'node:path'

/**
 * Finds staged Clippy diagnostics from a newline-delimited JSON stream.
 *
 * @param {string[]} messages - newline-delimited JSON parser messages
 * @param {string} root - repo root path (used for path normalization)
 * @param {string[]} stagedPaths - repo-relative staged file paths
 * @returns {string[]} rendered diagnostics matching a staged primary span
 */
function findStagedDiagnostics(messages, root, stagedPaths) {
  const rootNorm = root.replace(/\\/g, '/').replace(/\/$/, '')
  const stagedSet = new Set(stagedPaths.map((p) => normalizePath(p, rootNorm)))

  const matched = []

  for (const msg of messages) {
    if (!msg || typeof msg !== 'object') continue
    if (msg.reason !== 'compiler-message') continue
    if (!msg.message || typeof msg.message !== 'object') continue

    const spans = Array.isArray(msg.message.spans) ? msg.message.spans : []
    // Find primary span
    const primarySpan = spans.find((s) => s && s.is_primary === true)
    if (!primarySpan) continue

    const fileName = primarySpan.file_name
    if (!fileName) continue

    // Normalize the file path from the diagnostic using the supplied root
    const normFileName = normalizePath(fileName, rootNorm)
    if (!stagedSet.has(normFileName)) continue

    // Matched: staged warning or error
    const level = msg.message.level || 'warning'
    if (level !== 'warning' && level !== 'error') continue

    // Use rendered if present; otherwise build a useful fallback diagnostic
    // so a staged warning/error with missing rendered still counts as a match.
    let rendered = msg.message.rendered
    if (!rendered) {
      const lintText =
        typeof msg.message.message === 'string'
          ? msg.message.message
          : 'lint diagnostic'
      const line = primarySpan.line_start || 1
      const col = primarySpan.column_start || 1
      rendered = `${level}: ${lintText}\n --> ${normFileName}:${line}:${col}\n`
    }
    matched.push(rendered)
  }

  return matched
}

function normalizePath(p, rootNorm) {
  if (!p) return ''
  // Replace backslashes with forward slashes (Windows safety)
  let s = p.replace(/\\/g, '/')
  // Lexically collapse `.` and `..` segments (never touches disk)
  s = path.posix.normalize(s)
  // Strip leading ./
  while (s.startsWith('./')) s = s.slice(2)
  // Strip absolute root prefix
  if (s.startsWith(rootNorm + '/')) {
    s = s.slice(rootNorm.length + 1)
  } else if (s === rootNorm) {
    s = ''
  }
  return s
}

/**
 * CLI entry point. Reads NDJSON from stdin, exits:
 *   1 — a staged warning/error primary span was found
 *   0 — no staged warning/error matches
 *   2 — malformed JSON or missing --root/--staged args
 */
function main() {
  const args = process.argv.slice(2)
  let root = ''
  const staged = []

  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--root' && i + 1 < args.length) {
      root = args[++i]
    } else if (args[i] === '--staged' && i + 1 < args.length) {
      staged.push(args[++i])
    }
  }

  if (staged.length === 0 || root === '') {
    console.error(
      'usage: clippy-staged-diagnostics.mjs --root <root> --staged <path> [...]'
    )
    process.exit(2)
  }

  let stdin = ''
  process.stdin.setEncoding('utf8')
  process.stdin.on('data', (chunk) => {
    stdin += chunk
  })
  process.stdin.on('end', () => {
    if (!stdin.trim()) {
      process.exit(0)
    }

    let messages
    try {
      messages = stdin
        .split('\n')
        .filter((line) => line.trim())
        .map((line) => JSON.parse(line))
    } catch (e) {
      console.error('error: malformed Clippy NDJSON input')
      process.exit(2)
    }

    if (!Array.isArray(messages)) {
      console.error(
        'error: Clippy output must be newline-delimited JSON objects'
      )
      process.exit(2)
    }

    const matched = findStagedDiagnostics(messages, root, staged)

    for (const rendered of matched) {
      process.stdout.write(rendered)
      if (!rendered.endsWith('\n')) process.stdout.write('\n')
    }

    process.exit(matched.length > 0 ? 1 : 0)
  })
}

// Export pure function for importing tests
export { findStagedDiagnostics }

// Run main() only when executed as CLI (not imported)
const isMain =
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
if (isMain) {
  main()
}
