//! Values written as words, declared once. [`word_enum!`](crate::word_enum) is
//! one value out of several (`effort = "high"`), [`word_set!`](crate::word_set)
//! is several switches on at once (`tools = ["read", "bash"]`). Each declares
//! its words where the type is, so a config file, a flag, the session file and
//! the code all go by the same list, and a new word is one line.
//!
//! haest's own word types are built this way: [`Effort`](crate::Effort),
//! [`WorkspaceMode`](crate::WorkspaceMode),
//! [`MediaKind`](crate::messages::MediaKind) and the other enums, and
//! [`SkillSources`](crate::skills::SkillSources) and
//! [`ContextSources`](crate::context::ContextSources) as sets. `haest-config`
//! and `haest-settings` declare theirs with the same macros; a host may too.
//! Both macros are exported at the crate root.

/// A word a word type does not know: what a [`word_enum!`](crate::word_enum)'s
/// `FromStr` and serde, and [`WordSet::from_words`], refuse.
///
/// Its text names the setting, the word and the words there are:
/// `unknown effort "huge" (off, low, medium, high, xhigh, max)`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {what} \"{word}\" ({})", expected.join(", "))]
pub struct UnknownWord {
    /// What the words name (`"effort"`, `"tool"`): the type's label.
    pub what: &'static str,
    /// The word, as written.
    pub word: String,
    /// Every word the type knows, in its order.
    pub expected: &'static [&'static str],
}

/// A set of switches named by words, in one fixed order.
///
/// [`word_set!`](crate::word_set) implements it; read a list setting with
/// [`WordSet::from_words`] and write it back with [`WordSet::words`].
pub trait WordSet: Sized + Default {
    /// What one word names (`"tool"`), for [`UnknownWord`].
    const WHAT: &'static str;
    /// Every word, in the order a list setting writes them.
    const WORDS: &'static [&'static str];

    /// Is `word` on? An unknown word is off.
    fn has(&self, word: &str) -> bool;

    /// Turn `word` on; false for a word this set does not know.
    fn turn_on(&mut self, word: &str) -> bool;

    /// The words that are on, in [`Self::WORDS`] order.
    fn words(&self) -> Vec<&'static str> {
        Self::WORDS
            .iter()
            .copied()
            .filter(|w| self.has(w))
            .collect()
    }

    /// True when no word is on.
    fn is_empty(&self) -> bool {
        self.words().is_empty()
    }

    /// The set with these words on (every other one off).
    ///
    /// # Errors
    ///
    /// [`UnknownWord`] for the first word the set does not know.
    fn from_words<'a>(words: impl IntoIterator<Item = &'a str>) -> Result<Self, UnknownWord> {
        let mut set = Self::default();
        for w in words {
            if !set.turn_on(w) {
                return Err(UnknownWord {
                    what: Self::WHAT,
                    word: w.to_string(),
                    expected: Self::WORDS,
                });
            }
        }
        Ok(set)
    }
}

