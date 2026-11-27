#!/usr/bin/env node
/**
 * bundle-adapter.mjs — Inlines @neurostrata/plugin-core into each harness adapter's dist/.
 *
 * Usage (from adapter directory, e.g., plugins/opencode):
 *   node ../core/bundle-adapter.mjs
 *
 * Compiled OpenClaw/Hermes shims:
 *   node ../core/bundle-adapter.mjs --compiled-adapter
 *
 * Every invocation regenerates index.js, index.js.map, index.d.ts, and
 * index.d.ts.map. --watch recompiles core before rebundling thin adapters.
 */

import { execFileSync } from 'node:child_process'
import { mkdirSync, readFileSync, rmSync, watch, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = dirname(fileURLToPath(import.meta.url))
const rootDir = resolve(__dirname, '..')
const coreDir = resolve(rootDir, 'core')
const coreModule = '@neurostrata/plugin-core'
const artifactNames = [
  'index.js',
  'index.js.map',
  'index.d.ts',
  'index.d.ts.map'
]
const vlqAlphabet =
  'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
const vlqDigits = new Map([...vlqAlphabet].map((char, index) => [char, index]))

function decodeVlq(segment) {
  const values = []
  let raw = 0
  let shift = 0
  for (const char of segment) {
    const digit = vlqDigits.get(char)
    if (digit === undefined)
      throw new Error(`Invalid source-map VLQ character: ${char}`)
    raw += (digit & 31) * 2 ** shift
    if (digit & 32) {
      shift += 5
      continue
    }
    const magnitude = Math.floor(raw / 2)
    values.push(raw % 2 === 1 ? -magnitude : magnitude)
    raw = 0
    shift = 0
  }
  if (raw !== 0 || shift !== 0)
    throw new Error('Truncated source-map VLQ segment')
  return values
}

function encodeVlq(value) {
  let raw = value < 0 ? (-value << 1) | 1 : value << 1
  let encoded = ''
  do {
    let digit = raw & 31
    raw >>>= 5
    if (raw !== 0) digit |= 32
    encoded += vlqAlphabet[digit]
  } while (raw !== 0)
  return encoded
}

function decodeMappings(mappings) {
  let source = 0
  let originalLine = 0
  let originalColumn = 0
  let name = 0
  return mappings.split(';').map((line) => {
    let generatedColumn = 0
    if (line === '') return []
    return line.split(',').map((segment) => {
      const values = decodeVlq(segment)
      if (![1, 4, 5].includes(values.length)) {
        throw new Error(`Invalid source-map segment width: ${values.length}`)
      }
      generatedColumn += values[0]
      const decoded = [generatedColumn]
      if (values.length >= 4) {
        source += values[1]
        originalLine += values[2]
        originalColumn += values[3]
        decoded.push(source, originalLine, originalColumn)
      }
      if (values.length === 5) {
        name += values[4]
        decoded.push(name)
      }
      return decoded
    })
  })
}

function encodeMappings(lines) {
  let source = 0
  let originalLine = 0
  let originalColumn = 0
  let name = 0
  return lines
    .map((line) => {
      let generatedColumn = 0
      return line
        .map((segment) => {
          const values = [segment[0] - generatedColumn]
          generatedColumn = segment[0]
          if (segment.length >= 4) {
            values.push(
              segment[1] - source,
              segment[2] - originalLine,
              segment[3] - originalColumn
            )
            source = segment[1]
            originalLine = segment[2]
            originalColumn = segment[3]
          }
          if (segment.length === 5) {
            values.push(segment[4] - name)
            name = segment[4]
          }
          return values.map(encodeVlq).join('')
        })
        .join(',')
    })
    .join(';')
}

function readSourceMap(path) {
  const map = JSON.parse(readFileSync(path, 'utf8'))
  if (map.version !== 3 || typeof map.mappings !== 'string') {
    throw new Error(`Invalid source map: ${path}`)
  }
  if (
    !Array.isArray(map.sources) ||
    map.sources.length !== 1 ||
    typeof map.sources[0] !== 'string'
  ) {
    throw new Error(`Expected one source in ${path}`)
  }
  let source
  if (
    Array.isArray(map.sourcesContent) &&
    typeof map.sourcesContent[0] === 'string'
  ) {
    source = map.sourcesContent[0]
  } else {
    const sourceRoot = map.sourceRoot === undefined ? '' : map.sourceRoot
    if (typeof sourceRoot !== 'string') {
      throw new Error(`Invalid sourceRoot in ${path}`)
    }
    source = readFileSync(
      resolve(dirname(path), sourceRoot, map.sources[0]),
      'utf8'
    )
  }
  return {
    source,
    names: Array.isArray(map.names) ? map.names : [],
    lines: decodeMappings(map.mappings)
  }
}

function splitLines(text) {
  const lines = []
  let start = 0
  while (start < text.length) {
    let contentEnd = start
    while (
      contentEnd < text.length &&
      text[contentEnd] !== '\n' &&
      text[contentEnd] !== '\r'
    ) {
      contentEnd += 1
    }
    let end = contentEnd
    let eol = ''
    if (text[end] === '\r' && text[end + 1] === '\n') {
      eol = '\r\n'
      end += 2
    } else if (text[end] === '\r' || text[end] === '\n') {
      eol = text[end]
      end += 1
    }
    lines.push({ start, contentEnd, end, eol })
    start = end
  }
  if (text === '' || start === text.length)
    lines.push({
      start: text.length,
      contentEnd: text.length,
      end: text.length,
      eol: ''
    })
  return lines
}

function lineIndexAt(lines, offset) {
  let low = 0
  let high = lines.length - 1
  while (low <= high) {
    const middle = Math.floor((low + high) / 2)
    if (offset < lines[middle].start) high = middle - 1
    else low = middle + 1
  }
  return Math.max(0, Math.min(lines.length - 1, low - 1))
}

function mergeRanges(ranges) {
  const sorted = ranges
    .filter((range) => range.end > range.start)
    .sort((left, right) => left.start - right.start || left.end - right.end)
  const merged = []
  for (const range of sorted) {
    const previous = merged.at(-1)
    if (previous && range.start <= previous.end)
      previous.end = Math.max(previous.end, range.end)
    else merged.push({ ...range })
  }
  return merged
}

function expandRange(text, lines, range) {
  let { start, end } = range
  const first = lines[lineIndexAt(lines, start)]
  if (/^[\t ]*$/.test(text.slice(first.start, start))) start = first.start
  const last = lines[lineIndexAt(lines, Math.max(start, end - 1))]
  if (last.eol && /^[\t ]*$/.test(text.slice(end, last.contentEnd)))
    end = last.end
  return { start, end }
}

function sourceMapCommentRanges(text, lines) {
  return lines
    .filter((line) =>
      /^[\t ]*\/\/[#@][\t ]*sourceMappingURL=/.test(
        text.slice(line.start, line.contentEnd)
      )
    )
    .map((line) => ({ start: line.start, end: line.end }))
}

function tokenize(text) {
  const tokens = []
  let index = 0
  let line = 0
  let braces = 0
  let parentheses = 0
  let brackets = 0

  function advance(count = 1) {
    for (let step = 0; step < count && index < text.length; step += 1) {
      if (text[index] === '\n') line += 1
      else if (text[index] === '\r' && text[index + 1] !== '\n') line += 1
      index += 1
    }
  }

  function skipQuoted(quote) {
    advance()
    while (index < text.length) {
      if (text[index] === '\\') advance(2)
      else if (text[index] === quote) {
        advance()
        return
      } else advance()
    }
  }

  function skipComment() {
    if (text.startsWith('//', index)) {
      while (
        index < text.length &&
        text[index] !== '\n' &&
        text[index] !== '\r'
      )
        advance()
      return
    }
    advance(2)
    while (index < text.length && !text.startsWith('*/', index)) advance()
    if (index < text.length) advance(2)
  }

  function skipTemplateExpression() {
    let depth = 1
    while (index < text.length && depth > 0) {
      if (text.startsWith('//', index) || text.startsWith('/*', index))
        skipComment()
      else if (text[index] === "'" || text[index] === '"')
        skipQuoted(text[index])
      else if (text[index] === '`') skipTemplate()
      else {
        if (text[index] === '{') depth += 1
        else if (text[index] === '}') depth -= 1
        advance()
      }
    }
  }

  function skipTemplate() {
    advance()
    while (index < text.length) {
      if (text[index] === '\\') advance(2)
      else if (text[index] === '`') {
        advance()
        return
      } else if (text.startsWith('${', index)) {
        advance(2)
        skipTemplateExpression()
      } else advance()
    }
  }

  function skipRegex() {
    advance()
    let inClass = false
    while (index < text.length) {
      if (text[index] === '\\') advance(2)
      else if (text[index] === '[') {
        inClass = true
        advance()
      } else if (text[index] === ']') {
        inClass = false
        advance()
      } else if (text[index] === '/' && !inClass) {
        advance()
        while (/[A-Za-z]/.test(text[index] ?? '')) advance()
        return
      } else if (text[index] === '\n' || text[index] === '\r') return
      else advance()
    }
  }

  const regexPrefixes = new Set([
    '(',
    '[',
    '{',
    ',',
    ':',
    ';',
    '=',
    '!',
    '?',
    '=>',
    'return',
    'case',
    'throw',
    'else',
    'do',
    'typeof',
    'void',
    'delete',
    'in',
    'of',
    '&&',
    '||',
    '??',
    '+',
    '-',
    '*',
    '%',
    '&',
    '|',
    '^',
    '~',
    '<',
    '>',
    '<=',
    '>=',
    '==',
    '==='
  ])

  while (index < text.length) {
    const char = text[index]
    if (/\s/.test(char)) {
      advance()
      continue
    }
    if (text.startsWith('//', index) || text.startsWith('/*', index)) {
      skipComment()
      continue
    }
    if (char === "'" || char === '"') {
      const start = index
      const quote = char
      skipQuoted(quote)
      tokens.push({
        kind: 'string',
        value: text.slice(start + 1, index - 1),
        start,
        end: index,
        line,
        depth: braces + parentheses + brackets
      })
      continue
    }
    if (char === '`') {
      skipTemplate()
      continue
    }
    if (char === '/' && regexPrefixes.has(tokens.at(-1)?.value)) {
      skipRegex()
      continue
    }
    if (/[A-Za-z_$]/.test(char)) {
      const start = index
      advance()
      while (/[A-Za-z0-9_$]/.test(text[index] ?? '')) advance()
      tokens.push({
        kind: 'identifier',
        value: text.slice(start, index),
        start,
        end: index,
        line,
        depth: braces + parentheses + brackets
      })
      continue
    }
    const start = index
    const pair = text.slice(index, index + 2)
    const value = ['=>', '&&', '||', '??', '==', '===', '<=', '>='].includes(
      pair
    )
      ? pair
      : char
    advance(value.length)
    tokens.push({
      kind: 'punctuation',
      value,
      start,
      end: index,
      line,
      depth: braces + parentheses + brackets
    })
    if (value === '{') braces += 1
    else if (value === '}') braces = Math.max(0, braces - 1)
    else if (value === '(') parentheses += 1
    else if (value === ')') parentheses = Math.max(0, parentheses - 1)
    else if (value === '[') brackets += 1
    else if (value === ']') brackets = Math.max(0, brackets - 1)
  }
  return tokens
}

function statementEnd(tokens, from) {
  const semicolon = tokens.slice(from).find((token) => token.value === ';')
  return semicolon
    ? semicolon.end
    : (tokens[from - 1]?.end ?? tokens[from].start)
}

function hasAliasedNamedImport(clause) {
  const open = clause.findIndex((token) => token.value === '{')
  if (open === -1) return false
  let depth = 0
  for (const token of clause.slice(open)) {
    if (token.value === '{') depth += 1
    else if (token.value === '}') depth -= 1
    else if (token.value === 'as' && depth === 1) return true
  }
  return false
}

function analyzeModuleSyntax(text) {
  const tokens = tokenize(text)
  const removals = []
  let defaultExports = 0

  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index]
    if (token.depth !== 0) continue
    if (token.value === 'export' && tokens[index + 1]?.value === 'default') {
      defaultExports += 1
      continue
    }
    if (token.value === 'export' && tokens[index + 1]?.value === '{') {
      let depth = 0
      for (let cursor = index + 1; cursor < tokens.length; cursor += 1) {
        if (tokens[cursor].value === '{') depth += 1
        else if (tokens[cursor].value === '}') depth -= 1
        else if (
          tokens[cursor].value === 'as' &&
          tokens[cursor + 1]?.value === 'default' &&
          depth === 1
        ) {
          defaultExports += 1
          break
        }
        if (depth === 0) break
      }
    }
    if (token.value !== 'import' && token.value !== 'export') continue
    if (
      token.value === 'import' &&
      (tokens[index + 1]?.value === '(' || tokens[index + 1]?.value === '.')
    )
      continue

    let cursor = index + 1
    if (token.value === 'export' && tokens[cursor]?.value === 'default')
      continue
    const exportType =
      token.value === 'export' && tokens[cursor]?.value === 'type'
    if (exportType) cursor += 1
    if (
      token.value === 'export' &&
      tokens[cursor]?.value !== '{' &&
      tokens[cursor]?.value !== '*'
    )
      continue

    let fromIndex = -1
    let moduleToken = null
    for (; cursor < tokens.length; cursor += 1) {
      if (
        tokens[cursor].value === 'from' &&
        tokens[cursor + 1]?.kind === 'string'
      ) {
        fromIndex = cursor
        moduleToken = tokens[cursor + 1]
        break
      }
      if (tokens[cursor].value === ';' && tokens[cursor].depth === 0) break
    }
    if (
      !moduleToken &&
      token.value === 'import' &&
      tokens[index + 1]?.kind === 'string'
    ) {
      fromIndex = index + 1
      moduleToken = tokens[index + 1]
    }
    if (!moduleToken || moduleToken.value !== coreModule) continue

    if (token.value === 'import') {
      const clause = tokens.slice(index + 1, fromIndex)
      const first = clause[0]
      const named =
        first?.value === '{' ||
        (first?.value === 'type' && clause[1]?.value === '{')
      const namespace =
        first?.value === '*' ||
        (first?.value === 'type' && clause[1]?.value === '*')
      if (
        (!named && !namespace) ||
        namespace ||
        hasAliasedNamedImport(clause)
      ) {
        throw new Error('Core imports must be unaliased named imports')
      }
    }
    removals.push({ start: token.start, end: statementEnd(tokens, cursor + 2) })
  }
  return { removals: mergeRanges(removals), defaultExports }
}

