//! The wire's words ([`Access`](crate::Access), [`Level`](crate::Level) and
//! the rest): each enum's words declared once, and [`UnknownWord`] for one
//! it does not know.

/// A word a wire word type does not know: what its `FromStr` and its serde
/// refuse.
///
/// Its text names the type, the word and the words there are:
/// `unknown access "admin" (watch, command)`.
///
/// ```
/// use haest_wire::Access;
///
/// let err = "admin".parse::<Access>().unwrap_err();
/// assert_eq!(err.to_string(), r#"unknown access "admin" (watch, command)"#);
/// assert_eq!("command".parse::<Access>(), Ok(Access::Command));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {what} \"{word}\" ({})", expected.join(", "))]
#[non_exhaustive]
pub struct UnknownWord {
    /// What the words name (`"access"`, `"level"`).
    pub what: &'static str,
    /// The word, as written.
    pub word: String,
    /// Every word the type knows, in its order.
    pub expected: &'static [&'static str],
}

/// Declares an enum whose values are words: the enum, `as_str`, `Display`,
/// `FromStr` and serde (the bare word), all from one list, so the JSON, the
/// text and the parser cannot drift apart. `$what` names the type in
/// [`UnknownWord`]. The enum must derive `Clone` and `Copy`, and not serde.
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
            /// Every word, in the order declared.
            const WORDS: &'static [&'static str] = &[$($word),+];

            /// The word on the wire, as `Display` writes it and `FromStr` reads it.
            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $word,)+
                }
            }
        }

        /// The word; a width pads it (`{:7}`).
        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.pad(self.as_str())
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::UnknownWord;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($word => Ok($name::$variant),)+
                    _ => Err($crate::UnknownWord {
                        what: $what,
                        word: s.to_string(),
                        expected: Self::WORDS,
                    }),
                }
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let word = <String as ::serde::Deserialize>::deserialize(d)?;
                word.parse().map_err(<D::Error as ::serde::de::Error>::custom)
            }
        }
    };
}

pub(crate) use word_enum;
