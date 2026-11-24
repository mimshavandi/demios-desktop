//! Stream plumbing: channel-backed event streams and the lazy adapter that turns the async
//! `InferenceBackend::chat` into the synchronous `ChatModel::chat`.

use dn_core::CoreError;
use dn_core::chat::{EventStream, StreamEvent};
use futures_core::Stream;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc;

pub(crate) type Item = Result<StreamEvent, CoreError>;

/// Receiving end of a producer (thread or task). Dropping it closes the channel, which the
/// producer observes on its next send and stops: that is how stream drop cancels a call.
pub(crate) struct ChannelStream {
    rx: mpsc::Receiver<Item>,
    _guard: Option<AbortOnDrop>,
}

impl ChannelStream {
    pub(crate) fn new(rx: mpsc::Receiver<Item>) -> Self {
        Self { rx, _guard: None }
    }

    pub(crate) fn with_task(rx: mpsc::Receiver<Item>, task: tokio::task::JoinHandle<()>) -> Self {
        Self {
            rx,
            _guard: Some(AbortOnDrop(task)),
        }
    }
}

impl Stream for ChannelStream {
    type Item = Item;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Item>> {
        self.rx.poll_recv(cx)
    }
}

/// Aborts a tokio task when dropped (cancels in-flight HTTP reads immediately).
pub(crate) struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

type ChatFuture = Pin<Box<dyn Future<Output = crate::Result<EventStream>> + Send>>;

/// First awaits the backend's `chat` future, then yields its events. A setup error becomes
/// one `Err` item, after which the stream ends.
pub(crate) enum LazyStream {
    Pending(ChatFuture),
    Streaming(EventStream),
    Done,
}

impl Stream for LazyStream {
    type Item = Item;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Item>> {
        loop {
            match &mut *self {
                Self::Pending(fut) => match fut.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(s)) => *self = Self::Streaming(s),
                    Poll::Ready(Err(e)) => {
                        *self = Self::Done;
                        return Poll::Ready(Some(Err(e.into())));
                    }
                },
                Self::Streaming(s) => return s.as_mut().poll_next(cx),
                Self::Done => return Poll::Ready(None),
            }
        }
    }
}

/// Minimal `StreamExt::next` so callers (CLI, tests) don't need `futures-util`.
pub async fn next_event(s: &mut EventStream) -> Option<Item> {
    std::future::poll_fn(|cx| s.as_mut().poll_next(cx)).await
}
