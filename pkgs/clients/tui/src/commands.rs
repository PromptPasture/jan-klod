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

/// A command an extension contributed, as this client holds it.
///
/// Owned, unlike [`Command`]: the built-in table is a `const` of `&'static
/// str`, and contributions arrive at runtime over the protocol. Rather than
/// make [`Command`] owned — which would ripple through every use of a type
/// that is `Copy` today — the menu reads a borrowed [`Entry`] over both.
///
/// The strings are stored **already made inert** ([`crate::untrusted::inert`]).
/// Cleaning at the door rather than at each frame means no renderer has to
/// remember to do it, and a label cannot be safe in the menu and hostile in
/// the help overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contributed {
    /// The declaring extension's instance id, e.g. `interceptor.system`.
    /// Needed to invoke it: two extensions may contribute the same name.
    pub extension: String,
    /// The contributed name, without a slash.
    pub name: String,
    /// One line, shown beside the name.
    pub summary: String,
}

/// Where a menu entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source<'a> {
    /// The built-in table.
    BuiltIn,
    /// An extension, by instance id.
    Extension(&'a str),
}

/// One row of the menu, from either source.
///
/// Borrowed so the `const` table stays `const` and contributions are not
/// cloned on every keystroke — the menu is rebuilt as the filter changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    /// What the user types, including the slash.
    pub name: &'a str,
    /// One line, shown beside the name.
    pub summary: &'a str,
    /// Whether it acts yet.
    pub availability: Availability,
    /// Which list it came from.
    pub source: Source<'a>,
}

/// The built-in commands whose name starts with `typed`, most exact first.
///
/// Prefix filtering, not substring search, and sorted rather than narrowed to
/// one: a prefix is still a match, and a menu that emptied on the first
/// differing character would read as a dropped keystroke. The exact match
/// leads, which is why typing `/new` runs `/new` and not `/newline`.
///
/// Extensions contribute nothing here — see [`matching_with`].
#[must_use]
pub fn matching(typed: &str) -> Vec<Entry<'static>> {
    matching_with(typed, &[])
}

/// [`matching`], over the built-in table **and** what extensions contributed.
///
/// Built-ins first, then contributions in the order the host reported them —
/// an extension cannot push its entry above `/quit` by naming it `/aaa`. An
/// exact match still sorts to the top within that order, which is what makes
/// typing a full name and pressing enter reach the command you typed.
///
/// A contributed name that collides with a built-in is listed too, under its
/// own extension: hiding it would leave a client showing a command the
/// extension believes it offers, and the source column says which is which.
#[must_use]
pub fn matching_with<'a>(typed: &str, contributed: &'a [Contributed]) -> Vec<Entry<'a>> {
    let mut found: Vec<Entry<'a>> = COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(typed))
        .map(|c| Entry {
            name: c.name,
            summary: c.summary,
            availability: c.availability,
            source: Source::BuiltIn,
        })
        .collect();
    let built_in = found.len();
    found.extend(
        contributed
            .iter()
            .filter(|c| slashed_starts_with(&c.name, typed))
            .map(|c| Entry {
                name: c.name.as_str(),
                summary: c.summary.as_str(),
                // A contribution is offered because an extension is loaded and
                // said so, which is the same condition as it being able to run.
                availability: Availability::Ready,
                source: Source::Extension(c.extension.as_str()),
            }),
    );
    // Stable, so the two groups keep their order and only an exact match moves.
    found[..built_in].sort_by_key(|c| usize::from(c.name != typed));
    found[built_in..].sort_by_key(|c| usize::from(!slashed_equals(c.name, typed)));
    found
}

/// Whether `/name` starts with what was typed. Contributions are stored
/// without the slash the menu shows, so the comparison adds it rather than
/// each caller remembering to.
fn slashed_starts_with(name: &str, typed: &str) -> bool {
    typed
        .strip_prefix('/')
        .is_some_and(|rest| name.starts_with(rest))
}

/// Whether `/name` is exactly what was typed.
fn slashed_equals(name: &str, typed: &str) -> bool {
    typed.strip_prefix('/').is_some_and(|rest| name == rest)
}

#[cfg(test)]
mod tests {
    use super::{matching, matching_with, Availability, Contributed, Source, COMMANDS};

    fn contributed(name: &str) -> Contributed {
        Contributed {
            extension: "interceptor.system".to_owned(),
            name: name.to_owned(),
            summary: "what it does".to_owned(),
        }
    }

    #[test]
    fn a_contributed_command_is_offered_beside_the_built_ins() {
        let list = [contributed("prompt")];
        let found = matching_with("/", &list);
        assert_eq!(found.len(), COMMANDS.len() + 1);
        let contribution = found.last().expect("a contributed entry");
        assert_eq!(contribution.name, "prompt");
        assert_eq!(contribution.source, Source::Extension("interceptor.system"));
    }

    /// An extension must not be able to take the top of the menu by naming
    /// its command `/aaa`: built-ins come first, whatever it is called.
    #[test]
    fn a_contribution_cannot_outrank_a_built_in() {
        let list = [contributed("aaa")];
        let found = matching_with("/", &list);
        assert_eq!(
            found.first().map(|entry| entry.name),
            Some(COMMANDS[0].name),
            "the built-in table still leads the menu"
        );
    }

    #[test]
    fn typing_a_contributed_name_filters_to_it() {
        let list = [contributed("prompt")];
        let found = matching_with("/pro", &list);
        assert_eq!(
            found.iter().map(|entry| entry.name).collect::<Vec<_>>(),
            vec!["prompt"],
            "no built-in starts with `/pro`, so only the contribution matches"
        );
    }

    /// Hiding a collision would leave a client showing no sign of a command
    /// its extension believes it offers; the source is what tells them apart.
    #[test]
    fn a_contribution_that_collides_with_a_built_in_is_still_listed() {
        let list = [contributed("help")];
        let found = matching_with("/help", &list);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].source, Source::BuiltIn);
        assert_eq!(found[1].source, Source::Extension("interceptor.system"));
    }

    /// A contribution is offered because its extension is loaded and said so,
    /// which is the same condition as being able to run it.
    #[test]
    fn a_contribution_is_always_ready() {
        let list = [contributed("prompt")];
        let found = matching_with("/", &list);
        assert_eq!(
            found.last().map(|entry| entry.availability),
            Some(Availability::Ready)
        );
    }

    #[test]
    fn matching_without_contributions_is_the_built_in_table() {
        assert_eq!(matching("/").len(), COMMANDS.len());
    }

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