function coreDefaultRanges(text, declaration) {
  const tokens = tokenize(text)
  const ranges = []
  if (declaration) {
    for (let index = 0; index + 2 < tokens.length; index += 1) {
      if (
        tokens[index].depth !== 0 ||
        tokens[index].value !== 'declare' ||
        tokens[index + 1].value !== 'const' ||
        tokens[index + 2].value !== '_default'
      )
        continue
      let depth = 0
      for (let cursor = index + 3; cursor < tokens.length; cursor += 1) {
        if (tokens[cursor].value === '{') depth += 1
        else if (tokens[cursor].value === '}') depth -= 1
        else if (tokens[cursor].value === ';' && depth === 0) {
          ranges.push({ start: tokens[index].start, end: tokens[cursor].end })
          break
        }
      }
    }
  }
  for (let index = 0; index + 1 < tokens.length; index += 1) {
    if (
      tokens[index].depth === 0 &&
      tokens[index].value === 'export' &&
      tokens[index + 1].value === 'default'
    ) {
      ranges.push({
        start: tokens[index].start,
        end: statementEnd(tokens, index + 2)
      })
    }
  }
  return ranges
}

function removeRanges(text, inputLines, rawRanges) {
  const lines = splitLines(text)
  const ranges = mergeRanges(
    rawRanges.map((range) => expandRange(text, lines, range))
  )
  let output = ''
  const outputPieces = []

  for (let lineIndex = 0; lineIndex < lines.length; lineIndex += 1) {
    const line = lines[lineIndex]
    const intersections = []
    for (const range of ranges) {
      const start = Math.max(range.start, line.start)
      const end = Math.min(range.end, line.contentEnd)
      if (end > start) intersections.push({ start, end })
    }
    const removesEol = ranges.some(
      (range) => range.start <= line.start && range.end >= line.end && line.eol
    )
    const pieces = []
    let cursor = line.start
    for (const intersection of intersections) {
      if (intersection.start > cursor)
        pieces.push({ start: cursor, end: intersection.start })
      cursor = Math.max(cursor, intersection.end)
    }
    if (cursor < line.contentEnd)
      pieces.push({ start: cursor, end: line.contentEnd })
    const keptLength = pieces.reduce(
      (total, piece) => total + piece.end - piece.start,
      0
    )
    if (keptLength === 0 && removesEol) continue

    const generatedStart = output.length
    let generatedCursor = generatedStart
    const mappedPieces = pieces.map((piece) => {
      const mapped = {
        originalStart: piece.start,
        originalEnd: piece.end,
        generatedStart: generatedCursor
      }
      output += text.slice(piece.start, piece.end)
      generatedCursor += piece.end - piece.start
      return mapped
    })
    if (!removesEol) output += line.eol
    outputPieces.push({
      sourceLine: lineIndex,
      pieces: mappedPieces,
      contentLength: keptLength,
      generatedStart
    })
  }

  const mappedLines = outputPieces.map(
    ({ sourceLine, pieces, contentLength, generatedStart }) => {
      const sourceLineSegments = inputLines[sourceLine] ?? []
      const sourceStart = lines[sourceLine].start
      const mapped = []
      for (const segment of sourceLineSegments) {
        const absoluteColumn = sourceStart + segment[0]
        let delta = null
        if (absoluteColumn === lines[sourceLine].contentEnd)
          delta = contentLength
        else {
          for (const piece of pieces) {
            if (
              absoluteColumn >= piece.originalStart &&
              absoluteColumn < piece.originalEnd
            ) {
              delta =
                piece.generatedStart -
                generatedStart +
                absoluteColumn -
                piece.originalStart
              break
            }
          }
        }
        if (delta !== null) mapped.push([delta, ...segment.slice(1)])
      }
      return mapped
    }
  )
  return { text: output, lines: mappedLines }
}

