//! Small async helpers so we need neither `futures` nor `async-stream`.

use dn_core::CoreError;
use dn_core::chat::{EventStream, StreamEvent};
use futures_core::Stream;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc;

/// Boxed `Send` future, so adapter traits stay object-safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Sender half handed to a stream-producing future.
pub(crate) type Emit = mpsc::Sender<Result<StreamEvent, CoreError>>;

/// Turns a producer future into an [`EventStream`]. The future is polled *inside* the
/// stream's `poll_next`, so no task is spawned and dropping the stream cancels the HTTP
/// call (the `ChatModel` contract).
pub(crate) fn event_stream<F, Fut>(producer: F) -> EventStream
where
    F: FnOnce(Emit) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = mpsc::channel(64);
    Box::pin(GenStream {
        fut: Some(Box::pin(producer(tx))),
        rx,
    })
}

struct GenStream {
    fut: Option<BoxFuture<'static, ()>>,
    rx: mpsc::Receiver<Result<StreamEvent, CoreError>>,
}

impl Stream for GenStream {
    type Item = Result<StreamEvent, CoreError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(fut) = self.fut.as_mut()
            && fut.as_mut().poll(cx).is_ready()
        {
            self.fut = None;
        }
        // Once the producer is gone its sender is dropped, so `poll_recv` ends the stream
        // after draining buffered items.
        self.rx.poll_recv(cx)
    }
}

/// `StreamExt::next` without the `futures` crate.
pub async fn next_event(s: &mut EventStream) -> Option<Result<StreamEvent, CoreError>> {
    std::future::poll_fn(|cx| s.as_mut().poll_next(cx)).await
}
