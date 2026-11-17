//! [`answer_stream`]: a provider's [`ProviderStream`] from an async block,
//! its `Done` coming from what the block returns.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Stream, StreamExt, channel::mpsc};

use super::{ProviderEvent, ProviderStream};
use crate::messages::AssistantMessage;

/// Where a provider built with [`answer_stream`] sends its deltas as they
/// arrive. A cheap `Clone`, for a task that reads the response body.
///
/// Sending never fails: once the stream is dropped, or after the answer is
/// in, a delta goes nowhere.
#[derive(Debug, Clone)]
pub struct Deltas(mpsc::UnboundedSender<ProviderEvent>);

impl Deltas {
    /// Answer text as it arrives ([`ProviderEvent::TextDelta`]).
    pub fn text(&self, text: impl Into<String>) {
        self.send(ProviderEvent::TextDelta(text.into()));
    }

    /// Reasoning text as it arrives ([`ProviderEvent::ThinkingDelta`]).
    pub fn thinking(&self, text: impl Into<String>) {
        self.send(ProviderEvent::ThinkingDelta(text.into()));
    }

    /// A tool call began ([`ProviderEvent::ToolCallStart`]): the text before
    /// it was narration.
    pub fn tool_call_start(&self, id: impl Into<String>, name: impl Into<String>) {
        self.send(ProviderEvent::ToolCallStart {
            id: id.into(),
            name: name.into(),
        });
    }

    fn send(&self, event: ProviderEvent) {
        // Err: the stream was dropped or has ended; nobody reads the delta.
        let _ = self.0.unbounded_send(event);
    }
}

/// The stream of one model call, written as an async block: `produce` gets
/// the [`Deltas`] to send as the answer arrives and returns the whole
/// answer, which the stream ends with as its one [`ProviderEvent::Done`].
///
/// So a provider keeps [`Provider`](super::Provider)'s contract by
/// construction: deltas in the order sent, then exactly one `Done`, also
/// when the call failed (return the message marked with
/// [`mark_stream_failed`](super::mark_stream_failed)). The async block runs
/// only as the stream is polled, and dropping the stream drops it. It needs
/// no runtime of its own and spawns nothing.
///
/// # Examples
///
/// A [`Provider`](super::Provider) built with it is in that trait's docs.
/// The deltas come first, in the order sent, then the answer:
///
/// ```
/// use futures::StreamExt;
/// use haest_core::{AssistantMessage, AssistantPart, ProviderEvent, answer_stream};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::BoxError> {
/// let mut answer = AssistantMessage::start("example", "model-1");
/// let stream = answer_stream(|deltas| async move {
///     for word in ["hello ", "there"] {
///         deltas.text(word);
///     }
///     answer.content.push(AssistantPart::Text { text: "hello there".into() });
///     answer
/// });
/// let events: Vec<ProviderEvent> = stream.collect().await;
/// assert_eq!(events.len(), 3);
/// assert!(matches!(&events[2], ProviderEvent::Done(a) if a.text() == "hello there"));
/// # Ok(())
/// # }
/// ```
pub fn answer_stream<F, Fut>(produce: F) -> ProviderStream
where
    F: FnOnce(Deltas) -> Fut,
    Fut: Future<Output = AssistantMessage> + Send + 'static,
{
    let (tx, rx) = mpsc::unbounded();
    Box::pin(Answer {
        deltas: rx,
        answer: Some(Box::pin(produce(Deltas(tx)))),
        done: None,
    })
}

/// [`answer_stream`]'s stream: the deltas, then the answer.
struct Answer<Fut> {
    deltas: mpsc::UnboundedReceiver<ProviderEvent>,
    /// The producer, until it has returned.
    answer: Option<Pin<Box<Fut>>>,
    /// What it returned, until the deltas before it are out.
    done: Option<AssistantMessage>,
}

impl<Fut: Future<Output = AssistantMessage>> Stream for Answer<Fut> {
    type Item = ProviderEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<ProviderEvent>> {
        let this = &mut *self;
        // Every delta sent so far goes out before anything else.
        if let Poll::Ready(Some(event)) = this.deltas.poll_next_unpin(cx) {
            return Poll::Ready(Some(event));
        }
        if let Some(answer) = this.answer.as_mut() {
            let Poll::Ready(message) = answer.as_mut().poll(cx) else {
                return Poll::Pending;
            };
            this.answer = None;
            this.done = Some(message);
            // No delta lands after the answer, even from a clone that lives
            // on; the ones sent while it finished are still drained.
            this.deltas.close();
            if let Poll::Ready(Some(event)) = this.deltas.poll_next_unpin(cx) {
                return Poll::Ready(Some(event));
            }
        }
        Poll::Ready(this.done.take().map(ProviderEvent::Done))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use futures::StreamExt;

    use super::*;

    fn answer(text: &str) -> AssistantMessage {
        let mut m = AssistantMessage::start("test", "m");
        m.content
            .push(crate::messages::AssistantPart::Text { text: text.into() });
        m
    }

    #[tokio::test]
    async fn the_deltas_come_in_order_and_the_answer_once_at_the_end() {
        let events: Vec<ProviderEvent> = answer_stream(|deltas| async move {
            deltas.thinking("hm");
            deltas.text("a");
            tokio::task::yield_now().await;
            deltas.tool_call_start("c1", "Read");
            deltas.text("b");
            answer("ab")
        })
        .collect()
        .await;
        assert_eq!(
            events,
            [
                ProviderEvent::ThinkingDelta("hm".into()),
                ProviderEvent::TextDelta("a".into()),
                ProviderEvent::ToolCallStart {
                    id: "c1".into(),
                    name: "Read".into()
                },
                ProviderEvent::TextDelta("b".into()),
                ProviderEvent::Done(answer("ab")),
            ]
        );
    }

    #[tokio::test]
    async fn a_clone_kept_past_the_answer_neither_holds_the_stream_open_nor_adds_to_it() {
        let kept: Arc<Mutex<Option<Deltas>>> = Arc::default();
        let keep = kept.clone();
        let mut stream = answer_stream(move |deltas| async move {
            *keep.lock().unwrap() = Some(deltas.clone());
            answer("done")
        });
        assert_eq!(
            stream.next().await,
            Some(ProviderEvent::Done(answer("done")))
        );
        let late = kept
            .lock()
            .unwrap()
            .clone()
            .expect("the producer kept a clone");
        late.text("too late");
        assert_eq!(stream.next().await, None);
    }

    #[tokio::test]
    async fn nothing_runs_until_the_stream_is_polled() {
        let ran = Arc::new(Mutex::new(false));
        let flag = ran.clone();
        let stream = answer_stream(move |_| async move {
            *flag.lock().unwrap() = true;
            answer("")
        });
        assert!(!*ran.lock().unwrap());
        drop(stream);
        assert!(
            !*ran.lock().unwrap(),
            "dropping it drops the producer unrun"
        );
    }
}