function remapLines(lines, sourceIndex, nameOffset) {
  return lines.map((line) =>
    line.map((segment) => {
      const mapped = [...segment]
      if (mapped.length >= 4) mapped[1] = sourceIndex
      if (mapped.length === 5) mapped[4] += nameOffset
      return mapped
    })
  )
}

function identityLines(text, originalLineOffset = 0) {
  return splitLines(text).map((line, index) => {
    const content = text.slice(line.start, line.contentEnd)
    return content.trim() === '' ? [] : [[0, 0, originalLineOffset + index, 0]]
  })
}

function prepareCore() {
  const jsMap = readSourceMap(join(coreDir, 'dist/index.js.map'))
  const dtsMap = readSourceMap(join(coreDir, 'dist/index.d.ts.map'))
  const jsText = readFileSync(join(coreDir, 'dist/index.js'), 'utf8')
  const dtsText = readFileSync(join(coreDir, 'dist/index.d.ts'), 'utf8')
  const jsLines = splitLines(jsText)
  const dtsLines = splitLines(dtsText)
  const js = removeRanges(jsText, jsMap.lines, [
    ...coreDefaultRanges(jsText, false),
    ...sourceMapCommentRanges(jsText, jsLines)
  ])
  const dts = removeRanges(dtsText, dtsMap.lines, [
    ...coreDefaultRanges(dtsText, true),
    ...sourceMapCommentRanges(dtsText, dtsLines)
  ])
  return {
    js: {
      text: js.text,
      lines: remapLines(js.lines, 0, 0),
      names: jsMap.names
    },
    dts: {
      text: dts.text,
      lines: remapLines(dts.lines, 0, 0),
      names: dtsMap.names
    },
    jsSource: jsMap.source,
    dtsSource: dtsMap.source
  }
}

