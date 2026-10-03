//! The plan itself: a list of steps and the mutations a model can make to it.
//!
//! Pure. No WIT, no storage — [`Plan`] goes in as JSON and comes out as JSON,
//! so every rule below is testable on the host target without a component.
//! The glue in `lib.rs` does exactly two things this module cannot: read the
//! previous value and write the next one.
//!
//! # Why every call returns the whole plan
//!
//! Codex's `plan`, Claude Code's `TodoWrite` and oh-my-pi's `todo` all do
//! this, and the reason is the reason the tool exists at all: a small model
//! that mutates a list and is told only "ok" has to reconstruct the list from
//! its own transcript, which is the context pressure this is supposed to
//! relieve. Returning the state costs tokens once and saves the model
//! re-deriving it.

use serde_json::{json, Value};

/// Where a step is.
///
/// Three states, not more. A model that has to choose between six will spend
/// tokens on the choice, and nothing downstream reads this except the model
/// itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not started.
    Todo,
    /// Being worked on now.
    Doing,
    /// Finished.
    Done,
}

/// One step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Stable within a plan, assigned by this module rather than the model —
    /// a model that invents ids reuses them.
    pub id: u32,
    /// What the step is, in the model's own words.
    pub text: String,
    /// Where it is.
    pub state: State,
}

/// The whole plan for a session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// In the order they were added. Order is the model's; nothing sorts it.
    pub steps: Vec<Step>,
    /// The highest id ever issued, which is **not** the highest id present.
    ///
    /// Carried in the stored plan on purpose. Deriving the next id from the
    /// steps that remain reuses the number of a removed step — add a, add b,
    /// remove b, add c, and c is id 2 again, silently retargeting any
    /// instruction the model still holds about b.
    issued: u32,
}

/// Why a mutation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// The arguments were not a recognised operation.
    BadArguments(String),
    /// No step with that id. Named rather than ignored: a model that
    /// mis-numbers a step and is told "ok" will keep mis-numbering it.
    NoSuchStep(u32),
    /// An empty step is a step the model cannot act on later.
    EmptyText,
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadArguments(detail) => write!(f, "not a plan operation: {detail}"),
            Self::NoSuchStep(id) => write!(f, "no step {id} in this plan"),
            Self::EmptyText => write!(f, "a step needs text"),
        }
    }
}

impl State {
    /// The wire spelling, which is also what the model writes.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Doing => "doing",
            Self::Done => "done",
        }
    }

    /// Parse a state the model named.
    fn parse(text: &str) -> Option<Self> {
        match text {
            "todo" => Some(Self::Todo),
            "doing" => Some(Self::Doing),
            "done" => Some(Self::Done),
            _ => None,
        }
    }
}

impl Plan {
    /// Parse a stored plan, treating absence and corruption alike as empty.
    ///
    /// **Deliberately lenient in one direction only.** A plan that cannot be
    /// read is replaced rather than refused, because the alternative is a
    /// session where every call fails and the model cannot clear it. Nothing
    /// depends on a plan's durability — it is working memory, not a record.
    #[must_use]
    pub fn parse(stored: Option<&str>) -> Self {
        let Some(steps) = stored
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .and_then(|value| value.get("steps").cloned())
            .and_then(|steps| steps.as_array().cloned())
        else {
            return Self::default();
        };
        let steps: Vec<Step> = steps
            .iter()
            .filter_map(|step| {
                Some(Step {
                    id: u32::try_from(step.get("id")?.as_u64()?).ok()?,
                    text: step.get("text")?.as_str()?.to_string(),
                    state: State::parse(step.get("state")?.as_str()?)?,
                })
            })
            .collect();
        // Fall back to the highest id present when the stored plan predates
        // this field or was hand-edited. That can reuse an id, but only for a
        // plan that was already malformed — better than restarting at 1,
        // which reuses every id.
        let issued = stored
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .and_then(|value| value.get("issued").and_then(Value::as_u64))
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or_else(|| steps.iter().map(|s| s.id).max().unwrap_or(0));
        Self { steps, issued }
    }

