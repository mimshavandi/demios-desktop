//! The context size the person set for a model nobody else knows (ADR-0020 §2.1, design
//! ContextSize). Only used when the model or its provider doesn't know its own size.

use dn_core::CoreError;
use dn_core::chat::{BoxFuture, ChatModel, ChatRequest, EventStream, LimitSource, ModelLimits};
use dn_core::model::ModelRef;
use std::sync::Arc;

/// Allowed values for a size the person sets.
pub const USER_SIZE: std::ops::RangeInclusive<u32> = 1_024..=10_000_000;

/// `inner`, with `size` (the person's) answering `limits` when `inner` doesn't know.
pub struct WithUserSize {
    pub inner: Arc<dyn ChatModel>,
    pub size: Option<u32>,
}

impl ChatModel for WithUserSize {
    fn chat(&self, req: ChatRequest) -> EventStream {
        self.inner.chat(req)
    }

    fn limits<'a>(&'a self, model: &'a ModelRef) -> BoxFuture<'a, ModelLimits> {
        Box::pin(async move {
            let known = self.inner.limits(model).await;
            match (known.context_tokens, self.size) {
                (Some(_), _) => known,
                (None, Some(n)) => ModelLimits::known(n, LimitSource::User),
                (None, None) => known,
            }
        })
    }

    fn count_tokens(&self, req: &ChatRequest) -> Option<Result<u32, CoreError>> {
        self.inner.count_tokens(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Unknown;
    impl ChatModel for Unknown {
        fn chat(&self, _: ChatRequest) -> EventStream {
            unreachable!()
        }
    }
    struct Known;
    impl ChatModel for Known {
        fn chat(&self, _: ChatRequest) -> EventStream {
            unreachable!()
        }
        fn limits<'a>(&'a self, _: &'a ModelRef) -> BoxFuture<'a, ModelLimits> {
            Box::pin(async { ModelLimits::known(200_000, LimitSource::Published) })
        }
    }

    #[test]
    fn the_persons_size_fills_in_only_what_nobody_knows() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let m: ModelRef = "local:x".parse().unwrap();
        let get =
            |inner: Arc<dyn ChatModel>, size| rt.block_on(WithUserSize { inner, size }.limits(&m));
        assert_eq!(
            get(Arc::new(Unknown), Some(128_000)),
            ModelLimits::known(128_000, LimitSource::User)
        );
        assert_eq!(get(Arc::new(Unknown), None), ModelLimits::default());
        assert_eq!(
            get(Arc::new(Known), Some(8_000)),
            ModelLimits::known(200_000, LimitSource::Published)
        );
    }
}
