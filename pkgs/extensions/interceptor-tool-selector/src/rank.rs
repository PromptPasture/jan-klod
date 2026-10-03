//! Which tools the model is shown, and in what order (#219).
//!
//! Pure: text in, indices out. No WIT, no host, so every rule here is
//! tested on the host target without a component.
//!
//! # Why ranking and not filtering
//!
//! A tool the model needed and was not shown is a task it cannot do, and
//! it has no way to ask for the tool back — #217 was declined on exactly
//! that finding. So this returns an *order*, and the caller cuts with a
//! floor. A bad ranking then degrades to "the model saw more tools than it
//! needed", which is the problem we already have, rather than to "the model
//! could not do the job".
//!
//! # BM25, by hand
//!
//! Forty lines over term frequencies. The alternative is a search crate,
//! which would be the first in this tree, for a document set of a dozen
//! one-line tool descriptions. `k1` and `b` are the usual defaults; nothing
//! here is tuned, because tuning against eleven documents would be fitting
//! noise.

/// Term-frequency saturation. Standard BM25 default.
const K1: f64 = 1.2;
/// Length normalisation. Standard BM25 default.
const B: f64 = 0.75;

/// Split text into lowercase terms.
///
/// Single characters are dropped: they carry no signal over descriptions
/// this short, and `a`/`I` in a sentence would otherwise match everything.
fn terms(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() > 1)
        .map(str::to_lowercase)
        .collect()
}

/// One tool as this module sees it.
pub struct Doc {
    /// Lowercased, for [`Doc::named_in`].
    name: String,
    /// Its terms, from name and description together — a tool called
    /// `git` with a terse description is still findable by its name.
    terms: Vec<String>,
}

impl Doc {
    /// Build a document from a tool's name and description.
    #[must_use]
    pub fn new(name: &str, description: &str) -> Self {
        let mut terms = terms(name);
        terms.extend(self::terms(description));
        Self {
            name: name.to_lowercase(),
            terms,
        }
    }

    /// How many times `term` appears.
    fn count(&self, term: &str) -> usize {
        self.terms.iter().filter(|t| *t == term).count()
    }

    /// Whether `lowered` — already-lowercased conversation text — names this
    /// tool.
    ///
    /// A bounded substring rather than a term match, because the names are
    /// `ext-install` and `tool-fs`: [`terms`] would have split those into
    /// words that any sentence about installing an extension matches. The
    /// boundaries are what stop `fs` from being found inside `offset`.
    fn named_in(&self, lowered: &str) -> bool {
        if self.name.is_empty() {
            return false;
        }
        lowered.match_indices(&self.name).any(|(at, _)| {
            let before = lowered[..at].chars().next_back();
            let after = lowered[at + self.name.len()..].chars().next();
            !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
        })
    }
}

/// Order `docs` by relevance to `query`, most relevant first.
///
/// Returns indices into `docs`. **Ties keep their original order**, so the
/// advertised set does not churn between turns for reasons the user cannot
/// see — a model that watched its tools shuffle every turn would have to
/// re-read them every turn, which is the cost this exists to remove.
///
/// An empty result means nothing matched, which is different from
/// "everything matched equally": the caller shows its floor rather than
/// an arbitrary prefix.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "the counts are tools in a fleet and terms in a one-line \
              description; f64 loses nothing below 2^52, and these are two digits"
)]
pub fn rank(docs: &[Doc], query: &str) -> Vec<usize> {
    let query_terms = terms(query);
    if query_terms.is_empty() || docs.is_empty() {
        return Vec::new();
    }
    let total = docs.len() as f64;
    let average_len = docs.iter().map(|d| d.terms.len()).sum::<usize>() as f64 / total;

    let mut scored: Vec<(usize, f64)> = docs
        .iter()
        .enumerate()
        .map(|(index, doc)| {
            let len = doc.terms.len() as f64;
            let score = query_terms
                .iter()
                .map(|term| {
                    let count = doc.count(term) as f64;
                    if count == 0.0 {
                        return 0.0;
                    }
                    // How many documents contain it: a term in every tool's
                    // description ("the", "a file") says nothing about which
                    // tool to pick, and idf is what makes it say nothing.
                    let having = docs.iter().filter(|d| d.count(term) > 0).count() as f64;
                    let idf = ((total - having + 0.5) / (having + 0.5)).ln_1p();
                    let norm = count * (K1 + 1.0);
                    let denom = K1.mul_add(B.mul_add(len / average_len, 1.0 - B), count);
                    idf * norm / denom
                })
                .sum::<f64>();
            (index, score)
        })
        .filter(|(_, score)| *score > 0.0)
        .collect();

    // `sort_by` is stable, so equal scores keep input order. Descending by
    // score, and `partial_cmp` cannot fail here: every score is finite.
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().map(|(index, _)| index).collect()
}

