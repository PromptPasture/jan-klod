//! The one keybinding table. The status bar hint and the help overlay both derive from it.

/// The state a binding applies in. Mirrors [`crate::app::Turn`], plus `DialogClosed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    /// Nothing running. The composer's text is a new message.
    Idle,
    /// A turn is running and text is arriving.
    Streaming,
    /// A cancel has been asked for.
    Cancelling,
    /// A confirmation is waiting and its dialog is open.
    DialogOpen,
    /// A confirmation is waiting and its dialog was closed with `Esc`.
    DialogClosed,
}

/// One row of [`BINDINGS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// The key or chord, e.g. `"Ctrl+C"`. Empty for a state row, which [`hint`] renders as the action alone.
    pub keys: &'static str,
    /// What the key does, in the words the status bar shows.
    pub action: &'static str,
    /// Which state this binding applies in.
    pub context: Context,
}

/// One row per binding, in status-bar order. The only text the status bar and help overlay may show.
pub const BINDINGS: &[Binding] = &[
    Binding {
        keys: "Enter",
        action: "send",
        context: Context::Idle,
    },
    Binding {
        keys: "Esc",
        action: "quit",
        context: Context::Idle,
    },
    // One row per dialog is enough to make its key discoverable.
    Binding {
        keys: "Ctrl+S",
        action: "switch session",
        context: Context::Idle,
    },
    Binding {
        keys: "?",
        action: "help",
        context: Context::Idle,
    },
    Binding {
        keys: "Ctrl+D",
        action: "quit",
        context: Context::Idle,
    },
    // Only useful where the layout hides the sidebar pane (narrow widths).
    Binding {
        keys: "Ctrl+B",
        action: "sidebar (narrow widths)",
        context: Context::Idle,
    },
    Binding {
        keys: "Enter",
        action: "steer",
        context: Context::Streaming,
    },
    Binding {
        keys: "Ctrl+C",
        action: "cancel",
        context: Context::Streaming,
    },
    Binding {
        keys: "",
        action: "stopping — Ctrl+C already sent",
        context: Context::Cancelling,
    },
    Binding {
        keys: "↑/↓ or 1-9",
        action: "choose",
        context: Context::DialogOpen,
    },
    Binding {
        keys: "Enter",
        action: "confirm",
        context: Context::DialogOpen,
    },
    Binding {
        keys: "Esc",
        action: "close",
        context: Context::DialogOpen,
    },
    Binding {
        keys: "Enter",
        action: "reopen the prompt",
        context: Context::DialogClosed,
    },
    Binding {
        keys: "Esc",
        action: "quit",
        context: Context::DialogClosed,
    },
];

/// The hint for `context` from `table`, joined with " · ". Takes the table so tests can substitute one.
#[must_use]
pub fn hint(table: &[Binding], context: Context) -> String {
    table
        .iter()
        .filter(|b| b.context == context)
        .map(|b| {
            if b.keys.is_empty() {
                b.action.to_string()
            } else {
                format!("{} to {}", b.keys, b.action)
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The first matching binding only, for narrow widths.
#[must_use]
pub fn hint_narrow(table: &[Binding], context: Context) -> String {
    table
        .iter()
        .find(|b| b.context == context)
        .map_or_else(String::new, |b| {
            if b.keys.is_empty() {
                b.action.to_string()
            } else {
                format!("{} to {}", b.keys, b.action)
            }
        })
}

/// [`hint`] against [`BINDINGS`] — what every real caller wants.
#[must_use]
pub fn status_hint(context: Context) -> String {
    hint(BINDINGS, context)
}

/// [`hint_narrow`] against [`BINDINGS`].
#[must_use]
pub fn status_hint_narrow(context: Context) -> String {
    hint_narrow(BINDINGS, context)
}

#[cfg(test)]
mod tests {
    use super::{hint, hint_narrow, status_hint, Binding, Context, BINDINGS};

    /// Editing a table entry must move the hint, since the hint derives from the table.
    #[test]
    fn the_hint_changes_when_the_table_entry_changes() {
        const ORIGINAL: &[Binding] = &[Binding {
            keys: "Enter",
            action: "send",
            context: Context::Idle,
        }];
        const CHANGED: &[Binding] = &[Binding {
            keys: "Enter",
            action: "submit",
            context: Context::Idle,
        }];

        let before = hint(ORIGINAL, Context::Idle);
        let after = hint(CHANGED, Context::Idle);
        assert_ne!(before, after, "editing the table did not move the hint");
        assert!(after.contains("submit"), "{after:?}");
        assert!(!after.contains("send"), "{after:?}");
    }

    #[test]
    fn a_key_with_no_action_word_renders_alone() {
        assert_eq!(
            hint(BINDINGS, Context::Cancelling),
            "stopping — Ctrl+C already sent"
        );
    }

    #[test]
    fn every_context_the_status_bar_uses_has_at_least_one_binding() {
        for context in [
            Context::Idle,
            Context::Streaming,
            Context::Cancelling,
            Context::DialogOpen,
            Context::DialogClosed,
        ] {
            assert!(
                !status_hint(context).is_empty(),
                "{context:?} has nothing to show in the status bar"
            );
        }
    }

    #[test]
    fn the_narrow_hint_is_a_prefix_of_the_wide_one() {
        for context in [Context::Idle, Context::Streaming, Context::DialogOpen] {
            let wide = status_hint(context);
            let narrow = hint_narrow(BINDINGS, context);
            assert!(
                wide.starts_with(&narrow),
                "{context:?}: {narrow:?} is not the start of {wide:?}"
            );
            assert!(narrow.len() <= wide.len());
        }
    }
}
