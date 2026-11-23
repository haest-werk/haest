//! [`ModReload`]: whether a conversation's mod files reload as they change.

haest_core::word_enum! {
    /// Whether a conversation's `.js` / `.ts` mod files reload while it runs
    /// (the `mod_reload` setting). haest-bridges' `ModFiles` reads them again
    /// before each model call, through the host's files, and acts on what
    /// changed: a new or changed file loads, a deleted one unloads, a file
    /// that fails to load keeps its previous version and is reported. Native
    /// Rust mods never reload.
    ///
    /// ```
    /// use haest_settings::ModReload;
    ///
    /// assert_eq!(ModReload::default(), ModReload::Off);
    /// assert_eq!("ask".parse(), Ok(ModReload::Ask));
    /// assert_eq!(ModReload::WORDS, ["off", "ask", "auto"]);
    /// ```
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    #[non_exhaustive]
    pub enum ModReload ("mod reload") {
        /// The files load once, when the conversation starts.
        #[default]
        Off = "off",
        /// A change waits for the human's Allow, a deleted file's unload
        /// too: on Deny the version that runs stays, and the same content
        /// (or the same deletion) is not asked about again.
        Ask = "ask",
        /// A change loads without asking.
        Auto = "auto",
    }
}