    /// Apply one operation, returning the plan as the model should see it.
    ///
    /// # Errors
    /// [`PlanError`] when the arguments are not an operation, name a step
    /// that is not there, or would add an empty one.
    pub fn apply(&mut self, arguments: &str) -> Result<(), PlanError> {
        let call: Value = serde_json::from_str(arguments)
            .map_err(|err| PlanError::BadArguments(err.to_string()))?;
        let op = call
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| PlanError::BadArguments("no `op`".to_string()))?;
        let id = || {
            call.get("id")
                .and_then(Value::as_u64)
                .and_then(|id| u32::try_from(id).ok())
                .ok_or_else(|| PlanError::BadArguments("`id` must be a number".to_string()))
        };
        match op {
            "list" => {}
            "add" => {
                let text = call
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| PlanError::BadArguments("`text` must be a string".to_string()))?
                    .trim()
                    .to_string();
                if text.is_empty() {
                    return Err(PlanError::EmptyText);
                }
                self.issued += 1;
                self.steps.push(Step {
                    id: self.issued,
                    text,
                    state: State::Todo,
                });
            }
            "set-state" => {
                let id = id()?;
                let state = call
                    .get("state")
                    .and_then(Value::as_str)
                    .and_then(State::parse)
                    .ok_or_else(|| {
                        PlanError::BadArguments("`state` is todo, doing or done".to_string())
                    })?;
                let step = self
                    .steps
                    .iter_mut()
                    .find(|s| s.id == id)
                    .ok_or(PlanError::NoSuchStep(id))?;
                step.state = state;
            }
            "remove" => {
                let id = id()?;
                let before = self.steps.len();
                self.steps.retain(|s| s.id != id);
                if self.steps.len() == before {
                    return Err(PlanError::NoSuchStep(id));
                }
            }
            "clear" => self.steps.clear(),
            other => return Err(PlanError::BadArguments(format!("unknown op `{other}`"))),
        }
        Ok(())
    }

    /// The plan as the model sees it.
    #[must_use]
    pub fn render(&self) -> String {
        let steps: Vec<Value> = self
            .steps
            .iter()
            .map(|step| json!({"id": step.id, "text": step.text, "state": step.state.as_str()}))
            .collect();
        json!({ "steps": steps, "issued": self.issued }).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{Plan, PlanError, State};

    fn plan_with(ops: &[&str]) -> Plan {
        let mut plan = Plan::default();
        for op in ops {
            plan.apply(op).expect("the fixture's operations are valid");
        }
        plan
    }

    #[test]
    fn adding_a_step_starts_it_at_todo() {
        let plan = plan_with(&[r#"{"op":"add","text":"write the guest"}"#]);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].id, 1);
        assert_eq!(plan.steps[0].state, State::Todo);
    }

    #[test]
    fn a_step_moves_between_states() {
        let mut plan = plan_with(&[r#"{"op":"add","text":"a"}"#]);
        plan.apply(r#"{"op":"set-state","id":1,"state":"doing"}"#)
            .expect("moves");
        assert_eq!(plan.steps[0].state, State::Doing);
    }

    /// The failure a small model makes most: referring to a step that is not
    /// there. Silence would teach it the number was right.
    #[test]
    fn naming_a_step_that_is_not_there_is_an_error_not_a_no_op() {
        let mut plan = plan_with(&[r#"{"op":"add","text":"a"}"#]);
        assert_eq!(
            plan.apply(r#"{"op":"set-state","id":7,"state":"done"}"#),
            Err(PlanError::NoSuchStep(7))
        );
        assert_eq!(
            plan.apply(r#"{"op":"remove","id":7}"#),
            Err(PlanError::NoSuchStep(7))
        );
    }

    /// Ids do not get reused, because the model refers to them by number and
    /// a reused number silently retargets an instruction.
    #[test]
    fn removing_a_step_does_not_free_its_id() {
        let mut plan = plan_with(&[
            r#"{"op":"add","text":"a"}"#,
            r#"{"op":"add","text":"b"}"#,
            r#"{"op":"remove","id":2}"#,
            r#"{"op":"add","text":"c"}"#,
        ]);
        let ids: Vec<u32> = plan.steps.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![1, 3], "id 2 came back");
        plan.apply(r#"{"op":"clear"}"#).expect("clears");
        assert!(plan.steps.is_empty());
    }

    #[test]
    fn an_empty_step_is_refused() {
        let mut plan = Plan::default();
        assert_eq!(
            plan.apply(r#"{"op":"add","text":"   "}"#),
            Err(PlanError::EmptyText)
        );
    }

    #[test]
    fn arguments_that_are_not_an_operation_say_so() {
        let mut plan = Plan::default();
        assert!(matches!(
            plan.apply(r#"{"op":"reticulate"}"#),
            Err(PlanError::BadArguments(_))
        ));
        assert!(matches!(
            plan.apply("not json"),
            Err(PlanError::BadArguments(_))
        ));
    }

    /// A stored value that cannot be read is replaced, not fatal. The
    /// alternative is a session where the model can neither read the plan nor
    /// clear it.
    #[test]
    fn an_unreadable_stored_plan_parses_as_empty() {
        assert_eq!(Plan::parse(Some("{{{")), Plan::default());
        assert_eq!(Plan::parse(None), Plan::default());
    }

    #[test]
    fn a_plan_survives_a_round_trip_through_storage() {
        let plan = plan_with(&[
            r#"{"op":"add","text":"a"}"#,
            r#"{"op":"add","text":"b"}"#,
            r#"{"op":"set-state","id":1,"state":"done"}"#,
        ]);
        assert_eq!(Plan::parse(Some(&plan.render())), plan);
    }
}
