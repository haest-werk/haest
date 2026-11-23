//! The events a mod hooks, declared once. Each is an [`Event`]: the payload
//! a hook gets, tied to its name ([`ModEvent`]) and to the decision a hook
//! answers.
//!
//! The table at the bottom has one row per event: its name, its payload, its
//! decision, what a failing hook does by default, and its [`HookSet`] binding
//! (`on_*`). The rows write all of them, so an event cannot come without
//! any. The rest of an event is held from the other side: `impl Mod` for
//! [`HookSet`], for [`Mods`](crate::Mods) and for haest-bridges' `JsMod` name
//! every method (`clippy::missing_trait_methods`), so a `Mod` event without
//! its mod-file twin or its `HookSet` run fails the gates; the JavaScript
//! half of a mod file (bootstrap.js) is handed [`ModEvent::WORDS`] at load
//! and refuses to start unless it handles exactly those; and the parity
//! tests (`testing::decision_cases`, run by both crates)
//! match on every [`ModEvent`], so a new row fails to compile there until
//! it is driven through both.

use std::fmt;

use serde::{Serialize, de::DeserializeOwned};

use crate::{
    api::Before,
    decision::{ModelAfterDecision, ModelBeforeDecision, TurnEndDecision},
    hook::{Binding, Hook, OnError},
    hook_set::HookSet,
    payload::{
        ModelAfterPayload, ModelBeforePayload, PromptSubmitPayload, SessionView,
        SystemPromptPayload, ToolAfterPayload, ToolBeforePayload, TurnEndPayload,
    },
};

/// One event of the mod protocol, as its payload: the value a hook bound to
/// the event gets, tied to the event's name and to what a hook answers.
///
/// The payload and the decision are the JSON a mod file's handler gets and
/// returns (camelCase), so a hook across a process or a network sends this
/// payload and reads this decision as they are, and a Rust hook gets the
/// same values typed. The events are haest's own: the payload types below
/// are the only ones, and no other type can be one.
///
/// | Event | Payload | Decision |
/// |---|---|---|
/// | `tool.before` | [`ToolBeforePayload`] | [`Before`] |
/// | `tool.after` | [`ToolAfterPayload`] | `Option<ToolOutput>` |
/// | `prompt.submit` | [`PromptSubmitPayload`] | `Option<String>` |
/// | `system.prompt` | [`SystemPromptPayload`] | `Option<String>` |
/// | `model.before` | [`ModelBeforePayload`] | [`ModelBeforeDecision`] |
/// | `model.after` | [`ModelAfterPayload`] | [`ModelAfterDecision`] |
/// | `turn.end` | [`TurnEndPayload`] | [`TurnEndDecision`] |
pub trait Event:
    sealed::Sealed + Serialize + DeserializeOwned + Clone + fmt::Debug + Send + Sync + 'static
{
    /// Which event this is.
    const NAME: ModEvent;
    /// What a hook on this event does when it fails or runs out of time,
    /// unless its [`Binding`] says otherwise: [`OnError::Block`] for
    /// `tool.before`, [`OnError::Continue`] for every other event.
    const ON_ERROR: OnError;
    /// What a hook answers. Its [`Default`] passes (changes nothing).
    type Decision: Serialize + DeserializeOwned + Default + fmt::Debug + Send + 'static;

    /// A hook's answer, as JSON, read as this event's decision: the one
    /// decoder every bridge reads an answer with (a mod file's handler, a
    /// shell or HTTP hook), so an answer means the same whatever carries
    /// it. It is the decision's own serde, but for one thing:
    /// `system.prompt` reads `false` as nothing, as JavaScript's
    /// `cond && 'text'` answers when `cond` does not hold.
    ///
    /// ```
    /// use haest_mods::{Event, PromptSubmitPayload, SystemPromptPayload};
    /// use serde_json::json;
    ///
    /// assert_eq!(SystemPromptPayload::read_decision(json!(false)).unwrap(), None);
    /// assert!(SystemPromptPayload::read_decision(json!(42)).is_err());
    /// assert!(PromptSubmitPayload::read_decision(json!(false)).is_err());
    /// ```
    ///
    /// # Errors
    ///
    /// An answer the event does not take (`{ "deny": 42 }`, a number from
    /// `prompt.submit`): the hook failed.
    fn read_decision(answer: serde_json::Value) -> Result<Self::Decision, serde_json::Error> {
        crate::decision::read_answer::<Self>(answer)
    }

    /// The session the conversation is on as of this event, when its host
    /// named one: a [`HookSet`] hands every binding its payload, and a mod
    /// file every handler, with the session the conversation's
    /// [`ModEnv`](crate::ModEnv) names as the event begins
    /// ([`ModEnv::set_session`](crate::ModEnv::set_session)). It is read
    /// once per event, so every handler of the event, in every mod of its
    /// [`Mods`](crate::Mods), gets the same one.
    fn session(&self) -> Option<&SessionView>;
}

/// An event about one tool call (`tool.before`, `tool.after`): a binding
/// on it may ask only about some tools ([`Binding::with_matcher`]).
pub trait ToolEvent: Event {
    /// The name of the tool called.
    fn tool_name(&self) -> &str;
}

/// Seals [`Event`]: the events are haest's.
mod sealed {
    use crate::payload::SessionView;

    #[expect(
        unnameable_types,
        reason = "the seal of Event: public so it can bound Event, unnameable so no other crate implements it"
    )]
    pub trait Sealed {
        /// The payload as of `session`: what the fold (fold.rs) sets before
        /// a [`HookSet`](crate::HookSet) binding or a mod file's handler gets it.
        fn set_session(&mut self, session: Option<SessionView>);
    }
}