function prepareCompiledShim(compiledDir) {
  const jsMap = readSourceMap(join(compiledDir, 'index.js.map'))
  const dtsMap = readSourceMap(join(compiledDir, 'index.d.ts.map'))
  const jsText = readFileSync(join(compiledDir, 'index.js'), 'utf8')
  const dtsText = readFileSync(join(compiledDir, 'index.d.ts'), 'utf8')
  const jsSyntax = analyzeModuleSyntax(jsText)
  const dtsSyntax = analyzeModuleSyntax(dtsText)
  if (jsSyntax.defaultExports !== 1 || dtsSyntax.defaultExports !== 1) {
    throw new Error('Compiled shim must contain exactly one default export')
  }
  const jsLines = splitLines(jsText)
  const dtsLines = splitLines(dtsText)
  const js = removeRanges(jsText, jsMap.lines, [
    ...jsSyntax.removals,
    ...sourceMapCommentRanges(jsText, jsLines)
  ])
  const dts = removeRanges(dtsText, dtsMap.lines, [
    ...dtsSyntax.removals,
    ...sourceMapCommentRanges(dtsText, dtsLines)
  ])
  return {
    js: { text: js.text, lines: js.lines, names: jsMap.names },
    dts: { text: dts.text, lines: dts.lines, names: dtsMap.names },
    jsSource: jsMap.source,
    dtsSource: dtsMap.source
  }
}

