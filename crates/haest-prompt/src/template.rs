//! [`PromptTemplate`]: a Jinja template compiled once, the host's values,
//! and the chain of resolvers a name goes through.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    sync::{Arc, Mutex},
};

use minijinja::{
    Environment, Error, ErrorKind, UndefinedBehavior,
    syntax::SyntaxConfig,
    value::{Enumerator, Object, Serde, Value},
};
use serde_json::{Map, Value as Json};

use crate::PromptContext;

/// The name the one template is stored under; errors name it.
const NAME: &str = "system prompt";

type Resolver = Arc<dyn Fn(&str, Next<'_>) -> Option<Json> + Send + Sync>;

/// A system prompt template: Jinja, compiled when it is made, rendered for
/// each model call with a [`PromptContext`] and the host's own values.
///
/// The environment is the one Hugging Face chat templates are written
/// for: `trim_blocks` and `lstrip_blocks` on (as `transformers` sets
/// them), `{% break %}` and `{% continue %}`, Python's string methods
/// (`.strip()`, `.startswith()`), `tojson`, and `raise_exception(message)`
/// to fail a render on purpose.
///
/// A name the template reads goes, in order, through:
///
/// 1. each resolver ([`with_resolver`](Self::with_resolver)), in the order
///    they were added, each answering or handing the name on;
/// 2. the properties ([`with_property`](Self::with_property));
/// 3. haest's [`PromptContext`].
///
/// A name nobody answers is undefined: `{% if name %}` is false, while
/// `{{ name }}` fails the render, so a misspelt name is an error and not
/// an empty string. A name is looked up once per render.
///
/// It is cheap to clone: clones share the compiled template.
///
/// # Examples
///
/// ```
/// use haest_core::{catalog::ModelInfo, context::PromptInputs};
/// use haest_prompt::{PromptContext, PromptTemplate};
///
/// # fn main() -> Result<(), haest_prompt::TemplateError> {
/// let template = PromptTemplate::new(
///     r#"
/// {% if model.family == "anthropic" and model.tier == "opus" %}
/// You are {{ name }}. Take the time the work needs.
/// {% else %}
/// You are {{ name }}.
/// {% endif %}
///
/// {{ sections.environment }}
/// "#,
/// )?
/// .with_property("name", "the notebook's reader");
///
/// let inputs = PromptInputs {
///     cwd: "/notes".into(),
///     model_spec: "anthropic:claude-opus-5-5".into(),
///     ..PromptInputs::default()
/// };
/// let opus = ModelInfo::new("anthropic:claude-opus-5-5", 1_000_000)
///     .with_family("anthropic")
///     .with_tier("opus")
///     .with_generation(5.5);
/// let prompt = template.render(&PromptContext::new(&inputs, &[], Some(&opus)))?;
/// assert!(prompt.starts_with("You are the notebook's reader. Take the time"));
/// assert!(prompt.contains("- Working directory: /notes"));
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct PromptTemplate {
    env: Arc<Environment<'static>>,
    properties: BTreeMap<String, Json>,
    resolvers: Vec<Resolver>,
}

impl fmt::Debug for PromptTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PromptTemplate")
            .field("properties", &self.properties)
            .field("resolvers", &self.resolvers.len())
            .finish_non_exhaustive()
    }
}

impl PromptTemplate {
    /// The template `source`, compiled now.
    ///
    /// # Errors
    ///
    /// The syntax error, with its line, when `source` is not a template.
    pub fn new(source: impl Into<String>) -> Result<Self, TemplateError> {
        let mut env = Environment::new();
        let syntax = SyntaxConfig::builder()
            .trim_blocks(true)
            .lstrip_blocks(true)
            .build()?;
        env.set_syntax(syntax);
        env.set_undefined_behavior(UndefinedBehavior::SemiStrict);
        env.set_unknown_method_callback(minijinja_contrib::pycompat::unknown_method_callback);
        env.add_function("raise_exception", raise_exception);
        env.add_template_owned(NAME, source.into())?;
        Ok(Self {
            env: Arc::new(env),
            properties: BTreeMap::new(),
            resolvers: Vec::new(),
        })
    }

    /// The same, with `name` read as `value`, ahead of haest's own names.
    /// Setting a name again replaces it.
    ///
    /// ```
    /// use haest_core::context::PromptInputs;
    /// use haest_prompt::{PromptContext, PromptTemplate};
    /// use serde_json::json;
    ///
    /// # fn main() -> Result<(), haest_prompt::TemplateError> {
    /// let template = PromptTemplate::new("{{ team.name }} has {{ team.size }} people.")?
    ///     .with_property("team", json!({"name": "Docs", "size": 3}));
    /// let context = PromptContext::new(&PromptInputs::default(), &[], None);
    /// assert_eq!(template.render(&context)?, "Docs has 3 people.");
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn with_property(mut self, name: impl Into<String>, value: impl Into<Json>) -> Self {
        self.properties.insert(name.into(), value.into());
        self
    }