/// Writes, from one row per event: [`ModEvent`], each payload's [`Event`]
/// impl, the bindings a [`HookSet`] keeps, and each event's `on_*` method.
macro_rules! mod_events {
    ($(
        $(#[$doc:meta])*
        $variant:ident = $word:literal {
            payload: $payload:ident,
            decision: $decision:ty,
            on_error: $on_error:ident,
            $(#[$bind_doc:meta])*
            bind: $bind:ident,
        }
    )+) => {
        haest_core::word_enum! {
            /// One mod event, as a mod file names it in `haest.on(event, fn)`:
            /// an [`Event`]'s [`NAME`](Event::NAME).
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
            #[non_exhaustive]
            pub enum ModEvent ("mod event") {
                $($(#[$doc])* $variant = $word,)+
            }
        }

        $(
            impl sealed::Sealed for $payload {
                fn set_session(&mut self, session: Option<SessionView>) {
                    self.session = session;
                }
            }

            impl Event for $payload {
                const NAME: ModEvent = ModEvent::$variant;
                const ON_ERROR: OnError = OnError::$on_error;
                type Decision = $decision;

                fn session(&self) -> Option<&SessionView> {
                    self.session.as_ref()
                }
            }

            // One per event, not one generic impl: another crate could
            // make a generic `Binding<E>` a `Hook<E>` of its own event.
            impl<H: Hook<$payload>> From<H> for Binding<$payload> {
                /// `hook`, with its event's defaults ([`Binding::new`]).
                fn from(hook: H) -> Self {
                    Self::new(hook)
                }
            }
        )+

        /// Every event's bindings, in the order added.
        #[derive(Debug, Clone, Default)]
        pub(crate) struct Slots {
            $(pub(crate) $bind: Vec<Binding<$payload>>,)+
        }

        impl HookSet {
            $(
                $(#[$bind_doc])*
                ///
                /// Takes a hook itself (a closure, an `async fn`), or a
                /// [`Binding`] of one with its options. Bindings of one
                /// event run in the order added.
                #[must_use]
                pub fn $bind(mut self, binding: impl Into<Binding<$payload>>) -> Self {
                    self.slots.$bind.push(binding.into());
                    self
                }
            )+
        }
    };
}

mod_events! {
    /// [`Mod::tool_before`](crate::Mod::tool_before).
    ToolBefore = "tool.before" {
        payload: ToolBeforePayload,
        decision: Before,
        on_error: Block,
        /// Bind a hook to `tool.before`: it gets a [`ToolBeforePayload`] and
        /// answers a [`Before`] (refuse the call, or rewrite its input for
        /// the hooks after it and the tool). A failure refuses the call
        /// unless the binding says [`OnError::Continue`].
        bind: on_tool_before,
    }
    /// [`Mod::tool_after`](crate::Mod::tool_after).
    ToolAfter = "tool.after" {
        payload: ToolAfterPayload,
        decision: Option<haest_core::tool::ToolOutput>,
        on_error: Continue,
        /// Bind a hook to `tool.after`: it gets a [`ToolAfterPayload`] and
        /// answers the output that replaces the call's, or `None` to keep it.
        bind: on_tool_after,
    }
    /// [`Mod::prompt_submit`](crate::Mod::prompt_submit).
    PromptSubmit = "prompt.submit" {
        payload: PromptSubmitPayload,
        decision: Option<String>,
        on_error: Continue,
        /// Bind a hook to `prompt.submit`: it gets a [`PromptSubmitPayload`]
        /// and answers the human's message rewritten, or `None` to keep it.
        bind: on_prompt_submit,
    }
    /// [`Mod::system_prompt`](crate::Mod::system_prompt).
    SystemPrompt = "system.prompt" {
        payload: SystemPromptPayload,
        decision: Option<String>,
        on_error: Continue,
        /// Bind a hook to `system.prompt`: before every model call it gets a
        /// [`SystemPromptPayload`] and answers text to append to the system
        /// prompt, or `None` (as JSON, `null` or `false`).
        bind: on_system_prompt,
    }
    /// [`Mod::model_before`](crate::Mod::model_before).
    ModelBefore = "model.before" {
        payload: ModelBeforePayload,
        decision: ModelBeforeDecision,
        on_error: Continue,
        /// Bind a hook to `model.before`: before every model call it gets a
        /// [`ModelBeforePayload`] and answers a [`ModelBeforeDecision`]
        /// (compact, steer, stop the turn).
        bind: on_model_before,
    }
    /// [`Mod::model_after`](crate::Mod::model_after).
    ModelAfter = "model.after" {
        payload: ModelAfterPayload,
        decision: ModelAfterDecision,
        on_error: Continue,
        /// Bind a hook to `model.after`: once an answer lands it gets a
        /// [`ModelAfterPayload`] and answers a [`ModelAfterDecision`] (steer,
        /// stop the turn before the answer's calls run).
        bind: on_model_after,
    }
    /// [`Mod::turn_end`](crate::Mod::turn_end).
    TurnEnd = "turn.end" {
        payload: TurnEndPayload,
        decision: TurnEndDecision,
        on_error: Continue,
        /// Bind a hook to `turn.end`: when a turn is about to end on its own
        /// it gets a [`TurnEndPayload`] and answers a [`TurnEndDecision`]
        /// (keep the turn going, compact).
        bind: on_turn_end,
    }
}
