//! The settings that pick several things by word: which built-in tools, which
//! instruction files, which skill dirs. Each is a struct of switches, so a host
//! that hardcodes one gets a compile error for a typo; each also answers by
//! word ([`WordSet`](haest_core::words::WordSet)), the spelling a config file
//! and a flag use. The instruction files and skill dirs are haest-core's own
//! ([`ContextSources`](haest_core::context::ContextSources),
//! [`SkillSources`](haest_core::skills::SkillSources)).

haest_core::word_set! {
    /// Which of haest's built-in tools the model gets, one switch per tool.
    ///
    /// Each field is also a word ([`WordSet`](haest_core::words::WordSet)): the
    /// field's name is how a config file, a `HAEST_TOOLS` list and `--tools`
    /// spell it. `NONE` has every switch off, `ALL` every one on. haest-tools
    /// maps a tool's name to its word (`haest_tools::builtin_word`) and builds
    /// the tools whose word is on (`haest_tools::builtin_tools_where`).
    ///
    /// ```
    /// use haest_settings::{BuiltinTools, WordSet};
    ///
    /// let tools = BuiltinTools { read: true, bash: true, ..BuiltinTools::NONE };
    /// assert!(tools.has("bash") && !tools.has("write"));
    /// assert_eq!(BuiltinTools::ALL.words(), BuiltinTools::WORDS);
    /// ```
    pub BuiltinTools {
        /// Read: a file's contents.
        read,
        /// Write: create or replace a file.
        write,
        /// Edit: replace a string in a file.
        edit,
        /// Bash, and BashOutput / KillShell where background jobs run.
        bash,
        /// Grep: search file contents (ripgrep).
        grep,
        /// Glob: find files by name pattern.
        glob,
        /// WebFetch: one public web page into the context, as markdown.
        web_fetch,
        /// TaskCreate, TaskUpdate, TaskList and TaskGet: the conversation's task board.
        tasks,
        /// Skill: load one skill's full `SKILL.md`, by name.
        skill,
    }
}
