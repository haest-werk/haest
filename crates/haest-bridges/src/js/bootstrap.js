// The JavaScript half of a file mod: the `haest` API its default export
// receives, and the dispatcher the Rust half (thread.rs) drives. Everything
// crosses the boundary as JSON strings; Rust holds no JavaScript value between
// calls. Each mod has its own runtime, so nothing here is shared between mods.
//
// The events are the Rust side's list (events.rs), handed in at load: `ops`
// handles exactly those, or the mod does not start. An event op runs ONE
// handler and returns its answer as it gave it: the Rust half reads it as the
// event's decision (haest-mods' one decoder), folds it with the answers
// before it, and asks the next handler with what they left. Nothing here
// reads an answer.
;(() => {
  const { parse, stringify } = JSON
  const P = Promise
  const freeze = Object.freeze
  // The ops that are not an event.
  const CALLS = ['load', 'command', 'execute']
  let EVENTS = []
  const hooks = Object.create(null)
  const tools = new Map()
  const commands = new Map()
  const asks = new Map()
  let outbox = []
  let settled = []
  let askSeq = 0

  const isObject = v => v !== null && typeof v === 'object'
  const errorText = e => {
    try {
      return isObject(e) && 'message' in e ? String(e.message) : String(e)
    } catch {
      return 'an error that cannot be shown'
    }
  }
  const plain = (v, what) => {
    try {
      return parse(stringify(v))
    } catch (e) {
      throw new Error(`${what} must be plain JSON: ${errorText(e)}`)
    }
  }
  // prompt.submit and system.prompt answer at once, as they do in every haest.
  const now = (v, event) => {
    if (isObject(v) && typeof v.then === 'function')
      throw new Error(`${event} handlers return their value, not a promise`)
    return v
  }

  const api = env =>
    freeze({
      cwd: env.cwd,
      workspace: env.workspace,
      on(event, fn) {
        const list = hooks[event]
        if (!list) throw new Error(`unknown mod event "${event}"; known: ${EVENTS.join(', ')}`)
        if (typeof fn !== 'function') throw new Error(`haest.on("${event}", fn): fn is not a function`)
        list.push(fn)
        outbox.push({ type: 'on', event })
      },
      tool(t) {
        if (!isObject(t) || typeof t.name !== 'string' || !t.name)
          throw new Error('haest.tool(t): t.name must be a non-empty string')
        if (typeof t.execute !== 'function') throw new Error(`haest.tool("${t.name}"): execute is not a function`)
        const meta = {
          name: t.name,
          description: String(t.description ?? ''),
          parameters: plain(t.parameters ?? { type: 'object', properties: {} }, `haest.tool("${t.name}").parameters`),
          kind: String(t.kind ?? 'agent.unknown'),
          readOnly: !!t.readOnly,
        }
        tools.set(t.name, t)
        outbox.push({ type: 'tool', tool: meta })
      },
      // Only a tool this mod added; true when there was one.
      removeTool(name) {
        const bare = String(name)
        if (!tools.delete(bare)) return false
        outbox.push({ type: 'removeTool', name: bare })
        return true
      },
      // Any tool, whoever added it, until enableTool.
      disableTool(name) {
        outbox.push({ type: 'disableTool', name: String(name) })
      },
      enableTool(name) {
        outbox.push({ type: 'enableTool', name: String(name) })
      },
      command(name, run) {
        if (typeof run !== 'function') throw new Error(`haest.command("${name}", run): run is not a function`)
        const bare = String(name).replace(/^\//, '')
        commands.set(bare, run)
        outbox.push({ type: 'command', name: bare })
      },
      // `signal` is the handler's ctx.signal: the ask ends (deny) when that call is interrupted.
      ask(req, signal) {
        return new P((resolve, reject) => {
          const copy = plain(req, 'haest.ask(req)')
          const id = ++askSeq
          asks.set(id, { resolve, reject })
          const sig = isObject(signal) && typeof signal.id === 'number' ? signal.id : null
          outbox.push({ type: 'ask', id, req: copy, signal: sig })
        })
      },
      notice(text) {
        outbox.push({ type: 'notice', text: String(text) })
      },
    })

  // The op of `event`: its handler `nth` (from 1) on `payload`, handed it as
  // `args` spreads it, and the answer as `answer` lets it through.
  const handler =
    (event, args, answer = v => v) =>
    ({ nth, payload }) => {
      const fn = hooks[event][nth - 1]
      if (!fn) throw new Error(`${event} has no handler ${nth}`)
      return answer(fn(...args(payload)), event)
    }

  const ops = {
    async load({ env, events }) {
      const handled = Object.keys(ops).filter(op => !CALLS.includes(op))
      const missing = events.filter(e => !handled.includes(e))
      const extra = handled.filter(op => !events.includes(op))
      if (missing.length || extra.length)
        throw new Error(`the mod runtime is out of step with haest: no handler for [${missing}], not an event: [${extra}]`)
      EVENTS = events
      for (const e of events) hooks[e] = []
      const main = globalThis.__haestMod
      delete globalThis.__haestMod
      await main(api(env))
    },
    'tool.before': handler('tool.before', ({ call, ctx }) => [call, ctx]),
    'tool.after': handler('tool.after', ({ call, output, ctx }) => [call, output, ctx]),
    'prompt.submit': handler('prompt.submit', ({ text }) => [text], now),
    'system.prompt': handler('system.prompt', () => [], now),
    'model.before': handler('model.before', view => [view]),
    'model.after': handler('model.after', view => [view]),
    'turn.end': handler('turn.end', view => [view]),
    async command({ name, args }) {
      const run = commands.get(name)
      if (!run) throw new Error(`this mod has no command "${name}"`)
      const v = await run(args)
      return v === undefined || v === null ? null : String(v)
    },
    async execute({ name, input, ctx }) {
      const t = tools.get(name)
      if (!t) throw new Error(`this mod has no tool "${name}"`)
      const out = await t.execute(input, ctx)
      return typeof out === 'string' ? { content: out } : out
    },
  }

  globalThis.__haest = freeze({
    // Runs as a job, never inside this call: a handler that throws or loops is
    // caught (or interrupted) where the Rust half can see it.
    dispatch(id, op, json) {
      P.resolve()
        .then(() => {
          const args = parse(json)
          // An event's `{ nth, payload }`, or a tool's `{ name, input, ctx }`.
          const { ctx } = isObject(args.payload) ? args.payload : args
          if (isObject(ctx)) ctx.signal = freeze({ id })
          return ops[op](args)
        })
        .then(v => stringify(v ?? null) ?? 'null')
        .then(
          out => settled.push([id, true, out]),
          e => settled.push([id, false, errorText(e)]),
        )
    },
    answer(id, ok, value) {
      const a = asks.get(id)
      if (!a) return
      asks.delete(id)
      if (ok) a.resolve(value)
      else a.reject(new Error(value))
    },
    drain() {
      const out = stringify({ settled, outbox })
      settled = []
      outbox = []
      return out
    },
  })
})()
