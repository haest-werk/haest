//! What haest hands a template: [`PromptContext`].

use haest_core::{
    InputModality, ToolRef,
    catalog::ModelInfo,
    context::{PromptInputs, PromptSections},
    provider::parse_model_spec,
};
use serde::Serialize;

/// Everything haest knows about one model call, as a template reads it: the
/// model, the tools, haest's built sections and the environment.
///
/// | Name | What |
/// |---|---|
/// | `model.spec` | `provider:model`, as the call names it |
/// | `model.provider`, `model.id` | the spec's two halves |
/// | `model.label`, `model.family`, `model.tier`, `model.generation`, `model.context_window`, `model.input` | the catalog's row ([`ModelInfo`]); `none` when the catalog does not know the model |
/// | `tools` | the tools the call offers, in order: `name`, `guide` (`none` without a line) |
/// | `tool_names` | their names |
/// | `sections` | haest's built prompt, section by section ([`PromptSections`]) |
/// | `cwd`, `platform`, `date`, `git_branch` | the environment; `git_branch` is `none` outside a repository |
#[derive(Debug, Clone, Serialize)]
pub struct PromptContext {
    model: Model,
    tools: Vec<Tool>,
    tool_names: Vec<String>,
    sections: PromptSections,
    cwd: String,
    platform: String,
    date: String,
    git_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct Model {
    spec: String,
    provider: Option<String>,
    id: Option<String>,
    label: Option<String>,
    family: Option<String>,
    tier: Option<String>,
    generation: Option<f64>,
    context_window: Option<u64>,
    input: Option<Vec<InputModality>>,
}

#[derive(Debug, Clone, Serialize)]
struct Tool {
    name: String,
    guide: Option<String>,
}

impl PromptContext {
    /// The context of a call `inputs` describe, offering `tools`, on the model
    /// `model` is the catalog row of (None when the catalog does not know it).
    ///
    /// `inputs` gives the sections, the environment and the spec; its
    /// `tool_guide` and `tool_names` should be the ones `tools` give, as
    /// haest-runtime builds them.
    pub fn new(inputs: &PromptInputs, tools: &[ToolRef], model: Option<&ModelInfo>) -> Self {
        let spec = inputs.model_spec.clone();
        let (provider, id) = match parse_model_spec(&spec) {
            Ok((provider, id)) => (Some(provider.to_string()), Some(id.to_string())),
            Err(_) => (None, None),
        };
        let model = Model {
            provider,
            id,
            label: model.map(|m| m.label.clone()),
            family: model.and_then(|m| m.family.clone()),
            tier: model.and_then(|m| m.tier.clone()),
            generation: model.and_then(|m| m.generation),
            context_window: model.map(|m| m.context_window),
            input: model.map(|m| m.input.clone()),
            spec,
        };
        Self {
            model,
            tools: tools
                .iter()
                .map(|t| Tool {
                    name: t.name().to_string(),
                    guide: t.prompt_guide(),
                })
                .collect(),
            tool_names: tools.iter().map(|t| t.name().to_string()).collect(),
            sections: PromptSections::new(inputs),
            cwd: inputs.cwd.clone(),
            platform: inputs.platform.clone(),
            date: inputs.date.clone(),
            git_branch: inputs.git_branch.clone(),
        }
    }

    /// haest's built sections: what the prompt says without a template.
    pub fn sections(&self) -> &PromptSections {
        &self.sections
    }
}
