//! One hub's job table: the jobs in start order, each with its rolling output.

use std::time::Instant;

use tokio_util::sync::CancellationToken;

use super::{Job, JobStatus, MAX_BUFFER, MAX_FINISHED, Taken};

pub(super) struct Entry {
    pub job: Job,
    /// Everything kept so far.
    pub output: String,
    /// How much of the whole stream the model has already been given (bytes).
    read: usize,
    /// Bytes dropped off the front of `output` (the buffer cap).
    dropped: usize,
    pub stop: CancellationToken,
}

impl Entry {
    pub(super) fn append(&mut self, chunk: &str) {
        self.output.push_str(chunk);
        if self.output.len() <= MAX_BUFFER {
            return;
        }
        let mut cut = self.output.len() - MAX_BUFFER;
        while !self.output.is_char_boundary(cut) {
            cut += 1;
        }
        self.output.drain(..cut);
        self.dropped += cut;
    }

    pub(super) fn take(&mut self) -> Taken {
        // Both offsets are ends of whole chunks, so `from` is a character boundary.
        let from = self.read.saturating_sub(self.dropped);
        let missed = self.dropped.saturating_sub(self.read);
        let fresh = self.output.get(from..).unwrap_or_default().to_string();
        self.read = self.dropped + self.output.len();
        Taken {
            job: self.job.clone(),
            fresh,
            missed,
        }
    }

    fn ended(&self) -> bool {
        self.job.exit_code.is_some()
    }
}

#[derive(Default)]
pub(super) struct Jobs {
    /// Start order, so the first finished ones are the oldest.
    entries: Vec<Entry>,
    next: u64,
}

impl Jobs {
    pub(super) fn running(&self) -> Vec<Job> {
        self.entries
            .iter()
            .filter(|e| e.job.status == JobStatus::Running)
            .map(|e| e.job.clone())
            .collect()
    }

    pub(super) fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.job.id == id)
    }

    pub(super) fn get_mut(&mut self, id: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.job.id == id)
    }

    pub(super) fn add(&mut self, command: &str, description: &str, stop: CancellationToken) -> Job {
        self.next += 1;
        let job = Job {
            id: format!("b{}", self.next),
            command: command.into(),
            description: description.into(),
            status: JobStatus::Running,
            exit_code: None,
            started: Instant::now(),
        };
        self.entries.push(Entry {
            job: job.clone(),
            output: String::new(),
            read: 0,
            dropped: 0,
            stop,
        });
        job
    }

    /// Keep the last [`MAX_FINISHED`] that ENDED (a killed job still dying is kept:
    /// its end has yet to be reported).
    pub(super) fn forget_old_finished(&mut self) {
        let ended = self.entries.iter().filter(|e| e.ended()).count();
        let mut excess = ended.saturating_sub(MAX_FINISHED);
        self.entries.retain(|e| {
            let forget = excess > 0 && e.ended();
            if forget {
                excess -= 1;
            }
            !forget
        });
    }
}
