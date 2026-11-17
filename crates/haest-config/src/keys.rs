//! [`Keys`]: the environment a loader was given, as the source of the
//! provider keys.

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
};

use haest_core::KeySource;

/// The variables [`Loader::env`](crate::Loader::env) was given, as the source
/// of the provider keys: [`Loaded::keys`](crate::Loaded::keys).
///
/// A [`KeySource`], so it goes into the provider registry or haest-runtime's
/// `RuntimeInputs::with_keys` as it is. An empty value counts as none, as
/// every key source answers. Its Debug output shows the names only: a value
/// is a key.
///
/// ```
/// use haest_config::Keys;
/// use haest_core::KeySource;
///
/// let keys: Keys = [("OPENROUTER_API_KEY", "sk-or-example"), ("EMPTY", "")].into_iter().collect();
/// assert_eq!(keys.get("OPENROUTER_API_KEY"), Some("sk-or-example"));
/// assert_eq!(keys.key("EMPTY"), None);
/// assert_eq!(format!("{keys:?}"), r#"Keys(["EMPTY", "OPENROUTER_API_KEY"])"#);
/// ```
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Keys(BTreeMap<String, String>);

impl Keys {
    /// No variables: what a loader with no [`env`](crate::Loader::env) layer gives.
    pub fn new() -> Self {
        Self::default()
    }

    /// The value of `name`; None when it is not there or empty.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .get(name)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// The value of `name`, owned: [`KeySource::key`] without the import.
    pub fn key(&self, name: &str) -> Option<String> {
        self.get(name).map(str::to_string)
    }

    /// Every variable's name, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// Every variable, `(name, value)`, in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// How many variables there are, empty ones included.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl KeySource for Keys {
    fn key(&self, name: &str) -> Option<String> {
        Keys::key(self, name)
    }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for Keys {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self(
            iter.into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }
}

impl From<HashMap<String, String>> for Keys {
    fn from(vars: HashMap<String, String>) -> Self {
        Self(vars.into_iter().collect())
    }
}

/// The names only: a value is a key.
impl fmt::Debug for Keys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Keys")
            .field(&self.names().collect::<Vec<_>>())
            .finish()
    }
}
