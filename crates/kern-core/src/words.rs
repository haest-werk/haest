//! Values written as words, declared once. [`word_enum!`] is one value out of
//! several (`effort = "high"`), [`word_set!`] is several switches on at once
//! (`tools = ["read", "bash"]`). Each declares its words where the type is, so
//! a config file, a flag and the code all go by the same list, and a new word
//! is one line.

/// A set of switches named by words, in one fixed order.
pub trait WordSet: Sized + Default {
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

    fn is_empty(&self) -> bool {
        self.words().is_empty()
    }

    /// The set with these words on, or the first word it does not know.
    fn from_words<'a>(words: impl IntoIterator<Item = &'a str>) -> Result<Self, String> {
        let mut set = Self::default();
        for w in words {
            if !set.turn_on(w) {
                return Err(w.to_string());
            }
        }
        Ok(set)
    }
}

/// One struct of `bool`s per word, with `NONE`, `ALL` and its [`WordSet`].
#[macro_export]
macro_rules! word_set {
    ($(#[$doc:meta])* $vis:vis $name:ident { $($(#[$fdoc:meta])* $field:ident),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        $vis struct $name {
            $($(#[$fdoc])* pub $field: bool,)+
        }

        impl $name {
            pub const NONE: Self = Self { $($field: false,)+ };
            pub const ALL: Self = Self { $($field: true,)+ };
        }

        impl $crate::words::WordSet for $name {
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

/// An enum whose values are words: `ALL`, `WORDS`, `as_str`, `Display` and
/// `FromStr` (whose error names `$what` and the words) from one list.
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
            pub const ALL: &'static [$name] = &[$($name::$variant),+];
            pub const WORDS: &'static [&'static str] = &[$($word),+];

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
            type Err = String;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::ALL.iter().copied().find(|v| v.as_str() == s).ok_or_else(|| {
                    format!("unknown {} \"{s}\" ({})", $what, Self::WORDS.join(", "))
                })
            }
        }
    };
}
