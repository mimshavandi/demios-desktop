//! Speculative decoding with a draft model (ADR-0020 §6).
//!
//! A small model of the same family proposes a few tokens. The target model checks them all
//! in one batch. At each position the target's own sampler picks a token, and drafted tokens
//! are kept while they match it. So the output is exactly what the target alone would sample,
//! for any sampler, grammar included; only the speed changes. This is the same rule as
//! llama.cpp's `common_sampler_sample_and_accept_n`.
//!
//! The pinned llama-cpp-2 wraps only same-model MTP drafting, which none of the 14–32B models
//! we list supports. So the draft loop is written here on the plain context and batch API.
//!
//! A draft model must share the target's vocabulary ([`compatible`]). A pair that doesn't fails
//! to load with the reason; nothing runs without the draft when one was asked for.

use llama_cpp_2::model::LlamaModel;

/// Tokens drafted per step at the start; [`next_k`] adjusts it as it goes.
pub(super) const START_K: usize = 4;

/// The fewest tokens drafted per step.
pub(super) const MIN_K: usize = 2;

/// How many tokens may differ in vocabulary size (llama.cpp's limit; some models pad theirs).
const MAX_VOCAB_DIFF: i32 = 128;

/// Checks that `draft` can draft for `target`: the same vocabulary type, the same BOS and EOS,
/// vocabularies of about the same size, and the same text for every token they share
/// (from id 5 on, as llama.cpp does: the first few are often renamed control tokens).
pub(super) fn compatible(target: &LlamaModel, draft: &LlamaModel) -> Result<(), String> {
    let (t, d) = (target.vocab(), draft.vocab());
    if t.vocab_type() != d.vocab_type() {
        return Err(format!(
            "the draft model's tokenizer type ({:?}) isn't the target's ({:?})",
            d.vocab_type(),
            t.vocab_type()
        ));
    }
    if t.bos() != d.bos() || t.eos() != d.eos() || t.should_add_bos() != d.should_add_bos() {
        return Err("the draft model's start and end tokens aren't the target's".into());
    }
    let (nt, nd) = (t.n_tokens(), d.n_tokens());
    if (nt - nd).abs() > MAX_VOCAB_DIFF {
        return Err(format!(
            "the draft model has {nd} tokens and the target {nt}: more than {MAX_VOCAB_DIFF} apart"
        ));
    }
    for id in 5..nt.min(nd) {
        let tok = llama_cpp_2::token::LlamaToken::new(id);
        if t.text(tok) != d.text(tok) {
            return Err(format!(
                "token {id} differs: {:?} in the target, {:?} in the draft",
                t.text(tok),
                d.text(tok)
            ));
        }
    }
    Ok(())
}

/// The draft length for the next step: one more after a step where every drafted token was
/// kept, one fewer when fewer than half were.
pub(super) fn next_k(k: usize, drafted: usize, accepted: usize, max: usize) -> usize {
    let max = max.max(MIN_K);
    if drafted > 0 && accepted == drafted {
        (k + 1).min(max)
    } else if accepted * 2 < drafted {
        k.saturating_sub(1).max(MIN_K)
    } else {
        k.clamp(MIN_K, max)
    }
}

/// What one verification found: how many drafted tokens the target kept, and the token it
/// sampled after them (its own, at the first mismatch or after the last drafted token).
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Verdict<T> {
    pub accepted: usize,
    pub next: T,
}

/// Walks the target's samples for `drafted` (sample `i` is the target's choice at the
/// position of drafted token `i`; there's one more sample than drafted tokens).
pub(super) fn verify<T: PartialEq + Copy>(
    drafted: &[T],
    mut sample: impl FnMut(usize) -> T,
) -> Verdict<T> {
    for (i, &d) in drafted.iter().enumerate() {
        let t = sample(i);
        if t != d {
            return Verdict {
                accepted: i,
                next: t,
            };
        }
    }
    Verdict {
        accepted: drafted.len(),
        next: sample(drafted.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_draft_length_follows_how_much_is_kept() {
        assert_eq!(next_k(4, 4, 4, 8), 5);
        assert_eq!(next_k(8, 8, 8, 8), 8, "never more than the maximum");
        assert_eq!(next_k(4, 4, 1, 8), 3);
        assert_eq!(next_k(2, 2, 0, 8), 2, "never fewer than the minimum");
        assert_eq!(next_k(4, 4, 2, 8), 4);
    }

    #[test]
    fn drafted_tokens_are_kept_while_the_target_agrees() {
        // The target would write 1 2 3 9; the draft proposed 1 2 4 5.
        let target = [1, 2, 3, 9, 0];
        let v = verify(&[1, 2, 4, 5], |i| target[i]);
        assert_eq!(
            v,
            Verdict {
                accepted: 2,
                next: 3
            }
        );
        // Everything kept: the next token is the target's own after the last drafted one.
        let v = verify(&[1, 2, 3], |i| target[i]);
        assert_eq!(
            v,
            Verdict {
                accepted: 3,
                next: 9
            }
        );
        // Nothing kept.
        let v = verify(&[7], |i| target[i]);
        assert_eq!(
            v,
            Verdict {
                accepted: 0,
                next: 1
            }
        );
    }

    #[test]
    fn the_target_samples_each_position_once_in_order() {
        let mut asked = vec![];
        let _ = verify(&[1, 2, 3], |i| {
            asked.push(i);
            [1, 2, 8, 0][i]
        });
        assert_eq!(asked, [0, 1, 2], "it stops at the first mismatch");
    }
}
