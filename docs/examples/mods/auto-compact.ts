// Compact as a turn ends, once the context is past 60% of the model's window,
// so the next turn starts small. Drop it in a mod directory (the user's
// `<config>/mods/` or the project's `.haest/mods/`). Its Rust twin, and a run
// of both against a scripted model: crates/haest/examples/auto_compact.rs.

/** The share of the window past which a finished turn compacts. */
const SHARE = 0.6

/** What the summary must keep, after haest's own summary instruction. */
const KEEP = 'Keep the open tasks and every file path.'

export default function (haest: any): void {
  haest.on('turn.end', (turn: any) => {
    const { contextTokens, contextWindow } = turn
    // `contextWindow` is null when neither the catalog nor the host knows it.
    if (!contextWindow || contextTokens < contextWindow * SHARE) return undefined
    return { compact: { instructions: KEEP } }
  })
}
