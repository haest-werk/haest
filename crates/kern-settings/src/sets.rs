//! The settings that pick several things by word: which built-in tools, which
//! instruction files, which skill dirs. Each is a struct of switches, so a host
//! that hardcodes one gets a compile error for a typo; each also answers by
//! word ([`WordSet`]), the spelling a config file and a flag use.

use kern_core::skills::SkillSources;

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

/// One struct of `bool`s per word, and its [`WordSet`].
macro_rules! word_set {
    ($(#[$doc:meta])* $name:ident { $($(#[$fdoc:meta])* $field:ident),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        pub struct $name {
            $($(#[$fdoc])* pub $field: bool,)+
        }

        impl $name {
            pub const NONE: Self = Self { $($field: false,)+ };
            pub const ALL: Self = Self { $($field: true,)+ };
        }

        impl WordSet for $name {
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

word_set! {
    /// The built-in tools (`tools`). The words are kern-tools' own
    /// (`kern_tools::builtin_word`), plus the task list and Skill.
    BuiltinTools {
        read,
        write,
        edit,
        /// Bash, and BashOutput / KillShell where background jobs run.
        bash,
        grep,
        glob,
        web_fetch,
        /// TaskCreate, TaskUpdate, TaskList.
        tasks,
        skill,
    }
}

word_set! {
    /// The instruction files (`context`).
    ContextSources {
        /// AGENTS.md: `~/.agents/AGENTS.md`, then every dir down to the cwd.
        agents,
        /// Claude Code's CLAUDE.md family.
        claude,
    }
}

impl WordSet for SkillSources {
    const WORDS: &'static [&'static str] = &["agents", "claude", "plugins"];

    fn has(&self, word: &str) -> bool {
        match word {
            "agents" => self.agents,
            "claude" => self.claude,
            "plugins" => self.plugins,
            _ => false,
        }
    }

    fn turn_on(&mut self, word: &str) -> bool {
        match word {
            "agents" => self.agents = true,
            "claude" => self.claude = true,
            "plugins" => self.plugins = true,
            _ => return false,
        }
        true
    }
}
