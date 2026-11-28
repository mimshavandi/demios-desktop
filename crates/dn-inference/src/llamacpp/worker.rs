//! One thread per loaded model. It owns the model's chat context for as long as the model is
//! loaded, so a call can reuse what the last calls left in the KV cache ([`super::slots`]).
//!
//! - **Jobs run one at a time,** in the order they arrive. The backend's semaphore still
//!   decides who may send one, so `Busy` works as before.
//! - **The context is made on the first job** and kept until the model is unloaded. Unloading
//!   drops the job sender, which ends the thread.
//! - **With prompt reuse off,** every call clears the cache first, so the work is what it was
//!   with a fresh context per call.
//! - **An error or panic during a job** drops the context and every saved sequence, since
//!   their contents are no longer known. The next job starts clean.

use super::draft;
use super::slots::{self, SlotCache, Source};
use super::{BenchRun, Loaded, context, piece, prompt_tokens, sampler, send, utf8::Utf8Buf};
use crate::prompt;
use crate::stream::Item;
use crate::tags::TagParser;
use crate::{InferenceError, Result};
use dn_core::chat::{ChatRequest, FinishReason, StreamEvent, Usage};
use llama_cpp_2::LlamaStateSeqFlags;
use llama_cpp_2::SeqState;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{OwnedSemaphorePermit, mpsc, oneshot};

pub(super) enum Job {
    Chat {
        req: Box<ChatRequest>,
        thinking: bool,
        tx: mpsc::Sender<Item>,
        permit: OwnedSemaphorePermit,
    },
    Bench {
        n_prompt: u32,
        n_gen: u32,
        reply: oneshot::Sender<Result<BenchRun>>,
        permit: OwnedSemaphorePermit,
    },
    Stats {
        reply: oneshot::Sender<CacheStats>,
    },
}

/// What the prompt cache holds now, for `dn run -v` and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct CacheStats {
    /// Tokens in the live sequence.
    pub live_tokens: usize,
    /// Sequences saved in RAM.
    pub saved: usize,
    /// Their size in bytes.
    pub saved_bytes: usize,
}

/// Starts the worker for `l`. It runs until every sender is dropped.
pub(super) fn spawn(l: Arc<Loaded>) -> Result<std::sync::mpsc::Sender<Job>> {
    let (tx, rx) = std::sync::mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("dn-llamacpp-model".into())
        .spawn(move || {
            let mut w = Worker::new(&l);
            while let Ok(job) = rx.recv() {
                w.run(job);
            }
        })
        .map_err(|e| InferenceError::Runtime(e.to_string()))?;
    Ok(tx)
}

/// What the live sequence holds (`seq 0`).
struct Live<'m> {
    ctx: LlamaContext<'m>,
    /// The tokens at positions `0..tokens.len()`, exactly as decoded.
    tokens: Vec<LlamaToken>,
}

struct Worker<'m> {
    l: &'m Loaded,
    live: Option<Live<'m>>,
    /// The draft model's context, when the model has one. It follows the live sequence.
    draft: Option<Live<'m>>,
    cache: SlotCache<LlamaToken, SeqState>,
}

impl<'m> Worker<'m> {
    fn new(l: &'m Loaded) -> Self {
        Self {
            l,
            live: None,
            draft: None,
            cache: SlotCache::new(usize::try_from(l.opts.prompt_cache.ram_bytes).unwrap_or(0)),
        }
    }

    fn run(&mut self, job: Job) {
        match job {
            Job::Chat {
                req,
                thinking,
                tx,
                permit,
            } => {
                let _active = super::ActiveGuard::new();
                let _permit = permit;
                let r = self.guarded(|w| w.chat(&req, thinking, &tx));
                if let Err(e) = r {
                    let _ = tx.blocking_send(Err(e.into()));
                }
            }
            Job::Bench {
                n_prompt,
                n_gen,
                reply,
                permit,
            } => {
                let _permit = permit;
                let r = self.guarded(|w| w.bench(n_prompt, n_gen));
                let _ = reply.send(r);
            }
            Job::Stats { reply } => {
                let _ = reply.send(CacheStats {
                    live_tokens: self.live.as_ref().map_or(0, |l| l.tokens.len()),
                    saved: self.cache.len(),
                    saved_bytes: self.cache.bytes(),
                });
            }
        }
    }