function extractPluginSource(source) {
  const lines = splitLines(source)
  const start = lines.findIndex((line) => {
    const trimmed = source.slice(line.start, line.contentEnd).trim()
    return (
      trimmed.startsWith('export const plugin = createHarnessPlugin(') ||
      trimmed.startsWith('export default plugin;') ||
      trimmed.startsWith('export default {')
    )
  })
  if (start === -1) throw new Error('Could not locate adapter plugin export')
  const text = source.slice(lines[start].start)
  const syntax = analyzeModuleSyntax(text)
  if (syntax.defaultExports !== 1)
    throw new Error('Adapter must contain exactly one default export')
  const inputLines = identityLines(text, start)
  const ranges = [
    ...syntax.removals,
    ...sourceMapCommentRanges(text, splitLines(text))
  ]
  const prepared = removeRanges(text, inputLines, ranges)
  return {
    js: { text: prepared.text, lines: prepared.lines, names: [] },
    jsSource: source,
    declarationLine: start
  }
}

function appendSourceMapComment(text, lines, file) {
  const comment = `//# sourceMappingURL=${file}`
  if (text === '') return { text: `${comment}\n`, lines: [[]] }
  if (text.endsWith('\n') || text.endsWith('\r')) {
    return { text: `${text}${comment}\n`, lines: [...lines.slice(0, -1), []] }
  }
  return { text: `${text}\n${comment}\n`, lines: [...lines, []] }
}

