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
 *
 * Three keys are haest's own and the TypeScript rows don't carry them: `family`,
 * `tier` and `generation` (what a prompt template switches on). They are written
 * by hand in models.json and kept by spec across a run; a TypeScript row that has
 * one wins. The run names every row that has no `family`, so a new model gets one.
 *
 * `pricing` is kept the same way: a row the TypeScript side has no price for
 * (the direct `anthropic:` and `openai:` rows, priced by hand from the
 * providers' pricing pages, `cacheWrite1h` included) keeps the one in
 * models.json; a TypeScript row with a price wins, whole.
 */

import { join, resolve } from 'node:path'

const MODALITIES = new Set(['text', 'image', 'file', 'audio', 'video'])
const EFFORTS = new Set(['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'])

/** USD per 1M tokens: haest-core's `ModelPricing`. */
interface Pricing {
  input: number
  cachedInput: number
  output: number
  cacheWrite?: number
  cacheWrite1h?: number
}

/** haest's own keys: written by hand in models.json, kept across a run. */
interface Facts {
  family?: string
  tier?: string
  generation?: number
}

interface Row extends Facts {
  spec: string
  label: string
  input: string[]
  contextWindow: number
  reasoning?: { mandatory: boolean; efforts: string[] }
  pricing?: Pricing
}

/** What a run keeps of a row in models.json: haest's facts, and its price. */
interface Kept extends Facts {
  pricing?: Pricing
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
  const optional = p ? [p.cacheWrite, p.cacheWrite1h].filter(n => n !== undefined) : []
  if (p && ![p.input, p.cachedInput, p.output, ...optional].every(n => typeof n === 'number')) out.push('pricing')
  for (const key of ['family', 'tier'] as const) {
    if (row[key] !== undefined && (typeof row[key] !== 'string' || !row[key])) out.push(key)
  }
  if (row.generation !== undefined && !Number.isFinite(row.generation)) out.push('generation')
  return out
}

/** The hand-written facts and prices of every row models.json has now, by spec. */
async function keptFacts(path: string): Promise<Map<string, Kept>> {
  const file = Bun.file(path)
  if (!(await file.exists())) return new Map()
  const { groups } = (await file.json()) as { groups: Group[] }
  return new Map(
    groups
      .flatMap(g => g.models)
      .map(r => [r.spec, { family: r.family, tier: r.tier, generation: r.generation, pricing: r.pricing }]),
  )
}

/** `row` with haest's facts after its label and its price last: the TypeScript row's, else the kept ones. */
function withFacts(row: Row, kept: Kept | undefined): Row {
  const { spec, label, family, tier, generation, pricing, ...rest } = row
  const facts: Facts = {
    family: family ?? kept?.family,
    tier: tier ?? kept?.tier,
    generation: generation ?? kept?.generation,
  }
  return { spec, label, ...facts, ...rest, pricing: pricing ?? kept?.pricing }
}

const source = process.argv[2]
if (!source) {
  console.error('usage: bun scripts/gen-catalog.ts <path/to/haest-models.ts>')
  process.exit(2)
}

const mod = (await import(resolve(source))) as { HAEST_MODEL_GROUPS?: Group[]; HAEST_DEFAULT_MODEL?: string }
const defaultModel = mod.HAEST_DEFAULT_MODEL
if (!Array.isArray(mod.HAEST_MODEL_GROUPS) || typeof defaultModel !== 'string') {
  console.error(`${source} does not export HAEST_MODEL_GROUPS and HAEST_DEFAULT_MODEL`)
  process.exit(1)
}

const out = join(import.meta.dir, '..', 'crates', 'haest-providers', 'catalog', 'models.json')
const kept = await keptFacts(out)
const groups: Group[] = mod.HAEST_MODEL_GROUPS.map(g => ({
  label: g.label,
  models: g.models.map(r => withFacts(r, kept.get(r.spec))),
}))

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

await Bun.write(out, `${JSON.stringify({ defaultModel, groups }, null, 2)}\n`)
console.log(`wrote ${rows.length} models in ${groups.length} groups to ${out}`)
const unnamed = rows.filter(r => r.family === undefined).map(r => r.spec)
if (unnamed.length) {
  console.log(`no family, tier or generation yet (write them in models.json):\n  ${unnamed.join('\n  ')}`)
}
