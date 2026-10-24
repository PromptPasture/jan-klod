//! Host-side mirror of `wit/client-surface.wit`; strings are attacker-influenced and unsanitised, so a renderer must escape.

/// One argument a contributed command takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argument {
    /// Machine name, unique within the command.
    pub name: String,
    /// One line, shown to the user.
    pub description: String,
    /// Whether the command can run without it.
    pub required: bool,
}

/// Something a user can invoke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// Machine name, unique within the extension.
    pub name: String,
    /// Short label.
    pub title: String,
    /// One line of help.
    pub description: String,
    /// In declaration order.
    pub arguments: Vec<Argument>,
}

/// A short piece of state an extension wants visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusItem {
    /// Machine name, unique within the extension.
    pub name: String,
    /// The text to show.
    pub text: String,
    /// One line of detail.
    pub detail: String,
}

/// One field of a contributed form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Machine name, unique within the form.
    pub name: String,
    /// Label.
    pub label: String,
    /// Empty means free text; non-empty means choose one.
    pub options: Vec<String>,
    /// Used when the client cannot prompt.
    pub default_value: String,
}

/// A small set of fields gathered in one go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    /// Machine name, unique within the extension.
    pub name: String,
    /// Short label.
    pub title: String,
    /// In declaration order.
    pub fields: Vec<Field>,
}

/// Everything one extension contributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contributions {
    /// Instance id, e.g. `interceptor.system`; set by the host since a guest cannot be trusted to name itself.
    pub extension: String,
    /// Commands, in declaration order.
    pub commands: Vec<Command>,
    /// Status items, in declaration order.
    pub status_items: Vec<StatusItem>,
    /// Forms, in declaration order.
    pub forms: Vec<Form>,
}

impl Contributions {
    /// Whether this extension declared nothing at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.commands.is_empty() && self.status_items.is_empty() && self.forms.is_empty()
    }
}

/// An argument as the client supplied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgumentValue {
    /// The declared argument's name.
    pub name: String,
    /// What the user gave.
    pub value: String,
}

/// What an invocation produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvokeOutcome {
    /// Shown to the user.
    pub text: String,
    /// Whether the contribution set changed, so the host re-reads it.
    pub contributions_changed: bool,
}

/// Why an invocation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvokeError {
    /// No such extension or contribution; one case so a client cannot probe which extensions are loaded.
    Unknown,
    /// A required argument was missing, or a value was unusable.
    InvalidArguments,
    /// The extension could not carry it out, or the call trapped.
    Failed(String),
}

impl std::fmt::Display for InvokeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => f.write_str("no extension contributes that"),
            Self::InvalidArguments => f.write_str("the arguments were not usable"),
            Self::Failed(why) => write!(f, "the extension could not run it: {why}"),
        }
    }
}

impl std::error::Error for InvokeError {}
