//! [`NewJob`]: what [`JobHub::start`](super::JobHub::start) starts.

/// A background job to start: its command line and what it is for.
///
/// The command comes first and alone ([`NewJob::new`]), the description by
/// name ([`with_description`](Self::with_description)), so one cannot be
/// passed for the other.
///
/// ```
/// use haest_tools::NewJob;
///
/// let job = NewJob::new("cargo build").with_description("build");
/// assert_eq!((job.command(), job.description()), ("cargo build", "build"));
/// assert_eq!(NewJob::new("make").description(), "make");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewJob {
    command: String,
    description: Option<String>,
}

impl NewJob {
    /// `command`, run through the workspace's shell; described as itself.
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            description: None,
        }
    }

    /// The same job described as `description` (Bash passes the call's own).
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// The command line.
    pub fn command(&self) -> &str {
        &self.command
    }

    /// What it is for: its description, else its command.
    pub fn description(&self) -> &str {
        self.description.as_deref().unwrap_or(&self.command)
    }

    /// Its command and its description, moved out.
    pub(super) fn into_parts(self) -> (String, String) {
        let description = self.description.unwrap_or_else(|| self.command.clone());
        (self.command, description)
    }
}
