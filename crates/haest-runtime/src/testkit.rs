//! For tests: bare settings in `/w`, and a host whose [`Ui`] keeps every warning.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use haest_settings::Settings;

use crate::{
    RuntimeHooks, Ui,
    haest::{
        core::fs::MemFs,
        core::session::JsonlSessionFile,
        mods::{ApprovalAnswer, ApprovalRequest, Prompter},
    },
};

pub fn settings() -> Settings {
    Settings::bare("/w", "/h")
}

/// Keeps what the runtime warned about.
#[derive(Default)]
pub struct Recorded(Mutex<Vec<String>>);

impl Recorded {
    pub fn warnings(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl Ui for Recorded {
    fn info(&self, _: &str) {}
    fn warn(&self, text: &str) {
        self.0.lock().unwrap().push(text.into());
    }
    fn error(&self, text: &str) {
        self.warn(text);
    }
}

/// A host on these settings; its session file is in memory. Needs a tokio runtime.
pub fn hooks(settings: Settings) -> (Arc<RuntimeHooks>, Arc<Recorded>) {
    let ui = Arc::new(Recorded::default());
    let store = JsonlSessionFile::new(Arc::new(MemFs::new()), Path::new("/d"), "t").unwrap();
    let hooks = RuntimeHooks::new(settings, ui.clone(), Arc::new(store));
    (Arc::new(hooks), ui)
}

/// Says no to every ask.
pub struct Deny;

#[async_trait]
impl Prompter for Deny {
    async fn prompt(&self, _: &ApprovalRequest) -> ApprovalAnswer {
        ApprovalAnswer::Deny
    }
}
