//! The `/` command table (#152).
//!
//! **One list.** The menu renders from this, and 19h's help overlay will read
//! the same constant. A menu hand-written beside a documented table is a menu
//! that will disagree with it, and the disagreement shows up as a command that
//! does nothing — which reads to a user as a broken client rather than as a
//! stale list.
//!
//! # Present and honest beats absent
//!
//! Four of the six commands cannot act yet. They are here anyway, each carrying
//! the reason, because **absent is not a state a user can tell from never
//! planned**. A `/` menu that silently lacks `/cancel` teaches someone that
//! cancelling is not a thing; one that lists it and says what it is waiting on
//! teaches them when to look again.
//!
//! What each is waiting on is a fact about this crate rather than a guess:
//! `Transport` carries exactly `stream_turn`, `answer` and `describe`. The
//! protocol defines `Command::SessionCreate` and `Command::TurnCancel`, but the
//! client cannot send either — see
//! [#157](https://github.com/PromptPasture/jan-klod/issues/157).

/// Whether a command can act, and if not, what it is waiting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// It does what it says.
    Ready,
    /// It is listed, and selecting it explains this instead of acting.
    Pending(&'static str),
}

/// One entry in the `/` menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    /// What the user types, including the slash.
    pub name: &'static str,
    /// One line, shown beside the name.
    pub summary: &'static str,
    /// Whether it acts yet.
    pub availability: Availability,
}

/// The commands, in the order the menu offers them.
///
/// Ordered by how often they are wanted rather than alphabetically: a menu is
/// read top-down and `/quit` is not what anyone came for.
pub const COMMANDS: [Command; 6] = [
    Command {
        name: "/newline",
        summary: "insert a line break",
        // The only one that needs nothing but the composer, and the reason the
        // list exists at all: where a terminal delivers neither `Shift+Enter`
        // nor `Alt+Enter`, this is how a user types a newline.
        availability: Availability::Ready,
    },
    Command {
        name: "/cancel",
        summary: "stop the running turn",
        availability: Availability::Pending("the client cannot send `turn/cancel` yet — see #157"),
    },
    Command {
        name: "/new",
        summary: "start a new session",
        availability: Availability::Pending(
            "the client cannot send `session/create` yet — see #157",
        ),
    },
    Command {
        name: "/sessions",
        summary: "switch session",
        availability: Availability::Pending("the session switcher is 19h (#105)"),
    },
    Command {
        name: "/help",
        summary: "show the key bindings",
        availability: Availability::Pending("the help overlay is 19h (#105)"),
    },
    Command {
        name: "/quit",
        summary: "leave jan-klod",
        availability: Availability::Ready,
    },
];

/// The commands whose name starts with `typed`, which is `/` plus a fragment.
///
/// Substring would match `/new` for `s`, which is not what a prefix menu means;
/// a user typing `/se` is narrowing, not searching.
#[must_use]
pub fn matching(typed: &str) -> Vec<&'static Command> {
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(typed))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{matching, Availability, COMMANDS};

    #[test]
    fn every_command_is_a_slash_and_a_name_and_they_are_distinct() {
        let mut seen: Vec<&str> = Vec::new();
        for command in COMMANDS {
            assert!(
                command.name.starts_with('/') && command.name.len() > 1,
                "{:?} is not a command name",
                command.name
            );
            assert!(
                !seen.contains(&command.name),
                "{} appears twice, so one of them can never be selected",
                command.name
            );
            assert!(
                !command.summary.is_empty(),
                "{} has no summary",
                command.name
            );
            seen.push(command.name);
        }
    }

    /// The point of `Availability`: a command that cannot act must say what it
    /// is waiting on, or listing it is worse than leaving it out.
    #[test]
    fn every_pending_command_carries_a_reason() {
        for command in COMMANDS {
            if let Availability::Pending(reason) = command.availability {
                assert!(
                    reason.len() > 10,
                    "{} is pending on {reason:?}, which tells a user nothing",
                    command.name
                );
            }
        }
    }

    #[test]
    fn filtering_is_a_prefix_not_a_search() {
        assert_eq!(matching("/").len(), COMMANDS.len(), "bare slash offers all");
        let ne: Vec<&str> = matching("/ne").iter().map(|c| c.name).collect();
        assert_eq!(ne, vec!["/newline", "/new"]);
        assert_eq!(matching("/newl").len(), 1);
        assert!(
            matching("/zzz").is_empty(),
            "no match is empty, and the menu shows an empty state rather than closing"
        );
        assert!(
            matching("ew").is_empty(),
            "substring would match `/new` here, and a prefix menu is narrowing rather than searching"
        );
    }

    /// The two that act need nothing this crate does not have; if that stops
    /// being true, this test is where it surfaces.
    #[test]
    fn exactly_the_commands_that_need_no_transport_are_ready() {
        let ready: Vec<&str> = COMMANDS
            .iter()
            .filter(|c| c.availability == Availability::Ready)
            .map(|c| c.name)
            .collect();
        assert_eq!(
            ready,
            vec!["/newline", "/quit"],
            "a command became ready or stopped being so — say which, and why, \
             rather than editing this list to match"
        );
    }
}
