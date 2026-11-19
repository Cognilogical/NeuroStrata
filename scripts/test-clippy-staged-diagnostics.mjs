#!/usr/bin/env node
// Tests for scripts/clippy-staged-diagnostics.mjs
// Run with: node --test scripts/test-clippy-staged-diagnostics.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { findStagedDiagnostics } from './clippy-staged-diagnostics.mjs'

const HERE = path.dirname(fileURLToPath(import.meta.url))
const FILTER = path.join(HERE, 'clippy-staged-diagnostics.mjs')
const ROOT = path.resolve(HERE, '..')

function diag({
  file = 'src/main.rs',
  level = 'warning',
  primary = true,
  rendered = 'warning: something\n --> src/main.rs:1:1\n',
  message = 'something'
} = {}) {
  const span = {
    file_name: file,
    is_primary: primary,
    line_start: 1,
    line_end: 1,
    column_start: 1,
    column_end: 2
  }
  return {
    reason: 'compiler-message',
    message: {
      level,
      message,
      rendered,
      code: { code: 'some_lint', explanation: null },
      spans: primary
        ? [span]
        : [{ file_name: 'src/other.rs', is_primary: true }, span]
    }
  }
}

function runFilter({ input, args }) {
  const res = spawnSync(process.execPath, [FILTER, ...args], {
    input,
    encoding: 'utf8',
    cwd: ROOT
  })
  return res
}

test('warning primary span on staged file matches (exit 1)', () => {
  const input = JSON.stringify(diag({ file: 'src/main.rs', level: 'warning' }))
  const res = runFilter({
    input,
    args: ['--root', ROOT, '--staged', 'src/main.rs']
  })
  assert.equal(res.status, 1)
  assert.match(res.stdout, /warning: something/)
})

test('error primary span on staged file matches (exit 1)', () => {
  const input = JSON.stringify(diag({ file: 'src/main.rs', level: 'error' }))
  const res = runFilter({
    input,
    args: ['--root', ROOT, '--staged', 'src/main.rs']
  })
  assert.equal(res.status, 1)
})

test('non-primary span does not match (exit 0)', () => {
  const input = JSON.stringify(diag({ file: 'src/staged.rs', primary: false }))
  const res = runFilter({
    input,
    args: ['--root', ROOT, '--staged', 'src/staged.rs']
  })
  assert.equal(res.status, 0)
})

test('unrelated baseline warning does not match (exit 0)', () => {
  const input = JSON.stringify(diag({ file: 'src/unstaged.rs' }))
  const res = runFilter({
    input,
    args: ['--root', ROOT, '--staged', 'src/main.rs']
  })
  assert.equal(res.status, 0)
})

test('normal cargo non-diagnostic messages are ignored (exit 0)', () => {
  const input = [
    JSON.stringify({ reason: 'build-script-executed', package_id: 'x' }),
    JSON.stringify({ reason: 'compiler-artifact', package_id: 'x' })
  ].join('\n')
  const res = runFilter({
    input,
    args: ['--root', ROOT, '--staged', 'src/main.rs']
  })
  assert.equal(res.status, 0)
})

test('malformed JSON exits 2', () => {
  const res = runFilter({
    input: '{not json',
    args: ['--root', ROOT, '--staged', 'src/main.rs']
  })
  assert.equal(res.status, 2)
})

test('absolute path under root is normalized and matched (exit 1)', () => {
  const input = JSON.stringify(diag({ file: `${ROOT}/src/main.rs` }))
  const res = runFilter({
    input,
    args: ['--root', ROOT, '--staged', 'src/main.rs']
  })
  assert.equal(res.status, 1)
})

test('./ relative path is normalized and matched (exit 1)', () => {
  const input = JSON.stringify(diag({ file: './src/main.rs' }))
  const res = runFilter({
    input,
    args: ['--root', ROOT, '--staged', 'src/main.rs']
  })
  assert.equal(res.status, 1)
})

test('Cargo manifest staged path matches (exit 1)', () => {
  const input = JSON.stringify(diag({ file: 'Cargo.toml' }))
  const res = runFilter({
    input,
    args: ['--root', ROOT, '--staged', 'Cargo.toml']
  })
  assert.equal(res.status, 1)
})

test('multiple staged files: any matching diagnostic triggers exit 1', () => {
  const input = [
    JSON.stringify(diag({ file: 'src/a.rs' })),
    JSON.stringify(diag({ file: 'src/b.rs' }))
  ].join('\n')
  const res = runFilter({
    input,
    args: ['--root', ROOT, '--staged', 'src/a.rs', '--staged', 'src/b.rs']
  })
  assert.equal(res.status, 1)
})

test('rendered text is preserved in output', () => {
  const rendered = 'error[E0000]: boom\n --> src/main.rs:2:3\n'
  const input = JSON.stringify(diag({ rendered }))
  const res = runFilter({
    input,
    args: ['--root', ROOT, '--staged', 'src/main.rs']
  })
  assert.equal(res.status, 1)
  assert.ok(res.stdout.includes('error[E0000]: boom'))
})