function compose(core, shim, file, coreSource, shimSource) {
  const separator =
    core.text === '' || core.text.endsWith('\n') || core.text.endsWith('\r')
      ? ''
      : '\n'
  const coreLines =
    core.text === ''
      ? []
      : core.text.endsWith('\n') || core.text.endsWith('\r')
        ? core.lines.slice(0, -1)
        : core.lines
  const text = `${core.text}${separator}${shim.text}`
  const lines = [...coreLines, ...remapLines(shim.lines, 1, core.names.length)]
  const withComment = appendSourceMapComment(text, lines, `${file}.map`)
  return {
    text: withComment.text,
    map: JSON.stringify({
      version: 3,
      file,
      sources: ['../../core/index.ts', '../index.ts'],
      sourcesContent: [coreSource, shimSource],
      names: [...core.names, ...shim.names],
      mappings: encodeMappings(withComment.lines)
    })
  }
}

function composeDeclarations(core, shim, coreSource, shimSource) {
  const separator =
    core.text === '' || core.text.endsWith('\n') || core.text.endsWith('\r')
      ? ''
      : '\n'
  const coreLines =
    core.text === ''
      ? []
      : core.text.endsWith('\n') || core.text.endsWith('\r')
        ? core.lines.slice(0, -1)
        : core.lines
  const text = `${core.text}${separator}${shim.text}`
  const lines = [...coreLines, ...remapLines(shim.lines, 1, core.names.length)]
  const withComment = appendSourceMapComment(text, lines, 'index.d.ts.map')
  return {
    text: withComment.text,
    map: JSON.stringify({
      version: 3,
      file: 'index.d.ts',
      sources: ['../../core/index.ts', '../index.ts'],
      sourcesContent: [coreSource, shimSource],
      names: [...core.names, ...shim.names],
      mappings: encodeMappings(withComment.lines)
    })
  }
}