    /// Runs `f`, turning a panic into an error. After any error the cache state is unknown,
    /// so the context and the saved sequences go.
    fn guarded<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        let r = match r {
            Ok(r) => r,
            Err(p) => Err(InferenceError::Runtime(format!(
                "generation panicked: {}",
                super::panic_message(p.as_ref())
            ))),
        };
        if r.is_err() {
            self.live = None;
            self.draft = None;
            self.cache.clear();
        }
        r
    }

    fn live(&mut self) -> Result<&mut Live<'m>> {
        if self.live.is_none() {
            self.live = Some(Live {
                ctx: context(self.l, false)?,
                tokens: Vec::new(),
            });
        }
        Ok(self.live.as_mut().expect("just made"))
    }

    /// Prepares the live sequence for `prompt` and returns how many of its tokens are
    /// already in the cache (0 with reuse off, or when the model can't keep a prefix).
    fn reuse(&mut self, prompt: &[LlamaToken]) -> Result<usize> {
        let enabled = self.l.opts.prompt_cache.enabled && self.l.prefix_reuse.is_ok();
        let cap = self.cache.cap();
        let l = self.l;
        if !enabled {
            let live = self.live()?;
            live.ctx.clear_kv_cache();
            live.tokens.clear();
            return Ok(0);
        }
        let source = {
            let live_tokens = self.live.as_ref().map_or(&[][..], |l| &l.tokens[..]);
            slots::choose(live_tokens, prompt, &self.cache)
        };
        let keep = match source {
            Source::Live { keep } => keep,
            Source::Saved { index, keep } => {
                let saved = self.cache.take(index);
                let live = self.live()?;
                let old = std::mem::take(&mut live.tokens);
                let size = live
                    .ctx
                    .state_seq_get_size_ext(0, LlamaStateSeqFlags::empty());
                let stash = if old.len() >= slots::MIN_SAVE && size <= cap {
                    Some(live.ctx.state_seq_get(0, LlamaStateSeqFlags::empty()))
                } else {
                    None
                };
                live.ctx
                    .state_seq_set(&saved.state, 0)
                    .map_err(|e| InferenceError::Runtime(format!("restore cached prompt: {e}")))?;
                live.tokens = saved.tokens;
                if let Some(state) = stash {
                    let state = state
                        .map_err(|e| InferenceError::Runtime(format!("save cached prompt: {e}")))?;
                    self.cache.put(slots::Saved { tokens: old, state });
                }
                keep
            }
        };
        let live = self.live()?;
        let mut keep = slots::keep_for(keep, prompt.len());
        if keep > 0 && !slots::swa_allows(keep, live.ctx.kv_cache_seq_pos_min(0), l.swa_window) {
            keep = 0;
        }
        // The rest of the live sequence goes. Before it does, save it if a real part of it
        // goes (another chat, not just the re-rendered end of this one's last answer), so the
        // chat it belongs to can come back to it.
        let size = live
            .ctx
            .state_seq_get_size_ext(0, LlamaStateSeqFlags::empty());
        if slots::worth_saving(live.tokens.len(), keep) && size <= cap {
            let state = live
                .ctx
                .state_seq_get(0, LlamaStateSeqFlags::empty())
                .map_err(|e| InferenceError::Runtime(format!("save cached prompt: {e}")))?;
            let tokens = live.tokens.clone();
            self.cache.put(slots::Saved { tokens, state });
        }
        let live = self.live()?;
        if keep == 0 {
            live.ctx.clear_kv_cache();
        } else if live
            .ctx
            .kv_cache_seq_rm(0, Some(keep as u32), None)
            .is_err()
        {
            live.ctx.clear_kv_cache();
            keep = 0;
        }
        live.tokens.truncate(keep);
        Ok(keep)
    }

    fn chat(&mut self, req: &ChatRequest, thinking: bool, tx: &mpsc::Sender<Item>) -> Result<()> {
        let l = self.l;
        let (tokens, in_think) = prompt_tokens(l, &prompt::turns(req), thinking)?;
        let vocab = l.model.vocab();
        let n_prompt = tokens.len();
        if n_prompt as u32 >= l.n_ctx {
            return Err(InferenceError::ContextOverflow {
                prompt: n_prompt,
                n_ctx: l.n_ctx,
            });
        }
        let room = l.n_ctx as usize - n_prompt;
        let max_new = req.max_tokens.map_or(room, |m| (m as usize).min(room));

        let cached = self.reuse(&tokens)?;
        let live = self.live()?;
        let n_batch = l.opts.n_batch.max(1) as usize;
        let mut batch = LlamaBatch::new(n_batch, 1);
        if !decode_from(live, &tokens, cached, n_batch, &mut batch, &|| {
            tx.is_closed()
        })? {
            return Ok(());
        }

        let mut sampler = sampler(l, req)?;
        let mut out = Emitter {
            tx,
            vocab: &vocab,
            parser: TagParser::new()
                .in_think(in_think)
                .with_tools(!req.tools.is_empty()),
            utf8: Utf8Buf::default(),
            generated: 0,
            max_new,
        };
        let mut usage = Usage {
            input_tokens: n_prompt as u32,
            cached_input_tokens: cached as u32,
            ..Usage::default()
        };
        let finish = match l.draft.as_ref() {
            None => {
                let live = self.live.as_mut().expect("made by reuse");
                plain(live, &mut sampler, &mut out, &mut batch)?
            }
            Some(d) => {
                if self.draft.is_none() {
                    self.draft = Some(Live {
                        ctx: super::draft_context(l, d)?,
                        tokens: Vec::new(),
                    });
                }
                let (Some(live), Some(draft)) = (self.live.as_mut(), self.draft.as_mut()) else {
                    unreachable!("both made above");
                };
                let mut s = Speculation {
                    l,
                    live,
                    draft,
                    max_k: d.max_tokens,
                    n_batch,
                };
                s.run(&mut sampler, &mut out, &mut usage, &mut batch)?
            }
        };
        // None: the stream was dropped.
        let Some(mut finish) = finish else {
            return Ok(());
        };
        if !out.finish() {
            return Ok(());
        }
        if out.parser.tool_calls() > 0 {
            finish = FinishReason::ToolCalls;
        }
        usage.output_tokens = out.generated as u32;
        send(
            tx,
            StreamEvent::Done {
                finish_reason: finish,
                usage,
            },
        );
        Ok(())
    }

    /// llama-bench style: a cold prompt of exactly `n_prompt` tokens, then `n_gen` greedy
    /// tokens. The live sequence is cleared first and left empty.
    fn bench(&mut self, n_prompt: u32, n_gen: u32) -> Result<BenchRun> {
        let l = self.l;
        if n_prompt + n_gen >= l.n_ctx {
            return Err(InferenceError::ContextOverflow {
                prompt: (n_prompt + n_gen) as usize,
                n_ctx: l.n_ctx,
            });
        }
        let vocab = l.model.vocab();
        let seed_text = "The quick brown fox jumps over the lazy dog. ".repeat(64);
        let base = vocab.tokenize(seed_text.as_bytes(), false, false);
        let tokens: Vec<LlamaToken> = base
            .iter()
            .cycle()
            .take(n_prompt as usize)
            .copied()
            .collect();
        let live = self.live()?;
        live.ctx.clear_kv_cache();
        live.tokens.clear();
        let n_batch = l.opts.n_batch.max(1) as usize;
        let mut batch = LlamaBatch::new(n_batch, 1);

        let t0 = Instant::now();
        decode_from(live, &tokens, 0, n_batch, &mut batch, &|| false)?;
        // decode() may return before the GPU finishes; wait so the timing is real.
        live.ctx.synchronize();
        let prompt_ms = t0.elapsed().as_secs_f64() * 1e3;

        let mut sampler = LlamaSampler::greedy();
        let t1 = Instant::now();
        for _ in 0..n_gen {
            let token = sampler.sample(&live.ctx, batch.n_tokens() - 1);
            decode_one(live, token, &mut batch)?;
        }
        live.ctx.synchronize();
        let gen_ms = t1.elapsed().as_secs_f64() * 1e3;
        live.ctx.clear_kv_cache();
        live.tokens.clear();
        let rate = |n: u32, ms: f64| if ms > 0.0 { n as f64 * 1e3 / ms } else { 0.0 };
        Ok(BenchRun {
            n_prompt,
            n_gen,
            prompt_ms,
            gen_ms,
            prompt_tokens_per_s: rate(n_prompt, prompt_ms),
            gen_tokens_per_s: rate(n_gen, gen_ms),
        })
    }
}