/// How many tools to advertise, at most and at least.
#[derive(Clone, Copy)]
pub struct Cut {
    /// The bound. **`0` means no bound**: advertise the whole fleet.
    ///
    /// The default, because the right number is a property of the fleet and
    /// the model rather than of this code, and a bound this module invented
    /// would hide a tool the operator never agreed to hide. Same shape as
    /// every other lossy behaviour here — `execution`, `persist`, `host-fs`
    /// — off until someone asks for it.
    pub most: usize,
    /// The floor: how many to advertise when the ranking is thin or empty.
    ///
    /// This is what makes a bad ranking survivable. The tools it tops up
    /// with are the fleet's first, in the order the operator configured
    /// them, which is the only signal available when nothing matched.
    pub least: usize,
}

/// Which tools to advertise, as indices in the fleet's own order.
///
/// Order is the input's, not the ranking's: a model whose tool list
/// reshuffled every turn would re-read it every turn, which is the cost
/// this exists to remove. The cut is where the saving comes from.
///
/// Two rules outrank `most`, and both are deliberate:
///
/// * a tool named anywhere in the conversation is always advertised —
///   withdrawing a tool the model is mid-way through using would strand it,
///   and #217 established it cannot ask for the tool back;
/// * `least` is topped up even past `most`, because the floor exists to
///   bound the damage and an operator who set them in conflict meant the
///   safer one.
#[must_use]
pub fn select(docs: &[Doc], asked: &str, conversation: &str, cut: Cut) -> Vec<usize> {
    if cut.most == 0 || cut.most >= docs.len() {
        return (0..docs.len()).collect();
    }
    let lowered = conversation.to_lowercase();
    let mut keep: Vec<usize> = (0..docs.len())
        .filter(|&index| docs[index].named_in(&lowered))
        .collect();
    for index in rank(docs, asked) {
        if keep.len() >= cut.most {
            break;
        }
        if !keep.contains(&index) {
            keep.push(index);
        }
    }
    let floor = cut.least.min(docs.len());
    for index in 0..docs.len() {
        if keep.len() >= floor {
            break;
        }
        if !keep.contains(&index) {
            keep.push(index);
        }
    }
    keep.sort_unstable();
    keep
}

#[cfg(test)]
mod tests {
    use super::{rank, select, terms, Cut, Doc};

    fn fleet() -> Vec<Doc> {
        vec![
            Doc::new("fs", "read and write files in the workspace"),
            Doc::new("git", "status, diff and log of the repository"),
            Doc::new("fetch", "make an outbound HTTP request to a URL"),
            Doc::new("plan", "keep a plan of the steps for this task"),
        ]
    }

    #[test]
    fn a_matching_description_outranks_one_that_does_not() {
        let docs = fleet();
        let order = rank(&docs, "show me the diff of the repository");
        assert_eq!(order.first(), Some(&1), "git should lead: {order:?}");
        assert!(
            !order.contains(&2),
            "fetch shares no term and should not appear: {order:?}"
        );
    }

    /// A tool is findable by its name even when the description says little.
    #[test]
    fn a_tool_is_found_by_its_name() {
        let docs = fleet();
        assert_eq!(rank(&docs, "use plan").first(), Some(&3));
    }

    /// A term every tool uses cannot decide between them, which is what idf
    /// is for. Without it, "the" would rank whichever description was
    /// longest.
    #[test]
    fn a_term_common_to_every_tool_does_not_decide_the_order() {
        let docs = vec![
            Doc::new("a", "the the the the the"),
            Doc::new("b", "the quick brown fox"),
        ];
        let order = rank(&docs, "the quick");
        assert_eq!(
            order.first(),
            Some(&1),
            "the rare term should decide, not the common one: {order:?}"
        );
    }

    /// Nothing matched is empty, not an arbitrary prefix. The caller shows
    /// its floor; guessing here would hide that nothing was understood.
    #[test]
    fn a_query_matching_nothing_ranks_nothing() {
        assert!(rank(&fleet(), "xylophone marzipan").is_empty());
        assert!(rank(&fleet(), "").is_empty());
        assert!(rank(&fleet(), "  , . !").is_empty());
    }

    /// Equal scores keep input order, so the advertised set does not churn
    /// between turns for reasons the user cannot see.
    #[test]
    fn ties_keep_their_original_order() {
        let docs = vec![
            Doc::new("one", "identical wording here"),
            Doc::new("two", "identical wording here"),
            Doc::new("three", "identical wording here"),
        ];
        assert_eq!(rank(&docs, "identical wording"), vec![0, 1, 2]);
    }

