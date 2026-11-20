//! A tool made from another: [`ToolWrapper`] forwards every [`Tool`] method
//! to the wrapped tool unless the wrapper says otherwise, and
//! [`ToolOverride`] changes what the model reads (name, description,
//! parameters) and nothing else.

use async_trait::async_trait;
use serde_json::Value;

use super::{JsonMap, OnResume, Tool, ToolContext, ToolError, ToolOutput, ToolRef};

/// A tool that is another tool with some parts changed. Every method
/// forwards to [`inner`](Self::inner) unless the wrapper implements it, and
/// every `ToolWrapper` is a [`Tool`]: hand it to a [`Tools`](super::Tools)
/// list as any tool.
///
/// One place forwards, so a wrapper that changes only `execute` still keeps
/// the wrapped tool's `read_only`, `on_resume` and `canonical_input`, and a
/// method `Tool` gains later is forwarded too.
///
/// Implementing it takes `use haest_core::tool::ToolWrapper;`. It is not in
/// the prelude: a type that is both a `ToolWrapper` and (through it) a
/// `Tool` has two methods of each name, so where both traits are imported a
/// call on the concrete type names one (`Tool::name(&wrapper)`). A call
/// through a [`ToolRef`] or a `Tools` list is never ambiguous.
///
/// # Examples
///
/// A host that appends a note to every result of a tool:
///
/// ```
/// use async_trait::async_trait;
/// use haest_core::{JsonMap, ToolContext, ToolError, ToolOutput, ToolRef, Tools, tool::ToolWrapper};
/// # use haest_core::{ShutWorkspace, Tool, tool};
/// # use serde_json::{Value, json};
/// # struct Read;
/// # #[async_trait]
/// # impl Tool for Read {
/// #     fn name(&self) -> &str { "Read" }
/// #     fn description(&self) -> String { "Read a file.".into() }
/// #     fn parameters(&self) -> Value { json!({"type": "object"}) }
/// #     fn kind(&self) -> &str { "file.read" }
/// #     fn read_only(&self) -> bool { true }
/// #     async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
/// #         Ok(ToolOutput::text("1 hello"))
/// #     }
/// # }
///
/// struct Noted(ToolRef);
///
/// #[async_trait]
/// impl ToolWrapper for Noted {
///     fn inner(&self) -> &ToolRef {
///         &self.0
///     }
///     async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
///         let mut out = self.0.execute(input, ctx).await?;
///         out.content.push_str("\n(the file may have changed since)");
///         Ok(out)
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::BoxError> {
/// let tools = Tools::new().with(Noted(ToolRef::new(Read)));
/// let read = tools.get("Read").ok_or("no Read")?;
/// assert!(read.read_only(), "forwarded");
/// let ctx = ToolContext::new("/", ShutWorkspace);
/// let out = read.execute(tool::input(json!({}))?, &ctx).await?;
/// assert!(out.content.ends_with("(the file may have changed since)"));
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait ToolWrapper: Send + Sync {
    /// The tool this one wraps.
    fn inner(&self) -> &ToolRef;
    /// [`Tool::name`]; default the wrapped tool's.
    fn name(&self) -> &str {
        self.inner().name()
    }
    /// [`Tool::description`]; default the wrapped tool's.
    fn description(&self) -> String {
        self.inner().description()
    }
    /// [`Tool::prompt_guide`]; default the wrapped tool's.
    fn prompt_guide(&self) -> Option<String> {
        self.inner().prompt_guide()
    }
    /// [`Tool::parameters`]; default the wrapped tool's.
    fn parameters(&self) -> Value {
        self.inner().parameters()
    }
    /// [`Tool::kind`]; default the wrapped tool's.
    fn kind(&self) -> &str {
        self.inner().kind()
    }
    /// [`Tool::read_only`]; default the wrapped tool's.
    fn read_only(&self) -> bool {
        self.inner().read_only()
    }
    /// [`Tool::on_resume`]; default the wrapped tool's.
    fn on_resume(&self) -> OnResume {
        self.inner().on_resume()
    }
    /// [`Tool::canonical_input`]; default the wrapped tool's.
    fn canonical_input(&self, input: &JsonMap) -> JsonMap {
        self.inner().canonical_input(input)
    }
    /// [`Tool::execute`]; default the wrapped tool's.
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        self.inner().execute(input, ctx).await
    }
}