/// Decodes `tokens[from..]` at their positions, in `n_batch` chunks, with logits only for the
/// last token. Returns `false` if `cancelled` turned true between chunks; the live sequence
/// then holds what was decoded so far.
fn decode_from(
    live: &mut Live<'_>,
    tokens: &[LlamaToken],
    from: usize,
    n_batch: usize,
    batch: &mut LlamaBatch<'_>,
    cancelled: &dyn Fn() -> bool,
) -> Result<bool> {
    debug_assert_eq!(live.tokens.len(), from);
    let last = tokens.len().saturating_sub(1);
    for chunk in tokens[from..].chunks(n_batch) {
        if cancelled() {
            return Ok(false);
        }
        batch.clear();
        for &t in chunk {
            let i = live.tokens.len() + batch.n_tokens() as usize;
            batch
                .add(t, i as i32, &[0], i == last)
                .map_err(|e| InferenceError::Runtime(e.to_string()))?;
        }
        live.ctx
            .decode(batch)
            .map_err(|e| InferenceError::Runtime(format!("decode: {e}")))?;
        live.tokens.extend_from_slice(chunk);
    }
    Ok(true)
}

/// Decodes one generated token after the live sequence.
fn decode_one(live: &mut Live<'_>, token: LlamaToken, batch: &mut LlamaBatch<'_>) -> Result<()> {
    batch.clear();
    batch
        .add(token, live.tokens.len() as i32, &[0], true)
        .map_err(|e| InferenceError::Runtime(e.to_string()))?;
    live.ctx
        .decode(batch)
        .map_err(|e| InferenceError::Runtime(format!("decode: {e}")))?;
    live.tokens.push(token);
    Ok(())
}

