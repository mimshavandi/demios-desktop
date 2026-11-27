//! The [`InferenceBackend`] trait (ADR-0003) and the [`LoadedModel`] adapter to
//! [`dn_core::chat::ChatModel`].

use crate::stream::LazyStream;
use crate::{BackendCaps, LoadOptions, ModelHandle, ModelSpec, Result};
use dn_core::chat::{BoxFuture, ChatModel, ChatRequest, EventStream, LimitSource, ModelLimits};
use dn_core::model::ModelRef;
use std::future::Future;
use std::sync::Arc;

/// Streamed answer of a backend: same type as [`ChatModel::chat`] returns.
pub type TokenStream = EventStream;

/// One inference engine (llama.cpp in-process, an OpenAI-compatible server, later mistral.rs).
///
/// Contract:
/// - `chat` returns once the request is accepted; tokens arrive on the stream.
/// - The stream ends with exactly one `StreamEvent::Done` unless it yields an `Err`.
/// - Dropping the stream cancels generation promptly.
/// - Handles are only valid on the backend that issued them; `unload` invalidates them,
///   but streams already running keep the weights alive until they finish.
pub trait InferenceBackend: Send + Sync + 'static {
    /// Short id, e.g. `llamacpp`, `openai_compat`.
    fn name(&self) -> &'static str;

    fn capabilities(&self) -> BackendCaps;

    fn load(
        &self,
        spec: &ModelSpec,
        opts: &LoadOptions,
    ) -> impl Future<Output = Result<ModelHandle>> + Send;

    fn chat(
        &self,
        h: &ModelHandle,
        req: ChatRequest,
    ) -> impl Future<Output = Result<TokenStream>> + Send;

    /// One L2-normalised vector per input.
    fn embed(
        &self,
        h: &ModelHandle,
        input: &[String],
    ) -> impl Future<Output = Result<Vec<Vec<f32>>>> + Send;

    fn unload(&self, h: ModelHandle) -> impl Future<Output = Result<()>> + Send;

    /// The context `h` was loaded with, in tokens (input and output together). `None` when
    /// the backend doesn't know it (a server that doesn't report one).
    fn context_tokens(&self, _h: &ModelHandle) -> Option<u32> {
        None
    }

    /// The exact number of prompt tokens `req` takes on `h`: the same template and
    /// tokenizer generation uses (ADR-0020 §2.1). `None` when the backend can't count.
    fn count_tokens(&self, _h: &ModelHandle, _req: &ChatRequest) -> Option<Result<u32>> {
        None
    }
}

/// A loaded model usable anywhere a [`ChatModel`] is expected (the agent loop).
pub struct LoadedModel<B: InferenceBackend> {
    backend: Arc<B>,
    handle: ModelHandle,
}

impl<B: InferenceBackend> LoadedModel<B> {
    pub fn new(backend: Arc<B>, handle: ModelHandle) -> Self {
        Self { backend, handle }
    }

    /// Load `spec` on `backend` and wrap the handle.
    pub async fn load(backend: Arc<B>, spec: &ModelSpec, opts: &LoadOptions) -> Result<Self> {
        let handle = backend.load(spec, opts).await?;
        Ok(Self { backend, handle })
    }

    pub fn handle(&self) -> &ModelHandle {
        &self.handle
    }

    pub fn backend(&self) -> &Arc<B> {
        &self.backend
    }

    pub async fn embed(&self, input: &[String]) -> Result<Vec<Vec<f32>>> {
        self.backend.embed(&self.handle, input).await
    }

    pub async fn unload(self) -> Result<()> {
        self.backend.unload(self.handle).await
    }
}

impl<B: InferenceBackend> ChatModel for LoadedModel<B> {
    fn chat(&self, req: ChatRequest) -> EventStream {
        let backend = self.backend.clone();
        let handle = self.handle.clone();
        Box::pin(LazyStream::Pending(Box::pin(async move {
            backend.chat(&handle, req).await
        })))
    }

