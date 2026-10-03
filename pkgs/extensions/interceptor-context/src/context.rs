//! Context trimming — pure Rust, unit-tested.
//!
//! v1: char/4 token estimate + sliding window. System messages always kept;
//! recent non-system messages kept oldest-first until budget exhausted.
//! Current turn always kept even if it overflows.

/// Estimate tokens (~4 chars/token).
#[must_use]
pub const fn estimate_tokens(char_len: usize) -> usize {
    char_len / 4
}

/// Return message indices to keep within `budget` tokens, in conversation order.
///
/// `tokens[i]` is message i's estimated cost; `is_system[i]` indicates system messages.
/// Both slices must have equal length.
#[must_use]
pub fn keep_indices(tokens: &[usize], is_system: &[bool], budget: usize) -> Vec<usize> {
    debug_assert_eq!(
        tokens.len(),
        is_system.len(),
        "tokens and is_system must be the same length"
    );
    let total: usize = tokens.iter().sum();
    if total <= budget {
        return (0..tokens.len()).collect();
    }

    // System messages always kept; non-system messages split remaining budget.
    let system_total: usize = tokens
        .iter()
        .zip(is_system)
        .filter_map(|(t, sys)| sys.then_some(*t))
        .sum();
    let mut remaining = budget.saturating_sub(system_total);

    // Walk newest non-system messages, keeping recent window.
    let mut kept_recent = Vec::new();
    for i in (0..tokens.len()).rev() {
        if is_system[i] {
            continue;
        }
        if tokens[i] <= remaining {
            remaining -= tokens[i];
            kept_recent.push(i);
        } else if kept_recent.is_empty() {
            // Always keep current turn even if it overflows.
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
        // total 85 > budget 45; system(10) + recent window within 35.
        let tokens = [10, 30, 30, 15];
        let is_system = [true, false, false, false];
        let kept = keep_indices(&tokens, &is_system, 45);
        assert_eq!(kept, vec![0, 3], "system + most recent fit");
    }

    #[test]
    fn always_keeps_the_current_turn_even_if_oversized() {
        let tokens = [5, 1000];
        let is_system = [true, false];
        // Current turn kept even at tiny budget.
        assert_eq!(keep_indices(&tokens, &is_system, 1), vec![0, 1]);
    }

    #[test]
    fn estimate_is_chars_over_four() {
        assert_eq!(estimate_tokens(0), 0);
        assert_eq!(estimate_tokens(4), 1);
        assert_eq!(estimate_tokens(10), 2);
    }
}