    /// The same, with `resolve` asked for every name the template reads,
    /// after the resolvers added before it. It answers `Some(value)`, or
    /// hands the name on with `next.resolve(name)`; None leaves it
    /// undefined.
    ///
    /// It runs while the template renders, and only for the names the
    /// template reads.
    ///
    /// ```
    /// use haest_core::context::PromptInputs;
    /// use haest_prompt::{PromptContext, PromptTemplate};
    ///
    /// # fn main() -> Result<(), haest_prompt::TemplateError> {
    /// let template = PromptTemplate::new("{{ user }} works in {{ cwd }}.")?
    ///     .with_resolver(|name, next| match name {
    ///         "user" => Some("Ada".into()),
    ///         _ => next.resolve(name),
    ///     });
    /// let inputs = PromptInputs { cwd: "/notes".into(), ..PromptInputs::default() };
    /// let context = PromptContext::new(&inputs, &[], None);
    /// assert_eq!(template.render(&context)?, "Ada works in /notes.");
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn with_resolver<F>(mut self, resolve: F) -> Self
    where
        F: Fn(&str, Next<'_>) -> Option<Json> + Send + Sync + 'static,
    {
        self.resolvers.push(Arc::new(resolve));
        self
    }

    /// The prompt for the call `context` describes, without the whitespace
    /// the template leaves at either end.
    ///
    /// # Errors
    ///
    /// What failed the render, with its line: a name printed that nobody
    /// answered, a `raise_exception`, a filter or method given the wrong
    /// value.
    pub fn render(&self, context: &PromptContext) -> Result<String, TemplateError> {
        // A struct of strings, numbers and lists: always a map.
        let Ok(Json::Object(haest)) = serde_json::to_value(context) else {
            return Err(TemplateError {
                message: "haest's prompt context did not serialize to a map".into(),
                line: None,
                source: None,
            });
        };
        let root = Root {
            properties: self.properties.clone(),
            resolvers: self.resolvers.clone(),
            haest,
            seen: Mutex::default(),
        };
        let template = self.env.get_template(NAME)?;
        let text = template.render(Value::from_object(root))?;
        Ok(text.trim().to_string())
    }
}

/// `raise_exception(message)`: fails the render with `message`, as in a
/// Hugging Face chat template.
fn raise_exception(message: String) -> Result<Value, Error> {
    Err(Error::new(ErrorKind::InvalidOperation, message))
}

/// The rest of the resolver chain after one resolver: hand a name on with
/// [`resolve`](Self::resolve).
#[derive(Clone, Copy)]
pub struct Next<'a> {
    rest: &'a [Resolver],
    end: &'a dyn Fn(&str) -> Option<Json>,
}

impl fmt::Debug for Next<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Next")
            .field("resolvers", &self.rest.len())
            .finish_non_exhaustive()
    }
}

impl Next<'_> {
    /// What the resolvers after this one, then the properties, then haest
    /// answer for `name`.
    pub fn resolve(self, name: &str) -> Option<Json> {
        match self.rest.split_first() {
            Some((first, rest)) => first(
                name,
                Next {
                    rest,
                    end: self.end,
                },
            ),
            None => (self.end)(name),
        }
    }
}

/// The names one render reads, answered lazily through the chain.
struct Root {
    properties: BTreeMap<String, Json>,
    resolvers: Vec<Resolver>,
    haest: Map<String, Json>,
    /// Each name's answer, so a resolver runs once per name per render.
    seen: Mutex<HashMap<String, Option<Value>>>,
}

impl fmt::Debug for Root {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Root")
            .field("properties", &self.properties)
            .field("resolvers", &self.resolvers.len())
            .field("haest", &self.haest)
            .finish_non_exhaustive()
    }
}

impl Root {
    fn lookup(&self, name: &str) -> Option<Json> {
        let end = |name: &str| {
            self.properties
                .get(name)
                .or_else(|| self.haest.get(name))
                .cloned()
        };
        Next {
            rest: &self.resolvers,
            end: &end,
        }
        .resolve(name)
    }
}

impl Object for Root {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        self.get_value_by_str(key.as_str()?)
    }

    fn get_value_by_str(self: &Arc<Self>, name: &str) -> Option<Value> {
        // A poisoned memo only costs a second lookup.
        if let Some(found) = self.seen.lock().ok().and_then(|s| s.get(name).cloned()) {
            return found;
        }
        let found = self.lookup(name).map(|json| Value::from(Serde(json)));
        if let Ok(mut seen) = self.seen.lock() {
            seen.insert(name.to_string(), found.clone());
        }
        found
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        let names: BTreeSet<&String> = self.properties.keys().chain(self.haest.keys()).collect();
        Enumerator::Values(names.into_iter().map(|n| Value::from(n.as_str())).collect())
    }
}

/// A template that would not compile, or a render that failed.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct TemplateError {
    message: String,
    line: Option<usize>,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl TemplateError {
    /// The template's line the error points at, when it points at one.
    pub fn line(&self) -> Option<usize> {
        self.line
    }
}

impl From<Error> for TemplateError {
    fn from(e: Error) -> Self {
        Self {
            message: e.to_string(),
            line: e.line(),
            source: Some(Box::new(e)),
        }
    }
}
