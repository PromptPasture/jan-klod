//! Context trimming — pure Rust, unit-tested natively.
//!
//! v1 strategy: a char/4 token estimate and a **sliding window**. System messages
//! are always kept (they carry the instructions); among the rest, the most recent
//! messages that fit the remaining budget are kept, oldest dropped first. At least
//! the single most recent non-system message is always kept, even if it alone
//! exceeds the budget (the model still needs the current turn). Summarising the
//! dropped span is a later refinement behind this same seam.

/// Estimated tokens for a message of `char_len` characters (~4 chars/token).
#[must_use]
pub const fn estimate_tokens(char_len: usize) -> usize {
    char_len / 4
}

/// Indices of the messages to keep, in ascending (conversation) order, so that
/// the kept set fits `budget` tokens. `tokens[i]` is message *i*'s estimated
/// token count and `is_system[i]` whether it is a system message.
///
/// The two slices must be the same length.
#[must_use]
pub fn keep_indices(tokens: &[usize], is_system: &[bool], budget: usize) -> Vec<usize> {
    debug_assert_eq!(tokens.len(), is_system.len(), "tokens and is_system must be the same length");
    let total: usize = tokens.iter().sum();
    if total <= budget {
        return (0..tokens.len()).collect();
    }

    // System messages are always kept; the rest share the remaining budget.
    let system_total: usize = tokens
        .iter()
        .zip(is_system)
        .filter_map(|(t, sys)| sys.then_some(*t))
        .sum();
    let mut remaining = budget.saturating_sub(system_total);

    // Walk non-system messages newest → oldest, keeping a contiguous recent window.
    let mut kept_recent = Vec::new();
    for i in (0..tokens.len()).rev() {
        if is_system[i] {
            continue;
        }
        if tokens[i] <= remaining {
            remaining -= tokens[i];
            kept_recent.push(i);
        } else if kept_recent.is_empty() {
            // Always keep the current turn, even if it alone overflows.
            kept_recent.push(i);
            break;
        } else {
            break;
        }
    }

    let mut kept: Vec<usize> = (0..tokens.len()).filter(|i| is_system[*i]).collect();
    kept.extend(kept_recent);
    kept.sort_unstable();
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_budget_keeps_everything() {
        let tokens = [10, 20, 30];
        let is_system = [true, false, false];
        assert_eq!(keep_indices(&tokens, &is_system, 100), vec![0, 1, 2]);
    }

    #[test]
    fn over_budget_drops_oldest_non_system() {
        // system(0) always kept; budget after system = 20; keep newest that fit.
        let tokens = [10, 30, 30, 15]; // total 85 > budget
        let is_system = [true, false, false, false];
        // budget 45: system(10) + remaining 35 -> keep idx3(15) then idx2(30>20? 35-15=20, 30>20) stop.
        let kept = keep_indices(&tokens, &is_system, 45);
        assert_eq!(kept, vec![0, 3], "system + most recent within budget");
    }

    #[test]
    fn always_keeps_the_current_turn_even_if_oversized() {
        let tokens = [5, 1000];
        let is_system = [true, false];
        // budget tiny; the single non-system message is kept regardless.
        assert_eq!(keep_indices(&tokens, &is_system, 1), vec![0, 1]);
    }

    #[test]
    fn estimate_is_chars_over_four() {
        assert_eq!(estimate_tokens(0), 0);
        assert_eq!(estimate_tokens(4), 1);
        assert_eq!(estimate_tokens(10), 2);
    }
}
