//! Context trimming: keeps system messages and the current turn, then fills the budget with recent messages.

/// ~4 chars per token.
#[must_use]
pub const fn estimate_tokens(char_len: usize) -> usize {
    char_len / 4
}

/// `tokens[i]` and `is_system[i]` describe message i; the result is in conversation order.
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

    let system_total: usize = tokens
        .iter()
        .zip(is_system)
        .filter_map(|(t, sys)| sys.then_some(*t))
        .sum();
    let mut remaining = budget.saturating_sub(system_total);

    let mut kept_recent = Vec::new();
    for i in (0..tokens.len()).rev() {
        if is_system[i] {
            continue;
        }
        if tokens[i] <= remaining {
            remaining -= tokens[i];
            kept_recent.push(i);
        } else if kept_recent.is_empty() {
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
        let tokens = [10, 30, 30, 15];
        let is_system = [true, false, false, false];
        let kept = keep_indices(&tokens, &is_system, 45);
        assert_eq!(kept, vec![0, 3], "system + most recent fit");
    }

    #[test]
    fn always_keeps_the_current_turn_even_if_oversized() {
        let tokens = [5, 1000];
        let is_system = [true, false];
        assert_eq!(keep_indices(&tokens, &is_system, 1), vec![0, 1]);
    }

    #[test]
    fn estimate_is_chars_over_four() {
        assert_eq!(estimate_tokens(0), 0);
        assert_eq!(estimate_tokens(4), 1);
        assert_eq!(estimate_tokens(10), 2);
    }
}
