//! What a host hands kern instead of kern reaching for it: named keys
//! ([`KeySource`]) and the environment of the processes the agent starts
//! ([`ChildEnv`]). kern never reads the process environment itself.

use std::{collections::HashMap, fmt, sync::Arc};

/// Named values a provider asks the host for: its key and endpoint overrides
/// (`OPENROUTER_API_KEY`, `ANTHROPIC_BASE_URL`, a providers.json entry's
/// `apiKeyEnv`). The host answers from its environment, a keychain, a vault, or
/// not at all. An empty value counts as none.
pub trait KeySource: Send + Sync {
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

/// OpenRouter's API: kern's OpenRouter provider and GenerateImage both call it.
pub const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1";

/// Where this host's OpenRouter is: its `OPENROUTER_BASE_URL` key, else [`OPENROUTER_URL`].
pub fn openrouter_base_url(keys: &dyn KeySource) -> String {
    keys.key("OPENROUTER_BASE_URL")
        .unwrap_or_else(|| OPENROUTER_URL.into())
}

/// The environment of every process the agent starts: a real shell, its
/// background jobs, an MCP server. A child gets these variables and nothing
/// else -- not the host's own environment, not its keys -- unless the host put
/// them here. Debug output shows the names only.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ChildEnv(Arc<Vec<(String, String)>>);

impl ChildEnv {
    pub fn new(vars: impl IntoIterator<Item = (String, String)>) -> Self {
        Self(Arc::new(vars.into_iter().collect()))
    }

    pub fn vars(&self) -> &[(String, String)] {
        &self.0
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
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
