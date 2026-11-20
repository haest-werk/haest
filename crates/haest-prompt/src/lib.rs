//! haest's system prompt as a Jinja template, for a host that wants to
//! write its own. Opt-in: without it haest builds its prompt in code
//! ([`haest_core::context::build_system_prompt`]), and nothing here is
//! compiled.
//!
//! The template language is Jinja, rendered by [MiniJinja] and set up the
//! way Hugging Face chat templates (`chat_template.jinja`) expect, so a
//! prompt reads like the templates model cards already ship. The template
//! sees haest's own prompt section by section (`sections.tools`,
//! `sections.environment`, ...), and reorders, wraps or drops them without
//! copying their text. It sees the model's catalog row too (`model.family`,
//! `model.tier`, `model.generation`), the tools with their guide lines,
//! and the host's own values ([`PromptTemplate::with_property`],
//! [`PromptTemplate::with_resolver`]). [`PromptContext`] lists every name.
//!
//! ```
//! use haest_core::{catalog::ModelInfo, context::PromptInputs};
//! use haest_prompt::{PromptContext, PromptTemplate};
//!
//! # fn main() -> Result<(), haest_prompt::TemplateError> {
//! let template = PromptTemplate::new(
//!     r#"
//! {{ sections.identity }}
//! {% if model.family == "anthropic" and model.generation >= 5 %}
//! Think before each tool call.
//! {% endif %}
//!
//! {{ sections.environment }}
//! "#,
//! )?;
//!
//! let inputs = PromptInputs {
//!     model_spec: "anthropic:claude-sonnet-5-5".into(),
//!     ..PromptInputs::default()
//! };
//! let sonnet = ModelInfo::new("anthropic:claude-sonnet-5-5", 1_000_000)
//!     .with_family("anthropic")
//!     .with_generation(5.5);
//! let prompt = template.render(&PromptContext::new(&inputs, &[], Some(&sonnet)))?;
//! assert!(prompt.starts_with("You are haest"));
//! assert!(prompt.contains("Think before each tool call."));
//! # Ok(())
//! # }
//! ```
//!
//! haest-runtime renders a template before every model call with
//! `RuntimeInputs::with_prompt_template` (feature `prompt`), and falls back
//! to the built-in prompt, with a warning, when a render fails.
//!
//! # Limitations
//!
//! - One template: no `{% include %}`, `{% extends %}` or `{% import %}` of
//!   another file.
//! - No `strftime_now`, the clock function Hugging Face's renderers add: the
//!   date is in `date`.
//! - A template is code the host trusts. A loop that never ends hangs the
//!   render; haest sets no fuel limit.
//! - `model.generation` is a number, so `5.10` reads as `5.1`.
//!
//! [MiniJinja]: https://docs.rs/minijinja

mod context;
mod template;
#[cfg(test)]
mod tests;

pub use context::PromptContext;
pub use template::{Next, PromptTemplate, TemplateError};