function buildArtifacts(adapterDir, compiledAdapter) {
  const core = prepareCore()
  let shim
  if (compiledAdapter) {
    shim = prepareCompiledShim(join(adapterDir, 'dist/.compiled'))
  } else {
    const source = readFileSync(join(adapterDir, 'index.ts'), 'utf8')
    const extracted = extractPluginSource(source)
    const declarations = `export declare const plugin: ReturnType<typeof createHarnessPlugin>;\nexport default plugin;\n`
    shim = {
      js: extracted.js,
      jsSource: extracted.jsSource,
      dts: {
        text: declarations,
        lines: identityLines(declarations, extracted.declarationLine),
        names: []
      },
      dtsSource: source
    }
  }

  const js = compose(core.js, shim.js, 'index.js', core.jsSource, shim.jsSource)
  const dts = composeDeclarations(
    core.dts,
    shim.dts,
    core.dtsSource,
    shim.dtsSource
  )
  return {
    'index.js': js.text,
    'index.js.map': js.map,
    'index.d.ts': dts.text,
    'index.d.ts.map': dts.map
  }
}

function bundleAdapter(adapterDir, compiledAdapter = false) {
  const adapterDirResolved = resolve(adapterDir || process.cwd())
  const artifacts = buildArtifacts(adapterDirResolved, compiledAdapter)
  const distDir = join(adapterDirResolved, 'dist')
  mkdirSync(distDir, { recursive: true })
  for (const name of artifactNames) {
    const path = join(distDir, name)
    writeFileSync(path, artifacts[name], 'utf8')
    console.log(`[bundle] wrote ${path}`)
  }
  if (compiledAdapter)
    rmSync(join(distDir, '.compiled'), { recursive: true, force: true })
}

function main() {
  const args = process.argv.slice(2)
  const watchMode = args.includes('--watch') || args.includes('-w')
  const compiledAdapter = args.includes('--compiled-adapter')
  const cwd = resolve(process.cwd())
  if (compiledAdapter) {
    const validCwds = [resolve(rootDir, 'openclaw'), resolve(rootDir, 'hermes')]
    if (!validCwds.includes(cwd)) {
      throw new Error(
        '--compiled-adapter requires cwd plugins/openclaw or plugins/hermes'
      )
    }
    if (watchMode)
      throw new Error('--compiled-adapter cannot be combined with --watch')
  }
  const dirArg = args.find((argument) => !argument.startsWith('-'))
  const adapterDir = compiledAdapter ? cwd : resolve(dirArg || cwd)
  bundleAdapter(adapterDir, compiledAdapter)

  if (watchMode) {
    const coreWatchPath = join(coreDir, 'index.ts')
    const adapterWatchPath = join(adapterDir, 'index.ts')
    const rebuild = () => {
      try {
        execFileSync('tsc', ['--project', join(coreDir, 'tsconfig.json')], {
          stdio: 'inherit'
        })
        bundleAdapter(adapterDir, false)
      } catch (error) {
        console.error(`[bundle watch] error: ${error.message}`)
      }
    }
    watch(coreWatchPath, { persistent: true }, rebuild)
    watch(adapterWatchPath, { persistent: true }, rebuild)
    console.log('[bundle] watching for changes...')
  }
}

main()
