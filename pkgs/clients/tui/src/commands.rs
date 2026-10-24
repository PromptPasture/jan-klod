//! The `/` command table: one list drives both the menu and the help overlay.
//!
//! Unavailable commands stay listed with their blocker. `Ready` means the
//! command acts, not that it needs no transport; this module never knows about
//! transport.

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

/// The commands, in menu order: by how often they are wanted, not alphabetically.
pub const COMMANDS: [Command; 6] = [
    Command {
        name: "/newline",
        summary: "insert a line break",
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
        availability: Availability::Ready,
    },
    Command {
        name: "/sessions",
        summary: "switch session",
        availability: Availability::Ready,
    },
    Command {
        name: "/help",
        summary: "show the key bindings",
        availability: Availability::Ready,
    },
    Command {
        name: "/quit",
        summary: "leave jan-klod",
        availability: Availability::Ready,
    },
];

/// A command an extension contributed. Owned, unlike [`Command`], since contributions arrive at runtime.
///
/// Strings are stored already made inert ([`crate::untrusted::inert`]), so no renderer must remember to sanitize them.
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

/// One row of the menu, from either source. Borrowed, so contributions are not cloned per keystroke.
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

/// The built-in commands starting with `typed`, exact match first. Extensions are added by [`matching_with`].
#[must_use]
pub fn matching(typed: &str) -> Vec<Entry<'static>> {
    matching_with(typed, &[])
}

/// [`matching`] over the built-ins and contributions. Built-ins always come first, so an extension cannot outrank `/quit`.
///
/// A contribution that collides with a built-in is still listed, under its own extension.
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
                availability: Availability::Ready,
                source: Source::Extension(c.extension.as_str()),
            }),
    );
    found[..built_in].sort_by_key(|c| usize::from(c.name != typed));
    found[built_in..].sort_by_key(|c| usize::from(!slashed_equals(c.name, typed)));
    found
}

/// Whether `/name` starts with `typed`. Contributions are stored without the slash, so it is added here.
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

    /// An extension cannot take the top of the menu by naming its command `/aaa`.
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

    #[test]
    fn a_contribution_that_collides_with_a_built_in_is_still_listed() {
        let list = [contributed("help")];
        let found = matching_with("/help", &list);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].source, Source::BuiltIn);
        assert_eq!(found[1].source, Source::Extension("interceptor.system"));
    }

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

    /// A pending command must state its blocker, or listing it is worse than omitting it.
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

    /// Pins which commands act.
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