/// What happens after a token is handed to the [`Emitter`].
enum Flow {
    Go,
    Stop(FinishReason),
    /// The stream was dropped.
    Closed,
}

/// Turns sampled tokens into stream events, and knows when the answer is done.
struct Emitter<'a, 'v> {
    tx: &'a mpsc::Sender<Item>,
    vocab: &'a llama_cpp_2::vocab::LlamaVocab<'v>,
    parser: TagParser,
    utf8: Utf8Buf,
    generated: usize,
    max_new: usize,
}

impl Emitter<'_, '_> {
    /// Before the first token.
    fn start(&self) -> Flow {
        if self.tx.is_closed() {
            Flow::Closed
        } else if self.max_new == 0 {
            Flow::Stop(FinishReason::Length)
        } else {
            Flow::Go
        }
    }

    /// Tokens that may still be written.
    fn left(&self) -> usize {
        self.max_new - self.generated
    }

    fn token(&mut self, token: LlamaToken) -> Flow {
        if self.tx.is_closed() {
            return Flow::Closed;
        }
        if self.vocab.is_eog(token) {
            return Flow::Stop(FinishReason::Stop);
        }
        self.generated += 1;
        if let Some(s) = self.utf8.push(&piece(self.vocab, token)) {
            for ev in self.parser.push(&s) {
                if !send(self.tx, ev) {
                    return Flow::Closed;
                }
            }
        }
        if self.generated >= self.max_new {
            Flow::Stop(FinishReason::Length)
        } else {
            Flow::Go
        }
    }

    /// Sends what's left in the buffers. `false` if the stream was dropped.
    fn finish(&mut self) -> bool {
        let tail = self.utf8.finish();
        let mut evs = self.parser.push(&tail);
        evs.extend(self.parser.finish());
        evs.into_iter().all(|ev| send(self.tx, ev))
    }
}

/// `Some(finish)` to stop, `None` when the stream was dropped; `Go` goes on.
macro_rules! flow {
    ($f:expr) => {
        match $f {
            Flow::Go => {}
            Flow::Stop(f) => return Ok(Some(f)),
            Flow::Closed => return Ok(None),
        }
    };
}

/// One token at a time from the target model.
fn plain(
    live: &mut Live<'_>,
    sampler: &mut LlamaSampler,
    out: &mut Emitter<'_, '_>,
    batch: &mut LlamaBatch<'_>,
) -> Result<Option<FinishReason>> {
    flow!(out.start());
    loop {
        let token = sampler.sample(&live.ctx, batch.n_tokens() - 1);
        flow!(out.token(token));
        decode_one(live, token, batch)?;
    }
}

