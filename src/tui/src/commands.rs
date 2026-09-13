//! The `/` command table (#152).
//!
//! **One list.** Menu renders from this; help overlay reads the same constant.
//! Hand-written table vs. actual menu → disagreement → commands that do nothing.
//!
//! # Present and honest beats absent
//!
//! Unavailable commands still list their blockers (absent looks like never-planned).
//! Silent lack of `/cancel` suggests cancellation isn't a feature;
//! listing it + reason teaches when to retry. All act now, but mechanism persists.
//!
//! # `Ready` means "acts", not "needs no transport"
//!
//! `/cancel` acts but needs transport (breaking the old coincidence).
//! This type never knows about transport (load-bearing). `/cancel` flags intent
//! like `/quit` does; the event loop (which owns transport) sends it.

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
        availability: Availability::Ready,
    },
    Command {
        name: "/new",
        summary: "start a new session",
        // `session/create` (#157) plus the session-owner change 19h's
        // switcher made (#105): `App::request_new_session` asks, the loop
        // sends it and switches with `App::load_session`.
        availability: Availability::Ready,
    },
    Command {
        name: "/sessions",
        summary: "switch session",
        // 19h's switcher (#105): `App::request_sessions` asks for a fresh
        // `session/list`, the loop sends it and `App::open_sessions` shows it.
        availability: Availability::Ready,
    },
    Command {
        name: "/help",
        summary: "show the key bindings",
        // 19h's overlay (#105): `App::toggle_help` needs no transport at all.
        availability: Availability::Ready,
    },
    Command {
        name: "/quit",
        summary: "leave jan-klod",
        availability: Availability::Ready,
    },
];

/// Commands whose name starts with `typed` (prefix filtering, not substring search).
/// Exact match sorts first: `/new` typed must run `/new`, not `/newline`
/// (even if `/newline` is listed first).
#[must_use]
pub fn matching(typed: &str) -> Vec<&'static Command> {
    let mut found: Vec<&'static Command> = COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(typed))
        .collect();
    found.sort_by_key(|c| usize::from(c.name != typed));
    found
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

    /// Unavailable commands must state their blocker, or listing is worse than omitting.
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
        assert_eq!(
            ne,
            vec!["/newline", "/new"],
            "table order while neither is exact"
        );
        assert_eq!(matching("/newl").len(), 1);

        // `/new` is a prefix of `/newline`, and typing it in full must select
        // it rather than the longer name that happens to be listed first.
        let exact: Vec<&str> = matching("/new").iter().map(|c| c.name).collect();
        assert_eq!(
            exact,
            vec!["/new", "/newline"],
            "an exact match comes first"
        );
        assert!(
            matching("/zzz").is_empty(),
            "no match is empty, and the menu shows an empty state rather than closing"
        );
        assert!(
            matching("ew").is_empty(),
            "substring would match `/new` here, and a prefix menu is narrowing rather than searching"
        );
    }

    /// Pin which commands claim to act (rename reflects finding: `/cancel` is ready
    /// but needs transport, separating from "needs only composer" which it coincided with before).
    #[test]
    fn exactly_the_commands_that_act_are_ready() {
        let ready: Vec<&str> = COMMANDS
            .iter()
            .filter(|c| c.availability == Availability::Ready)
            .map(|c| c.name)
            .collect();
        assert_eq!(
            ready,
            vec!["/newline", "/cancel", "/new", "/sessions", "/help", "/quit"],
            "a command became ready or stopped being so — say which, and why, \
             rather than editing this list to match"
        );
    }
}
