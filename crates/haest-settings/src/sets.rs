//! The settings that pick several things by word: which built-in tools, which
//! instruction files, which skill dirs. Each is a struct of switches, so a host
//! that hardcodes one gets a compile error for a typo; each also answers by
//! word ([`WordSet`](haest_core::words::WordSet)), the spelling a config file
//! and a flag use. The instruction files and skill dirs are haest-core's own
//! ([`ContextSources`](haest_core::context::ContextSources),
//! [`SkillSources`](haest_core::skills::SkillSources)).

haest_core::word_set! {
    /// The built-in tools (`tools`), by haest-tools' words (`haest_tools::builtin_word`).
    pub BuiltinTools {
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
