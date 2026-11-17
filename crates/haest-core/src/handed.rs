//! What a host hands haest instead of haest reaching for it: named keys
//! ([`KeySource`]) and the environment of the processes the agent starts
//! ([`ChildEnv`]). haest never reads the process environment itself.
//!
//! The provider registry (`haest-providers`) asks a [`KeySource`]; the
//! workspaces (`haest-workspace`) and MCP servers (`haest-mcp`) start their
//! processes with a [`ChildEnv`].

use std::{collections::HashMap, fmt, sync::Arc};

/// Named values a provider asks the host for: its key and endpoint overrides
/// (`OPENROUTER_API_KEY`, `ANTHROPIC_BASE_URL`, a providers.json entry's
/// `apiKeyEnv`). The host answers from its environment, a keychain, a vault, or
/// not at all. An empty value counts as none.
///
/// A closure `Fn(&str) -> Option<String>` is one, and so is a
/// `HashMap<String, String>`; both answer an empty value as None.
///
/// ```
/// use std::collections::HashMap;
///
/// use haest_core::KeySource;
///
/// let keys = HashMap::from([("OPENROUTER_API_KEY".to_string(), "sk-test".to_string())]);
/// assert_eq!(keys.key("OPENROUTER_API_KEY").as_deref(), Some("sk-test"));
///
/// // A host that reads its own environment says so itself; haest never does.
/// let from_env = |name: &str| std::env::var(name).ok();
/// assert_eq!(from_env.key("HAEST_DOC_EXAMPLE_UNSET"), None);
/// ```
pub trait KeySource: Send + Sync {
    /// The value named `name`, or None. Implementations answer an empty
    /// value as None.
    fn key(&self, name: &str) -> Option<String>;
}

impl<F: Fn(&str) -> Option<String> + Send + Sync> KeySource for F {
    fn key(&self, name: &str) -> Option<String> {
        self(name).filter(|v| !v.is_empty())
    }
}

impl KeySource for HashMap<String, String> {
    fn key(&self, name: &str) -> Option<String> {
        self.get(name).filter(|v| !v.is_empty()).cloned()
    }
}

crate::plug_ref! {
    /// A [`KeySource`], shared: what `haest-providers`' registry asks. Built
    /// from the source itself (a closure, a map), or from an `Arc` of one.
    pub struct KeySourceRef(dyn KeySource);
}

/// The environment of every process the agent starts: a real shell, its
/// background jobs, an MCP server. A child gets these variables and nothing
/// else -- not the host's own environment, not its keys -- unless the host put
/// them here. Debug output shows the names only.
///
/// Cheap to clone (shared). The default is empty: a child with no variables.
///
/// ```
/// use haest_core::ChildEnv;
///
/// let base = ChildEnv::new([("PATH".to_string(), "/usr/bin:/bin".to_string())]);
/// let job = base.with(&ChildEnv::new([("TOKEN".to_string(), "secret".to_string())]));
/// assert_eq!(job.get("PATH"), Some("/usr/bin:/bin"));
/// assert!(!format!("{job:?}").contains("secret"));
/// ```
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ChildEnv(Arc<Vec<(String, String)>>);

impl ChildEnv {
    /// These variables, in order.
    pub fn new(vars: impl IntoIterator<Item = (String, String)>) -> Self {
        Self(Arc::new(vars.into_iter().collect()))
    }

    /// Every variable, in order, values included.
    pub fn vars(&self) -> &[(String, String)] {
        &self.0
    }

    /// The value of `name`; the last one when it is listed twice.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// These variables with `over`'s on top: a name in both has `over`'s value.
    pub fn with(&self, over: &ChildEnv) -> ChildEnv {
        if over.0.is_empty() {
            return self.clone();
        }
        let kept = self.0.iter().filter(|(k, _)| over.get(k).is_none());
        kept.chain(over.0.iter()).cloned().collect()
    }
}

impl FromIterator<(String, String)> for ChildEnv {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Self {
        Self::new(iter)
    }
}

impl fmt::Debug for ChildEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ChildEnv")
            .field(&self.0.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_what_the_host_answers_and_empty_is_none() {
        let keys = |name: &str| (name == "K").then(|| "v".to_string());
        assert_eq!(keys.key("K").as_deref(), Some("v"));
        assert_eq!(keys.key("OTHER"), None);
        let map = HashMap::from([("A".to_string(), String::new())]);
        assert_eq!(map.key("A"), None);
    }

    #[test]
    fn a_child_env_with_another_on_top_takes_its_values_and_keeps_one_of_each_name() {
        let pairs = |p: &[(&str, &str)]| -> ChildEnv {
            p.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let base = pairs(&[("PATH", "/bin"), ("HOME", "/h")]);
        let both = base.with(&pairs(&[("HOME", "/other"), ("GH_TOKEN", "t")]));
        assert_eq!(
            both.vars(),
            pairs(&[("PATH", "/bin"), ("HOME", "/other"), ("GH_TOKEN", "t")]).vars()
        );
        assert_eq!(base.with(&ChildEnv::default()), base);
    }

    #[test]
    fn a_child_env_never_prints_a_value() {
        let env = ChildEnv::new([("TOKEN".to_string(), "secret".to_string())]);
        assert_eq!(env.get("TOKEN"), Some("secret"));
        let shown = format!("{env:?}");
        assert!(
            shown.contains("TOKEN") && !shown.contains("secret"),
            "{shown}"
        );
    }
}