    /// The context this model was loaded with (whatever `model` the caller names: this
    /// value serves one model).
    fn limits<'a>(&'a self, _model: &'a ModelRef) -> BoxFuture<'a, ModelLimits> {
        let limits = match self.backend.context_tokens(&self.handle) {
            Some(n) => ModelLimits::known(n, LimitSource::Loaded),
            None => ModelLimits::default(),
        };
        Box::pin(async move { limits })
    }

    fn count_tokens(
        &self,
        req: &ChatRequest,
    ) -> Option<std::result::Result<u32, dn_core::CoreError>> {
        self.backend
            .count_tokens(&self.handle, req)
            .map(|r| r.map_err(Into::into))
    }
}

/// L2-normalise in place (embeddings are compared with dot products).
pub(crate) fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::next_event;
    use dn_core::chat::{ChatMessage, FinishReason, Role, StreamEvent, Usage};

    /// Backend that echoes the last user message word by word.
    struct Echo;

    impl InferenceBackend for Echo {
        fn name(&self) -> &'static str {
            "echo"
        }
        fn capabilities(&self) -> BackendCaps {
            BackendCaps {
                chat: true,
                ..Default::default()
            }
        }
        async fn load(&self, spec: &ModelSpec, _: &LoadOptions) -> Result<ModelHandle> {
            Ok(ModelHandle::new(1, spec.model_id.clone()))
        }
        async fn chat(&self, h: &ModelHandle, req: ChatRequest) -> Result<TokenStream> {
            if h.id != 1 {
                return Err(crate::InferenceError::UnknownHandle(h.id));
            }
            let text = match &req.messages.last().unwrap().content[0] {
                dn_core::chat::ContentPart::Text { text } => text.clone(),
                _ => String::new(),
            };
            let (tx, rx) = tokio::sync::mpsc::channel(8);
            tokio::spawn(async move {
                for w in text.split(' ') {
                    let _ = tx.send(Ok(StreamEvent::TextDelta { text: w.into() })).await;
                }
                let _ = tx
                    .send(Ok(StreamEvent::Done {
                        finish_reason: FinishReason::Stop,
                        usage: Usage::default(),
                    }))
                    .await;
            });
            Ok(Box::pin(crate::stream::ChannelStream::new(rx)))
        }
        async fn embed(&self, _: &ModelHandle, input: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(input.iter().map(|_| vec![1.0]).collect())
        }
        async fn unload(&self, _: ModelHandle) -> Result<()> {
            Ok(())
        }
    }

    fn req(text: &str) -> ChatRequest {
        ChatRequest {
            model: "local:echo".parse().unwrap(),
            messages: vec![ChatMessage::text(Role::User, text)],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            response_schema: None,
            reasoning: Default::default(),
            caller: "test".into(),
            native_web_search: None,
        }
    }

    #[tokio::test]
    async fn loaded_model_is_a_chat_model() {
        let m = LoadedModel::load(
            Arc::new(Echo),
            &ModelSpec::gguf("echo.gguf"),
            &LoadOptions::default(),
        )
        .await
        .unwrap();
        let cm: &dyn ChatModel = &m;
        let mut s = cm.chat(req("a b c"));
        let mut words = vec![];
        while let Some(ev) = next_event(&mut s).await {
            match ev.unwrap() {
                StreamEvent::TextDelta { text } => words.push(text),
                StreamEvent::Done { finish_reason, .. } => {
                    assert_eq!(finish_reason, FinishReason::Stop)
                }
                _ => {}
            }
        }
        assert_eq!(words, ["a", "b", "c"]);
    }

    #[tokio::test]
    async fn setup_error_becomes_one_err_item() {
        let m = LoadedModel::new(Arc::new(Echo), ModelHandle::new(9, "x"));
        let mut s = m.chat(req("hi"));
        assert!(next_event(&mut s).await.unwrap().is_err());
        assert!(next_event(&mut s).await.is_none());
    }

    #[test]
    fn normalize_unit_length() {
        let mut v = vec![3.0, 4.0];
        normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
    }
}
