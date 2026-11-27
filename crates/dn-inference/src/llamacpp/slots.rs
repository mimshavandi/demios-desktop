//! Prompt reuse between calls (ADR-0020 §6): which cached tokens a new prompt can keep.
//!
//! A loaded model keeps one live context. Its sequence 0 holds the tokens of the last call
//! (prompt and answer). The other recent sequences wait in RAM as saved state ([`SlotCache`]),
//! capped in bytes, least recently used first out. For a new prompt the worker:
//!
//! 1. picks the source sharing the longest token prefix with it: the live sequence, or a
//!    saved one if that shares clearly more ([`choose`]);
//! 2. keeps that prefix, removes the rest and decodes only the new tokens.
//!
//! There are no explicit slot names yet. Matching by prefix gives each chat or surface its own
//! slot without them, because their prompts start differently. When R1's assembler sends a
//! cache key, it can narrow the choice.
//!
//! This module is the bookkeeping only. It never touches llama.cpp, so it's tested on its own.

/// Tokens two sequences share from the start.
pub(super) fn common_prefix<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// A saved sequence must share at least this many more tokens than the live one before it's
/// worth restoring (restoring copies the whole state back).
pub(super) const RESTORE_MARGIN: usize = 64;

/// A live sequence shorter than this isn't worth saving when it's replaced.
pub(super) const MIN_SAVE: usize = 256;

/// A saved state's size in bytes.
pub(super) trait ByteLen {
    fn byte_len(&self) -> usize;
}

impl ByteLen for Vec<u8> {
    fn byte_len(&self) -> usize {
        self.len()
    }
}

impl ByteLen for llama_cpp_2::SeqState {
    fn byte_len(&self) -> usize {
        llama_cpp_2::SeqState::byte_len(self)
    }
}

/// One saved sequence: its tokens and llama.cpp's state for them.
pub(super) struct Saved<T, S> {
    pub tokens: Vec<T>,
    pub state: S,
}

/// Saved sequences in RAM, most recently used last, at most `cap` bytes of state.
pub(super) struct SlotCache<T, S> {
    entries: Vec<Saved<T, S>>,
    cap: usize,
    bytes: usize,
}

impl<T: PartialEq, S: ByteLen> SlotCache<T, S> {
    pub fn new(cap: usize) -> Self {
        Self {
            entries: Vec::new(),
            cap,
            bytes: 0,
        }
    }

    /// Adds a sequence, evicting the least recently used until it fits. A state larger than
    /// the whole cap isn't kept. An entry whose tokens are a prefix of the new one's (an
    /// earlier turn of the same chat) is dropped: the new one covers it.
    pub fn put(&mut self, s: Saved<T, S>) {
        if s.state.byte_len() > self.cap {
            return;
        }
        let mut i = 0;
        while i < self.entries.len() {
            let e = &self.entries[i];
            if e.tokens.len() <= s.tokens.len() && s.tokens.starts_with(&e.tokens) {
                self.bytes -= self.entries.remove(i).state.byte_len();
            } else {
                i += 1;
            }
        }
        while self.bytes + s.state.byte_len() > self.cap && !self.entries.is_empty() {
            self.bytes -= self.entries.remove(0).state.byte_len();
        }
        self.bytes += s.state.byte_len();
        self.entries.push(s);
    }

    /// The longest prefix a saved sequence shares with `prompt`, and its index.
    pub fn best(&self, prompt: &[T]) -> Option<(usize, usize)> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| (i, common_prefix(&e.tokens, prompt)))
            .filter(|&(_, n)| n > 0)
            .max_by_key(|&(i, n)| (n, i))
    }

    /// Takes a saved sequence out (it becomes the live one).
    pub fn take(&mut self, i: usize) -> Saved<T, S> {
        let s = self.entries.remove(i);
        self.bytes -= s.state.byte_len();
        s
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}

/// Whether a live sequence of `len` tokens, of which a call keeps `keep`, should be saved
/// before the rest is cut: it's long enough, and more than [`RESTORE_MARGIN`] tokens go.
pub(super) fn worth_saving(len: usize, keep: usize) -> bool {
    len >= MIN_SAVE && len.saturating_sub(keep) > RESTORE_MARGIN
}

/// Where a new prompt's cached prefix comes from.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Source {
    /// Keep `keep` tokens of the live sequence.
    Live { keep: usize },
    /// Save the live sequence (if worth it), restore saved entry `index`, keep `keep` tokens.
    Saved { index: usize, keep: usize },
}

/// Picks the source for `prompt`, given the live tokens and the cache.
pub(super) fn choose<T: PartialEq, S: ByteLen>(
    live: &[T],
    prompt: &[T],
    cache: &SlotCache<T, S>,
) -> Source {
    let live_keep = common_prefix(live, prompt);
    match cache.best(prompt) {
        Some((index, keep)) if keep >= live_keep + RESTORE_MARGIN => Source::Saved { index, keep },
        _ => Source::Live { keep: live_keep },
    }
}