    /// Single characters carry no signal over descriptions this short, and
    /// would otherwise match everything.
    #[test]
    fn single_characters_are_not_terms() {
        assert_eq!(terms("a file I read"), vec!["file", "read"]);
    }

    #[test]
    fn an_empty_fleet_ranks_nothing_rather_than_panicking() {
        assert!(rank(&[], "anything").is_empty());
    }

    /// A fleet with hyphenated names, as the real ones are.
    fn named_fleet() -> Vec<Doc> {
        vec![
            Doc::new("tool-fs", "read and write files in the workspace"),
            Doc::new("tool-git", "status, diff and log of the repository"),
            Doc::new("tool-fetch", "make an outbound HTTP request to a URL"),
            Doc::new("ext-install", "add an extension to this agent"),
        ]
    }

    fn cut(most: usize, least: usize) -> Cut {
        Cut { most, least }
    }

    /// The acceptance: the relevant tool is advertised and the unrelated one
    /// is not.
    #[test]
    fn an_unrelated_tool_is_not_advertised_and_a_relevant_one_is() {
        let docs = named_fleet();
        let asked = "show me the diff of the repository";
        let kept = select(&docs, asked, asked, cut(2, 1));
        assert!(kept.contains(&1), "git was dropped: {kept:?}");
        assert!(!kept.contains(&2), "fetch was advertised: {kept:?}");
    }

    /// A tool the model is mid-way through using outranks the ranking: with
    /// room for one, the named tool takes it and the better match does not.
    #[test]
    fn a_tool_named_in_the_conversation_is_kept_ahead_of_a_better_match() {
        let docs = named_fleet();
        let asked = "show me the diff of the repository";
        let conversation = format!("{asked}\ncalling tool-fetch with a URL");
        assert_eq!(
            rank(&docs, asked).first(),
            Some(&1),
            "the premise: git is the better match"
        );
        assert_eq!(
            select(&docs, asked, &conversation, cut(1, 1)),
            vec![2],
            "a tool in mid-use was withdrawn in favour of a better match"
        );
    }

    /// And it survives the bound outright: more tools in mid-use than the
    /// bound allows means the bound yields, not the tools. Withdrawing one
    /// would strand the model, which cannot ask for it back.
    #[test]
    fn tools_in_mid_use_are_kept_even_past_the_bound() {
        let docs = named_fleet();
        let conversation = "I ran tool-fetch and then ext-install";
        assert_eq!(select(&docs, "", conversation, cut(1, 1)), vec![2, 3]);
    }

    /// The name has to be the whole word: `tool-fs` must not be found
    /// inside `offset`, or every tool would be permanently in mid-use.
    #[test]
    fn a_name_inside_a_longer_word_is_not_a_mention() {
        let doc = Doc::new("fs", "unrelated wording");
        assert!(
            !doc.named_in("the offset was wrong"),
            "matched inside a word"
        );
        assert!(doc.named_in("run fs, please"));
    }

    /// Nothing matched is the dangerous case, and the floor is the answer:
    /// something is advertised rather than nothing.
    #[test]
    fn a_request_matching_nothing_still_advertises_the_floor() {
        let docs = named_fleet();
        assert_eq!(
            select(&docs, "xylophone", "xylophone", cut(2, 2)),
            vec![0, 1]
        );
        assert_eq!(select(&docs, "", "", cut(2, 1)), vec![0]);
    }

    /// Unbounded is the default, and the default changes nothing.
    #[test]
    fn with_no_bound_the_whole_fleet_is_advertised() {
        let docs = named_fleet();
        assert_eq!(select(&docs, "diff", "diff", cut(0, 2)), vec![0, 1, 2, 3]);
        assert_eq!(
            select(&docs, "diff", "diff", cut(99, 2)),
            vec![0, 1, 2, 3],
            "a bound above the fleet size is no bound"
        );
    }

    /// Set in conflict, the floor wins: it is the rule that bounds the
    /// damage, and the bound is only an economy.
    #[test]
    fn the_floor_outranks_the_bound() {
        let docs = named_fleet();
        assert_eq!(select(&docs, "diff", "diff", cut(1, 3)).len(), 3);
    }

    /// Advertised in the fleet's order, never the ranking's.
    #[test]
    fn the_advertised_set_keeps_the_fleets_order() {
        let docs = named_fleet();
        let asked = "url request outbound, and the log";
        assert_eq!(
            rank(&docs, asked),
            vec![2, 1, 0],
            "the premise: fetch outranks git for this text"
        );
        let kept = select(&docs, asked, asked, cut(2, 1));
        assert_eq!(kept, vec![1, 2], "reordered by score: {kept:?}");
    }
}
