//! For tests: settings (the bare defaults plus these flags, in `/w`, no files),
//! and a host whose [`Ui`] keeps every warning.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use kern_settings::{Inputs, Settings, settle};

use crate::{
    Host, Ui,
    kern::{
        fs::MemFs,
        mods::{Answer, ApprovalRequest, Prompter},
        session::JsonlSessionFile,
    },
};

pub fn settings(flags: &[(&str, &str)]) -> Settings {
    let flags: Vec<(String, String)> = flags
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let inputs = Inputs {
        cwd: Path::new("/w"),
        home: Path::new("/h"),
        env: &HashMap::new(),
        flags: &flags,
        user_file: None,
        project_file: None,
    };
    settle(&inputs).unwrap().settings
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

/// A host on `settings(flags)`; its session file is in memory. Needs a tokio runtime.
pub fn host(flags: &[(&str, &str)]) -> (Arc<Host>, Arc<Recorded>) {
    let ui = Arc::new(Recorded::default());
    let store = JsonlSessionFile::new(Arc::new(MemFs::new()), Path::new("/d"), "t").unwrap();
    let host = Host::new(settings(flags), ui.clone(), Arc::new(store));
    (Arc::new(host), ui)
}

/// Says no to every ask.
pub struct Deny;

#[async_trait]
impl Prompter for Deny {
    async fn prompt(&self, _: &ApprovalRequest) -> Answer {
        Answer::Deny
    }
}
