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
 * models.json; a TypeScript row with a price wins, whole. So is `cache`, what
 * the model's prompt cache can do (haest-core's `CacheFacts`, written by hand
 * from the providers' prompt-caching pages).
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

/** What a model's prompt cache can do: haest-core's `CacheFacts`. */
interface Cache {
  minCacheableTokens?: number
  ttl1h?: boolean
  prewarmMaxTokens?: number
  toolChanges?: boolean
}

const CACHE_KEYS = new Set(['minCacheableTokens', 'ttl1h', 'prewarmMaxTokens', 'toolChanges'])

interface Row extends Facts {
  spec: string
  label: string
  input: string[]
  contextWindow: number
  reasoning?: { mandatory: boolean; efforts: string[] }
  pricing?: Pricing
  cache?: Cache
}

/** What a run keeps of a row in models.json: haest's facts, its price and its cache facts. */
interface Kept extends Facts {
  pricing?: Pricing
  cache?: Cache
}

interface Group {
  label: string
  models: Row[]
}

const given = <T>(values: (T | undefined)[]): T[] => values.filter((v): v is T => v !== undefined)

/** A price: every amount it has is a number. */
function pricingOk(p: Pricing): boolean {
  return [p.input, p.cachedInput, p.output, ...given([p.cacheWrite, p.cacheWrite1h])].every(n => typeof n === 'number')
}

/** Cache facts: only keys haest knows, whole numbers, true or false. */
function cacheOk(c: Cache): boolean {
  return (
    Object.keys(c).every(k => CACHE_KEYS.has(k)) &&
    given([c.minCacheableTokens, c.prewarmMaxTokens]).every(Number.isInteger) &&
    given([c.ttl1h, c.toolChanges]).every(b => typeof b === 'boolean')
  )
}

/** A label for an optional text fact: absent, or a non-empty string. */
const optionalText = (v: unknown) => v === undefined || (typeof v === 'string' && v !== '')

/** What a row must hold, each with the problem named when it does not. */
const CHECKS: [string | ((row: Row) => string), (row: Row) => boolean][] = [
  ['spec is not provider:model', row => /^[^:]+:.+/.test(row.spec)],
  ['no label', row => Boolean(row.label)],
  [row => `input ${JSON.stringify(row.input)}`, row => Boolean(row.input?.length) && row.input.every(m => MODALITIES.has(m))],
  ['contextWindow', row => Number.isInteger(row.contextWindow) && row.contextWindow > 0],
  ['unknown effort', row => !row.reasoning || row.reasoning.efforts.every(e => EFFORTS.has(e))],
  ['pricing', row => !row.pricing || pricingOk(row.pricing)],
  ['cache', row => !row.cache || cacheOk(row.cache)],
  ['family', row => optionalText(row.family)],
  ['tier', row => optionalText(row.tier)],
  ['generation', row => row.generation === undefined || Number.isFinite(row.generation)],
]

/** Every reason this row would not load as a haest-core `ModelInfo`. */
function problems(row: Row): string[] {
  return CHECKS.filter(([, ok]) => !ok(row)).map(([name]) => (typeof name === 'string' ? name : name(row)))
}

/** The hand-written facts and prices of every row models.json has now, by spec. */
async function keptFacts(path: string): Promise<Map<string, Kept>> {
  const file = Bun.file(path)
  if (!(await file.exists())) return new Map()
  const { groups } = (await file.json()) as { groups: Group[] }
  return new Map(
    groups
      .flatMap(g => g.models)
      .map(r => [
        r.spec,
        { family: r.family, tier: r.tier, generation: r.generation, pricing: r.pricing, cache: r.cache },
      ]),
  )
}

/**
 * `row` with haest's facts after its label, then its price and its cache facts last: the
 * TypeScript row's, else the kept ones.
 */
function withFacts(row: Row, kept: Kept | undefined): Row {
  const { spec, label, family: _f, tier: _t, generation: _g, pricing: _p, cache: _c, ...rest } = row
  const pick = <K extends keyof Kept>(key: K): Kept[K] => row[key] ?? kept?.[key]
  const facts: Facts = { family: pick('family'), tier: pick('tier'), generation: pick('generation') }
  return { spec, label, ...facts, ...rest, pricing: pick('pricing'), cache: pick('cache') }
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
