//! The settings that pick several things by word: which built-in tools, which
//! instruction files, which skill dirs. Each is a struct of switches, so a host
//! that hardcodes one gets a compile error for a typo; each also answers by
//! word ([`WordSet`](kern_core::words::WordSet)), the spelling a config file
//! and a flag use. The instruction files and skill dirs are kern-core's own
//! ([`ContextSources`](kern_core::context::ContextSources),
//! [`SkillSources`](kern_core::skills::SkillSources)).

kern_core::word_set! {
    /// The built-in tools (`tools`), by kern-tools' words (`kern_tools::builtin_word`).
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