test('missing --root or --staged is a usage error (exit 2)', () => {
  const res = runFilter({ input: '[]', args: [] })
  assert.equal(res.status, 2)
})

test('named import findStagedDiagnostics is pure: no I/O, no exit, returns matches', () => {
  const diag1 = diag({
    file: 'src/main.rs',
    level: 'warning',
    rendered: 'warning: x\n'
  })
  const matched = findStagedDiagnostics([diag1], ROOT, ['src/main.rs'])
  assert.equal(matched.length, 1)
  assert.ok(matched[0].includes('warning: x'))
  // Empty staged list -> no matches, pure return
  assert.equal(findStagedDiagnostics([diag1], ROOT, []).length, 0)
  // Non-staged file -> no match
  assert.equal(findStagedDiagnostics([diag1], ROOT, ['src/other.rs']).length, 0)
  // Non-diagnostic messages ignored
  assert.equal(
    findStagedDiagnostics([{ reason: 'build-script-executed' }], ROOT, [
      'src/main.rs'
    ]).length,
    0
  )
})

test('custom root different from cwd: matching uses supplied root with absolute spans', () => {
  const customRoot = '/custom/repo/root'
  const msg = {
    reason: 'compiler-message',
    message: {
      level: 'warning',
      message: 'unused variable',
      rendered:
        'warning: unused variable\n --> /custom/repo/root/src/lib.rs:3:10\n',
      code: { code: 'unused_variables', explanation: null },
      spans: [
        {
          file_name: `${customRoot}/src/lib.rs`,
          is_primary: true,
          line_start: 3,
          column_start: 10,
          line_end: 3,
          column_end: 12
        }
      ]
    }
  }
  // cwd is ROOT (real repo), but customRoot is supplied; matching must use supplied root
  const matched = findStagedDiagnostics([msg], customRoot, ['src/lib.rs'])
  assert.equal(matched.length, 1)
  assert.ok(matched[0].includes('unused variable'))
})

test('custom root with Windows backslashes is normalized for matching', () => {
  const winRoot = 'C:\\custom\\repo\\root'
  const msg = {
    reason: 'compiler-message',
    message: {
      level: 'error',
      message: 'type mismatch',
      rendered:
        'error: type mismatch\n --> C:\\custom\\repo\\root\\src\\main.rs:1:1\n',
      code: { code: 'type_mismatch', explanation: null },
      spans: [
        {
          file_name: `C:\\custom\\repo\\root\\src\\main.rs`,
          is_primary: true,
          line_start: 1,
          column_start: 1,
          line_end: 1,
          column_end: 2
        }
      ]
    }
  }
  const matched = findStagedDiagnostics([msg], winRoot, ['src/main.rs'])
  assert.equal(matched.length, 1)
  assert.ok(matched[0].includes('type mismatch'))
})

test('tests/../src/... lexically normalizes to src/... and matches (exit 1)', () => {
  const input = JSON.stringify(diag({ file: 'tests/../src/main.rs' }))
  const res = runFilter({
    input,
    args: ['--root', ROOT, '--staged', 'src/main.rs']
  })
  assert.equal(res.status, 1)
})

test('tests/../src/... matches via pure findStagedDiagnostics', () => {
  const msg = diag({ file: 'tests/../src/main.rs' })
  const matched = findStagedDiagnostics([msg], ROOT, ['src/main.rs'])
  assert.equal(matched.length, 1)
})

test('tests/../src/... does NOT match a different staged file (exit 0)', () => {
  const input = JSON.stringify(diag({ file: 'tests/../src/other.rs' }))
  const res = runFilter({
    input,
    args: ['--root', ROOT, '--staged', 'src/main.rs']
  })
  assert.equal(res.status, 0)
})

test('deeply nested ../ segments normalize correctly', () => {
  const msg = diag({ file: 'a/b/../../src/main.rs' })
  const matched = findStagedDiagnostics([msg], ROOT, ['src/main.rs'])
  assert.equal(matched.length, 1)
})

test('non-ASCII and special characters in paths are preserved', () => {
  const msg = diag({ file: 'src/café/naïve.rs' })
  const matched = findStagedDiagnostics([msg], ROOT, ['src/café/naïve.rs'])
  assert.equal(matched.length, 1)
})

test('staged warning with missing rendered still matches via fallback (exit 1)', () => {
  const msg = {
    reason: 'compiler-message',
    message: {
      level: 'warning',
      message: 'dead code detected',
      spans: [
        {
          file_name: 'src/dead.rs',
          is_primary: true,
          line_start: 2,
          column_start: 1,
          line_end: 2,
          column_end: 2
        }
      ]
    }
  }
  // Pure function returns a fallback diagnostic
  const matched = findStagedDiagnostics([msg], ROOT, ['src/dead.rs'])
  assert.equal(matched.length, 1)
  assert.ok(matched[0].includes('dead code detected'))
  assert.ok(matched[0].includes('src/dead.rs:2:1'))
  // CLI emits exit 1 with fallback text
  const res = runFilter({
    input: JSON.stringify(msg),
    args: ['--root', ROOT, '--staged', 'src/dead.rs']
  })
  assert.equal(res.status, 1)
  assert.ok(res.stdout.includes('dead code detected'))
})