/// Declares a struct with one `pub bool` field per word, the constants `NONE`
/// and `ALL`, and its [`WordSet`](crate::words::WordSet).
///
/// The field name is the word; `$what` (the literal in parentheses) is what
/// one word names, for the [`UnknownWord`](crate::words::UnknownWord) error.
/// The struct derives `Debug`, `Clone`, `Copy`, `Default` (all off),
/// `PartialEq` and `Eq` itself, so derive none of them.
/// Doc comments on the struct and on each field are kept.
///
/// # Examples
///
/// ```
/// use haest_core::{word_set, words::WordSet};
///
/// word_set! {
///     /// Which rooms the lights are on in.
///     pub Rooms ("room") {
///         /// The kitchen.
///         kitchen,
///         /// The hall.
///         hall,
///     }
/// }
///
/// let on = Rooms::from_words(["hall"]).unwrap();
/// assert!(on.hall && !on.kitchen);
/// assert_eq!(on.words(), ["hall"]);
/// assert_eq!(Rooms::WORDS, ["kitchen", "hall"]);
/// let err = Rooms::from_words(["attic"]).unwrap_err();
/// assert_eq!((err.word.as_str(), err.what), ("attic", "room"));
/// assert!(Rooms::NONE.is_empty());
/// assert_eq!(Rooms::ALL.words(), ["kitchen", "hall"]);
/// ```
#[macro_export]
macro_rules! word_set {
    ($(#[$doc:meta])* $vis:vis $name:ident ($what:literal) { $($(#[$fdoc:meta])* $field:ident),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        $vis struct $name {
            $($(#[$fdoc])* pub $field: bool,)+
        }

        impl $name {
            /// Every word off.
            pub const NONE: Self = Self { $($field: false,)+ };
            /// Every word on.
            pub const ALL: Self = Self { $($field: true,)+ };
        }

        impl $crate::words::WordSet for $name {
            const WHAT: &'static str = $what;
            const WORDS: &'static [&'static str] = &[$(stringify!($field)),+];

            fn has(&self, word: &str) -> bool {
                match word {
                    $(stringify!($field) => self.$field,)+
                    _ => false,
                }
            }

            fn turn_on(&mut self, word: &str) -> bool {
                match word {
                    $(stringify!($field) => self.$field = true,)+
                    _ => return false,
                }
                true
            }
        }
    };
}

/// Declares an enum whose values are words, with `ALL`, `WORDS`, `as_str`,
/// `Display`, `FromStr` and serde, all from one list: a config file, a flag
/// and the session file write the same word.
///
/// `$what` (the literal in parentheses) names the setting in `FromStr`'s
/// error, an [`UnknownWord`](crate::words::UnknownWord):
/// `unknown effort "huge" (off, low, ...)`. Serde reads and writes the bare
/// word, and a wrong word fails with that same message.
///
/// The enum must derive `Clone` and `Copy` (`as_str` takes it by value). Do
/// not derive `Serialize` or `Deserialize`: the macro implements both, through
/// haest-core's own serde, so the declaring crate needs no serde dependency.
/// Other attributes and doc comments, on the enum and on each variant, are kept.
///
/// # Examples
///
/// ```
/// use haest_core::word_enum;
///
/// word_enum! {
///     /// How loud a note is.
///     #[derive(Debug, Clone, Copy, PartialEq, Eq)]
///     pub enum Volume ("volume") {
///         /// Barely there.
///         Quiet = "quiet",
///         /// Heard across the room.
///         Loud = "loud",
///     }
/// }
///
/// assert_eq!(Volume::Loud.as_str(), "loud");
/// assert_eq!(Volume::Quiet.to_string(), "quiet");
/// assert_eq!("loud".parse::<Volume>(), Ok(Volume::Loud));
/// assert_eq!(Volume::WORDS, ["quiet", "loud"]);
/// let err = "shouty".parse::<Volume>().unwrap_err();
/// assert_eq!((err.word.as_str(), err.expected), ("shouty", Volume::WORDS));
/// assert_eq!(serde_json::to_string(&Volume::Loud).unwrap(), "\"loud\"");
/// ```
#[macro_export]
macro_rules! word_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident ($what:literal) {
            $($(#[$vmeta:meta])* $variant:ident = $word:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis enum $name {
            $($(#[$vmeta])* $variant,)+
        }

        impl $name {
            /// Every value, in the order declared.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];
            /// Every value's word, in the order of [`Self::ALL`].
            pub const WORDS: &'static [&'static str] = &[$($word),+];

            /// The word this value is written as.
            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $word,)+
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::words::UnknownWord;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::ALL.iter().copied().find(|v| v.as_str() == s).ok_or_else(|| {
                    $crate::words::UnknownWord { what: $what, word: s.to_string(), expected: Self::WORDS }
                })
            }
        }

        impl $crate::__private::serde::Serialize for $name {
            fn serialize<S: $crate::__private::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> $crate::__private::serde::Deserialize<'de> for $name {
            fn deserialize<D: $crate::__private::serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let word = <String as $crate::__private::serde::Deserialize>::deserialize(d)?;
                word.parse().map_err(<D::Error as $crate::__private::serde::de::Error>::custom)
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use crate::{
        catalog::{InputModality, OpenRouterEffort},
        compaction::SummaryPath,
        deferred_tools::DeferMode,
        messages::{MediaKind, StopReason},
        provider::{Effort, WebSearchEngine},
        skills::SkillSource,
        task_board::TaskStatus,
        workspace::{ApplyFailureCode, ChangeKind, WorkspaceMode},
    };

    /// What the session file writes is the word a config reads, and back.
    fn round_trips<T>(all: &[T], word: fn(&T) -> &'static str)
    where
        T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        for v in all {
            let json = serde_json::to_value(v).unwrap();
            assert_eq!(json, serde_json::Value::from(word(v)));
            assert_eq!(&serde_json::from_value::<T>(json).unwrap(), v);
        }
    }

    #[test]
    fn every_word_enum_writes_and_reads_its_own_words() {
        round_trips(Effort::ALL, |v| v.as_str());
        round_trips(WebSearchEngine::ALL, |v| v.as_str());
        round_trips(WorkspaceMode::ALL, |v| v.as_str());
        round_trips(DeferMode::ALL, |v| v.as_str());
        round_trips(TaskStatus::ALL, |v| v.as_str());
        round_trips(MediaKind::ALL, |v| v.as_str());
        round_trips(StopReason::ALL, |v| v.as_str());
        round_trips(InputModality::ALL, |v| v.as_str());
        round_trips(OpenRouterEffort::ALL, |v| v.as_str());
        round_trips(SummaryPath::ALL, |v| v.as_str());
        round_trips(ChangeKind::ALL, |v| v.as_str());
        round_trips(ApplyFailureCode::ALL, |v| v.as_str());
        round_trips(SkillSource::ALL, |v| v.as_str());
        let err = serde_json::from_str::<Effort>("\"huge\"").unwrap_err();
        assert!(err.to_string().contains("unknown effort"), "{err}");
        assert!("eager".parse::<DeferMode>().is_err());
    }
}
