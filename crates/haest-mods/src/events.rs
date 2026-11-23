//! [`ModEvent`]: the one list of the events a mod hooks, by the names a mod
//! file uses. Every `Mod` event has its twin here, and the JavaScript half of
//! a mod file is handed this list when it loads (bootstrap.js refuses to
//! start when the events it handles are not exactly these), so the Rust and
//! the TypeScript events cannot drift apart unnoticed: a mod file that does
//! not load fails every file mod test.

haest_core::word_enum! {
    /// One mod event, as a mod file names it in `haest.on(event, fn)`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum ModEvent ("mod event") {
        /// [`Mod::tool_before`](crate::Mod::tool_before).
        ToolBefore = "tool.before",
        /// [`Mod::tool_after`](crate::Mod::tool_after).
        ToolAfter = "tool.after",
        /// [`Mod::prompt_submit`](crate::Mod::prompt_submit).
        PromptSubmit = "prompt.submit",
        /// [`Mod::system_prompt`](crate::Mod::system_prompt).
        SystemPrompt = "system.prompt",
        /// [`Mod::model_before`](crate::Mod::model_before).
        ModelBefore = "model.before",
        /// [`Mod::model_after`](crate::Mod::model_after).
        ModelAfter = "model.after",
        /// [`Mod::turn_end`](crate::Mod::turn_end).
        TurnEnd = "turn.end",
    }
}
