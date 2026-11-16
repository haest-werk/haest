#!/usr/bin/env bun
/**
 * Generates haest's bundled model catalog, crates/haest-providers/catalog/models.json,
 * from the TypeScript catalog module (the file that exports HAEST_MODEL_GROUPS and
 * HAEST_DEFAULT_MODEL). The JSON is committed; the crate loads it with include_str!.
 *
 *   bun scripts/gen-catalog.ts <path/to/haest-models.ts>
 *
 * The rows are copied, never translated: same keys, same order, same values, so a
 * catalog refresh on the TypeScript side is one run of this script.
 */

import { join, resolve } from 'node:path'

const MODALITIES = new Set(['text', 'image', 'file', 'audio', 'video'])
const EFFORTS = new Set(['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'])

interface Row {
  spec: string
  label: string
  input: string[]
  contextWindow: number
  reasoning?: { mandatory: boolean; efforts: string[] }
  pricing?: { input: number; cachedInput: number; output: number; cacheWrite?: number }
}

interface Group {
  label: string
  models: Row[]
}

/** Every reason this row would not load as a haest-core `ModelInfo`. */
function problems(row: Row): string[] {
  const out: string[] = []
  if (!/^[^:]+:.+/.test(row.spec)) out.push('spec is not provider:model')
  if (!row.label) out.push('no label')
  if (!row.input?.length || row.input.some(m => !MODALITIES.has(m))) out.push(`input ${JSON.stringify(row.input)}`)
  if (!Number.isInteger(row.contextWindow) || row.contextWindow <= 0) out.push('contextWindow')
  if (row.reasoning && row.reasoning.efforts.some(e => !EFFORTS.has(e))) out.push('unknown effort')
  const p = row.pricing
  if (p && ![p.input, p.cachedInput, p.output].every(n => typeof n === 'number')) out.push('pricing')
  return out
}

const source = process.argv[2]
if (!source) {
  console.error('usage: bun scripts/gen-catalog.ts <path/to/haest-models.ts>')
  process.exit(2)
}

const mod = (await import(resolve(source))) as { HAEST_MODEL_GROUPS?: Group[]; HAEST_DEFAULT_MODEL?: string }
const groups = mod.HAEST_MODEL_GROUPS
const defaultModel = mod.HAEST_DEFAULT_MODEL
if (!Array.isArray(groups) || typeof defaultModel !== 'string') {
  console.error(`${source} does not export HAEST_MODEL_GROUPS and HAEST_DEFAULT_MODEL`)
  process.exit(1)
}

const rows = groups.flatMap(g => g.models)
const bad = rows.flatMap(r => problems(r).map(p => `${r.spec}: ${p}`))
const specs = rows.map(r => r.spec)
const dupes = specs.filter((s, i) => specs.indexOf(s) !== i)
bad.push(...dupes.map(s => `${s}: listed twice`))
if (!specs.includes(defaultModel)) bad.push(`default model ${defaultModel} is not in the catalog`)
if (bad.length) {
  console.error(`refusing to write a catalog haest cannot load:\n  ${bad.join('\n  ')}`)
  process.exit(1)
}

const out = join(import.meta.dir, '..', 'crates', 'haest-providers', 'catalog', 'models.json')
await Bun.write(out, `${JSON.stringify({ defaultModel, groups }, null, 2)}\n`)
console.log(`wrote ${rows.length} models in ${groups.length} groups to ${out}`)
