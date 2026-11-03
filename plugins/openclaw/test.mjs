import { describe, it } from 'node:test';
import assert from 'node:assert';
import { resolveConfig, parseGuardValidate, detectTaskIntentsLocal, parseFileMemoryHits, buildSubagentContextHeader, injectSubagentPrompt } from './dist/index.js';

describe('OpenClaw Plugin', () => {
  it('resolveConfig', () => assert.ok(resolveConfig({})));
  it('parseGuardValidate', () => assert.strictEqual(parseGuardValidate('{ "verdict": { "bucket": "Pass" } }').bucket, 'Pass'));
  it('detectTaskIntentsLocal', () => {
    const signals = detectTaskIntentsLocal('TODO: fix this bug');
    assert.ok(signals.length > 0);
  });
  it('parseFileMemoryHits', () => {
    const text = '{ "hits": [ { "id": "42", "valid_from": "2026-01-01", "memory_type": "text", "content": "memory: 42" } ] }';
    const hits = parseFileMemoryHits(text);
    assert.strictEqual(hits.length, 1);
  });
  it('buildSubagentContextHeader', () => {
    const h = buildSubagentContextHeader('NeuroStrata', 'neurostrata-pi52');
    assert.ok(h.includes('Namespace: `NeuroStrata`'));
    assert.ok(h.includes('Mandatory Graph-First Lookup Rule'));
  });
  it('injectSubagentPrompt', () => {
    const t = { prompt: 'hello' };
    assert.strictEqual(injectSubagentPrompt(t, 'NeuroStrata', 'neurostrata-pi52'), true);
    assert.ok(t.prompt.includes('### NeuroStrata Task & Graph Context'));
  });
});