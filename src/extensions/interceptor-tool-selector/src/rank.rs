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
        Self { terms }
    }

    /// How many times `term` appears.
    fn count(&self, term: &str) -> usize {
        self.terms.iter().filter(|t| *t == term).count()
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

#[cfg(test)]
mod tests {
    use super::{rank, terms, Doc};

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
}
