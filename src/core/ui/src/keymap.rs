//! The one keybinding table (#97, #104).
//!
//! Modelled on [`crate::commands`]: a `const` array that renders both the
//! status bar's hint and — from 19h
//! ([#105](https://github.com/PromptPasture/jan-klod/issues/105)) — a help
//! overlay, so the two cannot disagree with each other or with what a keypress
//! actually does.
//!
//! [`hint`] is a function of a table and a [`Context`], not of [`BINDINGS`]
//! alone, precisely so a test can hand it a different table and watch the
//! string change — the shape #104's Acceptance line 3 asks for: "the hint for a
//! binding changes when the table entry changes, so a hand-written duplicate
//! would fail". `crate::tui::status_hint` calls [`status_hint`], which is the
//! one place that closes over [`BINDINGS`].

/// Which state a binding applies in.
///
/// Four of the five mirror [`crate::app::Turn`]; [`Self::DialogClosed`] is the
/// fifth — a prompt is pending but its modal was dismissed with `Esc`, which
/// reads keys differently than either an open dialog or a plain idle turn.
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
    /// The key or chord, as shown to a user — e.g. `"Ctrl+C"`.
    ///
    /// Empty for a row that names a state rather than a key (`Cancelling`'s
    /// only row: nothing is pressed to *reach* it, it is already true). [`hint`]
    /// renders such a row as [`Binding::action`] alone.
    pub keys: &'static str,
    /// What the key does, in the words the status bar shows.
    pub action: &'static str,
    /// Which state this binding applies in.
    pub context: Context,
}

/// One row per binding, in the order the status bar shows them.
///
/// This is the whole of what the status bar and (19h) the help overlay may
/// say about a keybinding — see the module docs for why that is the point.
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

/// Render the hint string `table` offers for `context` — every entry whose
/// [`Binding::context`] matches, in table order, joined the way the status bar
/// has always shown them.
///
/// Takes `table` as an argument rather than reading [`BINDINGS`] directly, so
/// a test can substitute a different table and observe the difference: that is
/// what proves the status bar's hint is *produced from* the table rather than
/// a string that merely resembles it.
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

/// The narrow form of [`hint`] — the first binding only, for #104's 60–79
/// width band ("fewer status hints"). Still produced from `table`, for the
/// same reason [`hint`] is.
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

    /// #104's Acceptance line 3, driven exactly as it is worded: change a
    /// table entry and observe the hint change. A hand-written duplicate of
    /// the status line would pass this table's own test and fail nothing —
    /// which is why the assertion is against [`hint`] itself, called with a
    /// table this test controls, not against a literal the status bar happens
    /// to also produce.
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