/// Cuts `live` back to its first `keep` tokens.
fn trim(live: &mut Live<'_>, keep: usize) -> Result<()> {
    if keep >= live.tokens.len() {
        return Ok(());
    }
    if keep == 0 {
        live.ctx.clear_kv_cache();
    } else {
        live.ctx
            .kv_cache_seq_rm(0, Some(keep as u32), None)
            .map_err(|e| InferenceError::Runtime(format!("cut the cache: {e}")))?;
    }
    live.tokens.truncate(keep);
    Ok(())
}

/// Speculative decoding ([`super::draft`]): the draft proposes, the target checks a whole
/// batch at once.
struct Speculation<'a, 'm> {
    l: &'m Loaded,
    live: &'a mut Live<'m>,
    draft: &'a mut Live<'m>,
    max_k: usize,
    n_batch: usize,
}

impl Speculation<'_, '_> {
    fn run(
        &mut self,
        sampler: &mut LlamaSampler,
        out: &mut Emitter<'_, '_>,
        usage: &mut Usage,
        batch: &mut LlamaBatch<'_>,
    ) -> Result<Option<FinishReason>> {
        flow!(out.start());
        // The draft keeps what it shares with the prompt (nothing with prompt reuse off); the
        // rest it decodes with the first step.
        let keep = if self.l.opts.prompt_cache.enabled {
            slots::common_prefix(&self.draft.tokens, &self.live.tokens)
        } else {
            0
        };
        trim(self.draft, keep)?;
        let mut dbatch = LlamaBatch::new(self.n_batch, 1);
        let mut greedy = LlamaSampler::greedy();
        let mut k = draft::START_K.min(self.max_k);
        let mut last = sampler.sample(&self.live.ctx, batch.n_tokens() - 1);
        flow!(out.token(last));
        loop {
            // Never draft past the context or the answer's limit.
            let room = (self.l.n_ctx as usize).saturating_sub(self.live.tokens.len() + 2);
            let k_now = k.min(room).min(out.left().saturating_sub(1));
            if k_now == 0 {
                decode_one(self.live, last, batch)?;
                last = sampler.sample(&self.live.ctx, 0);
                flow!(out.token(last));
                continue;
            }
            let drafted = self.propose(last, k_now, &mut dbatch, &mut greedy)?;
            // The target decodes `last` and every drafted token, with logits for each.
            batch.clear();
            let base = self.live.tokens.len();
            for (i, &t) in std::iter::once(&last).chain(&drafted).enumerate() {
                batch
                    .add(t, (base + i) as i32, &[0], true)
                    .map_err(|e| InferenceError::Runtime(e.to_string()))?;
            }
            self.live
                .ctx
                .decode(batch)
                .map_err(|e| InferenceError::Runtime(format!("decode: {e}")))?;
            let live = &*self.live;
            let v = draft::verify(&drafted, |i| sampler.sample(&live.ctx, i as i32));
            // Both caches keep `last` and the accepted tokens.
            self.live.tokens.push(last);
            self.live.tokens.extend_from_slice(&drafted[..v.accepted]);
            let n = self.live.tokens.len();
            self.live
                .ctx
                .kv_cache_seq_rm(0, Some(n as u32), None)
                .map_err(|e| InferenceError::Runtime(format!("cut the cache: {e}")))?;
            let keep = slots::common_prefix(&self.draft.tokens, &self.live.tokens);
            trim(self.draft, keep)?;
            usage.draft_tokens += drafted.len() as u32;
            usage.draft_accepted_tokens += v.accepted as u32;
            k = draft::next_k(k, drafted.len(), v.accepted, self.max_k);
            for &t in &drafted[..v.accepted] {
                flow!(out.token(t));
            }
            flow!(out.token(v.next));
            last = v.next;
        }
    }

    /// The draft catches up with the live sequence and `last`, then proposes `k` tokens
    /// greedily.
    fn propose(
        &mut self,
        last: LlamaToken,
        k: usize,
        dbatch: &mut LlamaBatch<'_>,
        greedy: &mut LlamaSampler,
    ) -> Result<Vec<LlamaToken>> {
        let from = self.draft.tokens.len();
        let mut seq = self.live.tokens.clone();
        seq.push(last);
        decode_from(self.draft, &seq, from, self.n_batch, dbatch, &|| false)?;
        let mut drafted = Vec::with_capacity(k);
        for j in 0..k {
            let t = greedy.sample(&self.draft.ctx, dbatch.n_tokens() - 1);
            drafted.push(t);
            if j + 1 < k {
                decode_one(self.draft, t, dbatch)?;
            }
        }
        Ok(drafted)
    }
}