test('staged error with empty rendered still matches via fallback (exit 1)', () => {  const msg = {
    reason: 'compiler-message',
    message: {
      level: 'error',
      message: 'type annotation required',
      rendered: '',
      spans: [
        {
          file_name: 'src/err.rs',
          is_primary: true,
          line_start: 5,
          column_start: 4,
          line_end: 5,
          column_end: 6
        }
      ]
    }
  }
  const matched = findStagedDiagnostics([msg], ROOT, ['src/err.rs'])
  assert.equal(matched.length, 1)
  const res = runFilter({
    input: JSON.stringify(msg),
    args: ['--root', ROOT, '--staged', 'src/err.rs']
  })
  assert.equal(res.status, 1)
  assert.ok(res.stdout.includes('type annotation required'))
  assert.ok(res.stdout.includes('src/err.rs:5:4'))
})

test('runner-style --staged with several paths catches a diagnostic in the second', () => {
  const input = JSON.stringify(diag({ file: 'src/second.rs' }))
  const res = runFilter({
    input,
    args: ['--root', ROOT, '--staged', 'src/first.rs', 'src/second.rs']
  })
  assert.equal(res.status, 1)
  assert.match(res.stdout, /warning: something/)
})

test('dep-info coverage distinguishes built and unbuilt staged Rust files', async () => {
  const { findUnbuiltStagedRustFiles } = await import(
    './clippy-staged-diagnostics.mjs'
  )
  assert.equal(typeof findUnbuiltStagedRustFiles, 'function')
  const deps = [
    'target/debug/deps/main.d: /repo/src/main.rs /repo/src/lib.rs',
    'target/debug/deps/tool.d:/repo/src/bin/tool.rs'
  ]
  assert.deepEqual(
    findUnbuiltStagedRustFiles(deps, '/repo', [
      'src/main.rs',
      'src/lib.rs',
      'src/bin/tool.rs',
      'Cargo.toml'
    ]),
    []
  )
  assert.deepEqual(
    findUnbuiltStagedRustFiles(deps, '/repo', [
      'src/main.rs',
      'fixtures/input.rs'
    ]),
    ['fixtures/input.rs']
  )
})

test('dep-info coverage handles escaped spaces and symlinked roots', async () => {
  const { findUnbuiltStagedRustFiles } = await import(
    './clippy-staged-diagnostics.mjs'
  )
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'clippy-dep-info-'))
  const realRoot = path.join(dir, 'real')
  fs.mkdirSync(path.join(realRoot, 'src'), { recursive: true })
  fs.writeFileSync(path.join(realRoot, 'src', 'a b.rs'), 'fn a() {}\n')
  const linkRoot = path.join(dir, 'link')
  fs.symlinkSync(realRoot, linkRoot)
  const deps = [
    `target/debug/deps/a.d: ${realRoot}/src/a\\ b.rs`
  ]
  assert.deepEqual(
    findUnbuiltStagedRustFiles(deps, linkRoot, ['src/a b.rs']),
    []
  )
  fs.rmSync(dir, { recursive: true, force: true })
})

test('CLI dep-info rejects unbuilt staged Rust and preserves diagnostics', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'clippy-dep-cli-'))
  const depRoot = path.join(dir, 'target')
  fs.mkdirSync(path.join(depRoot, 'debug', 'deps'), { recursive: true })
  fs.writeFileSync(
    path.join(depRoot, 'debug', 'deps', 'main.d'),
    `${dir}/target/debug/deps/main: ${dir}/src/main.rs\n`
  )
  const common = ['--root', dir, '--dep-info-root', depRoot, '--staged']
  const clean = runFilter({
    input: '',
    args: [...common, 'src/main.rs']
  })
  assert.equal(clean.status, 0)

  const unbuilt = runFilter({
    input: '',
    args: [...common, 'src/main.rs', 'fixtures/input.rs']
  })
  assert.equal(unbuilt.status, 1)
  assert.match(unbuilt.stderr, /fixtures\/input\.rs.*not.*build/i)

  const both = runFilter({
    input: JSON.stringify(diag({ file: `${dir}/src/main.rs` })),
    args: [...common, 'src/main.rs', 'fixtures/input.rs']
  })
  assert.equal(both.status, 1)
  assert.match(both.stdout, /warning: something/)
  assert.match(both.stderr, /fixtures\/input\.rs.*not.*build/i)
  fs.rmSync(dir, { recursive: true, force: true })
})

test('symlinked CLI path still runs main (exit 1)', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'clippy-symlink-'))
  const link = path.join(dir, 'filter-link.mjs')
  fs.symlinkSync(FILTER, link)
  const res = spawnSync(
    process.execPath,
    [link, '--root', ROOT, '--staged', 'src/main.rs'],
    {
      input: JSON.stringify(diag({ file: 'src/main.rs' })),
      encoding: 'utf8',
      cwd: ROOT
    }
  )
  fs.rmSync(dir, { recursive: true, force: true })
  assert.equal(res.status, 1)
  assert.match(res.stdout, /warning: something/)
})