/// How many cached tokens to keep for a prompt of `prompt_len` tokens that shares `keep`
/// with the cache. At least the last prompt token is always decoded again, since the
/// first answer token is sampled from its logits.
pub(super) fn keep_for(keep: usize, prompt_len: usize) -> usize {
    keep.min(prompt_len.saturating_sub(1))
}

/// Whether a model with sliding-window attention can keep `keep` tokens of a cached sequence
/// whose oldest still-cached position is `pos_min` (`window` tokens per window). The window
/// layers have dropped older tokens, and the next token needs the `window` before it.
/// This is llama-server's rule.
pub(super) fn swa_allows(keep: usize, pos_min: i32, window: Option<u32>) -> bool {
    match window {
        None | Some(0) => true,
        Some(w) => i64::from(pos_min) <= (keep as i64 - i64::from(w)).max(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(tokens: &[u32], bytes: usize) -> Saved<u32, Vec<u8>> {
        Saved {
            tokens: tokens.to_vec(),
            state: vec![0; bytes],
        }
    }

    #[test]
    fn the_common_prefix_is_counted_from_the_start() {
        assert_eq!(common_prefix(&[1, 2, 3], &[1, 2, 4]), 2);
        assert_eq!(common_prefix(&[1, 2], &[1, 2, 3]), 2);
        assert_eq!(common_prefix::<u32>(&[], &[1]), 0);
        assert_eq!(common_prefix(&[9, 2], &[1, 2]), 0);
    }

    #[test]
    fn the_last_prompt_token_is_always_decoded() {
        assert_eq!(keep_for(10, 10), 9);
        assert_eq!(keep_for(10, 20), 10);
        assert_eq!(keep_for(0, 0), 0);
    }

    #[test]
    fn the_cache_keeps_the_most_recent_within_its_cap() {
        let mut c = SlotCache::new(100);
        c.put(saved(&[1, 1], 40));
        c.put(saved(&[2, 2], 40));
        c.put(saved(&[3, 3], 40));
        assert_eq!(c.len(), 2, "the oldest went");
        assert_eq!(c.bytes(), 80);
        assert_eq!(c.best(&[1, 1, 5]), None);
        assert_eq!(c.best(&[3, 3, 5]), Some((1, 2)));
        c.put(saved(&[4], 101));
        assert_eq!(c.len(), 2, "a state larger than the cap isn't kept");
    }

    #[test]
    fn a_later_turn_of_the_same_chat_replaces_the_earlier_one() {
        let mut c = SlotCache::new(1000);
        c.put(saved(&[1, 2, 3], 30));
        c.put(saved(&[7, 8], 20));
        c.put(saved(&[1, 2, 3, 4, 5], 50));
        assert_eq!(c.len(), 2);
        assert_eq!(c.bytes(), 70);
        let s = c.take(c.best(&[1, 2, 3, 4, 5, 6]).unwrap().0);
        assert_eq!(s.tokens, [1, 2, 3, 4, 5]);
        assert_eq!(c.bytes(), 20);
    }

    #[test]
    fn a_saved_sequence_is_restored_only_when_it_shares_clearly_more() {
        let mut c = SlotCache::new(1 << 20);
        let a: Vec<u32> = (0..500).collect();
        let b: Vec<u32> = (1000..1500).collect();
        c.put(Saved {
            tokens: a.clone(),
            state: vec![0; 10],
        });
        // The live sequence is chat B; the prompt continues chat A.
        let mut prompt = a.clone();
        prompt.push(9999);
        assert_eq!(
            choose(&b, &prompt, &c),
            Source::Saved {
                index: 0,
                keep: 500
            }
        );
        // The prompt continues the live chat: keep it.
        let mut prompt = b.clone();
        prompt.push(9999);
        assert_eq!(choose(&b, &prompt, &c), Source::Live { keep: 500 });
        // A saved one sharing only a little more than the live one isn't worth a restore.
        let live: Vec<u32> = (0..480).collect();
        assert_eq!(choose(&live, &a, &c), Source::Live { keep: 480 });
    }

    #[test]
    fn a_sequence_is_saved_when_a_real_part_of_it_would_go() {
        assert!(!worth_saving(100, 0), "too short to matter");
        assert!(
            !worth_saving(1000, 990),
            "only the end of the last answer differs"
        );
        assert!(worth_saving(1000, 900));
        assert!(worth_saving(1000, 0));
    }

    #[test]
    fn sliding_window_models_keep_a_prefix_only_while_its_window_is_cached() {
        assert!(swa_allows(5000, 3000, None));
        // 1024-token window: keeping 5000 needs positions from 3976 on.
        assert!(swa_allows(5000, 3976, Some(1024)));
        assert!(!swa_allows(5000, 3977, Some(1024)));
        assert!(swa_allows(100, 0, Some(1024)));
    }
}
