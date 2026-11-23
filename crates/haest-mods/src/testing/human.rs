//! A human at the gate's prompt, for this crate's own tests.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;

use super::held;
use crate::{
    ask::ApprovalRequest,
    gate::{ApprovalAnswer, Prompter},
};

/// A human at the gate's prompt: answers in order (out of answers = never
/// answers), counting the prompts and the asks an earlier ALWAYS answered.
#[derive(Debug, Default)]
pub(crate) struct Human {
    answers: Mutex<VecDeque<ApprovalAnswer>>,
    prompts: AtomicUsize,
    receipts: AtomicUsize,
}

impl Human {
    /// A human who gives these answers, in order.
    pub(crate) fn answering(answers: impl IntoIterator<Item = ApprovalAnswer>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into_iter().collect()),
            ..Default::default()
        })
    }

    /// How many prompts they saw.
    pub(crate) fn prompts(&self) -> usize {
        self.prompts.load(Ordering::SeqCst)
    }

    /// How many asks an earlier ALWAYS answered for them.
    pub(crate) fn receipts(&self) -> usize {
        self.receipts.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Prompter for Human {
    async fn prompt(&self, _req: &ApprovalRequest) -> ApprovalAnswer {
        self.prompts.fetch_add(1, Ordering::SeqCst);
        let next = held(&self.answers).pop_front();
        match next {
            Some(a) => a,
            None => std::future::pending().await,
        }
    }
    fn auto_approved(&self, _req: &ApprovalRequest) {
        self.receipts.fetch_add(1, Ordering::SeqCst);
    }
}