// The one place a wrapper forwards: a method `Tool` gains later fails the
// gates here until `ToolWrapper` forwards it too.
#[async_trait]
#[warn(clippy::missing_trait_methods)]
impl<W: ToolWrapper> Tool for W {
    fn name(&self) -> &str {
        ToolWrapper::name(self)
    }
    fn description(&self) -> String {
        ToolWrapper::description(self)
    }
    fn prompt_guide(&self) -> Option<String> {
        ToolWrapper::prompt_guide(self)
    }
    fn parameters(&self) -> Value {
        ToolWrapper::parameters(self)
    }
    fn kind(&self) -> &str {
        ToolWrapper::kind(self)
    }
    fn read_only(&self) -> bool {
        ToolWrapper::read_only(self)
    }
    fn on_resume(&self) -> OnResume {
        ToolWrapper::on_resume(self)
    }
    fn canonical_input(&self, input: &JsonMap) -> JsonMap {
        ToolWrapper::canonical_input(self, input)
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        ToolWrapper::execute(self, input, ctx).await
    }
}

/// A tool with what the model reads changed: its name, its description, its
/// line in the prompt's tool guide or its parameter schema. Everything else
/// (kind, `read_only`, `on_resume`, what a call does) is the wrapped tool's.
///
/// A renamed tool has no guide line until one is set: the wrapped tool's
/// line names it by its old name.
///
/// Put in a [`Tools`](super::Tools) list under the wrapped tool's name, it
/// replaces that tool in its place.
///
/// ```
/// use haest_core::{Tool, ToolOverride, Tools};
/// # use async_trait::async_trait;
/// # use haest_core::{JsonMap, ToolContext, ToolError, ToolOutput};
/// # use serde_json::{Value, json};
/// # struct Read;
/// # #[async_trait]
/// # impl Tool for Read {
/// #     fn name(&self) -> &str { "Read" }
/// #     fn description(&self) -> String { "Read a file.".into() }
/// #     fn parameters(&self) -> Value { json!({"type": "object"}) }
/// #     fn kind(&self) -> &str { "file.read" }
/// #     fn read_only(&self) -> bool { true }
/// #     async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
/// #         Ok(ToolOutput::text(""))
/// #     }
/// # }
///
/// let mut tools = Tools::new().with(Read);
/// tools.push(ToolOverride::new(Read).with_description("Read a notebook page."));
/// assert_eq!(tools.len(), 1, "it took Read's place");
/// assert_eq!(tools[0].description(), "Read a notebook page.");
/// assert!(tools[0].read_only());
/// ```
#[derive(Debug, Clone)]
pub struct ToolOverride {
    inner: ToolRef,
    name: Option<String>,
    description: Option<String>,
    prompt_guide: Guide,
    parameters: Option<Value>,
}

/// An override's line in the tool guide.
#[derive(Debug, Clone)]
enum Guide {
    /// The wrapped tool's, unless the override renames it.
    Wrapped,
    /// The host's line.
    Line(String),
    /// No line.
    Silent,
}

impl ToolOverride {
    /// `tool` as it is, until a `with_*` changes a part.
    pub fn new(tool: impl Into<ToolRef>) -> Self {
        Self {
            inner: tool.into(),
            name: None,
            description: None,
            prompt_guide: Guide::Wrapped,
            parameters: None,
        }
    }

    /// The same, with `line` in the prompt's tool guide.
    #[must_use]
    pub fn with_prompt_guide(mut self, line: impl Into<String>) -> Self {
        self.prompt_guide = Guide::Line(line.into());
        self
    }

    /// The same, with no line in the prompt's tool guide.
    #[must_use]
    pub fn without_prompt_guide(mut self) -> Self {
        self.prompt_guide = Guide::Silent;
        self
    }

    /// The same, called by `name`.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// The same, described to the model as `description`.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// The same, with this JSON Schema for its input. The wrapped tool still
    /// reads the call: the schema should ask for what it takes.
    #[must_use]
    pub fn with_parameters(mut self, parameters: Value) -> Self {
        self.parameters = Some(parameters);
        self
    }
}

#[async_trait]
impl ToolWrapper for ToolOverride {
    fn inner(&self) -> &ToolRef {
        &self.inner
    }
    fn name(&self) -> &str {
        self.name.as_deref().unwrap_or_else(|| self.inner.name())
    }
    fn description(&self) -> String {
        self.description
            .clone()
            .unwrap_or_else(|| self.inner.description())
    }
    fn prompt_guide(&self) -> Option<String> {
        match &self.prompt_guide {
            Guide::Line(line) => Some(line.clone()),
            // The wrapped tool's line calls it by the name it no longer has.
            Guide::Wrapped if self.name.is_some() => None,
            Guide::Wrapped => self.inner.prompt_guide(),
            Guide::Silent => None,
        }
    }
    fn parameters(&self) -> Value {
        self.parameters
            .clone()
            .unwrap_or_else(|| self.inner.parameters())
    }
}
