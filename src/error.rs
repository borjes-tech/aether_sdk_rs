use serde_json::Value;

/// What can go wrong when a plugin asks the kernel for something.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The plugin does not hold the capability the call needs. Declare it in `plugin.toml`.
    #[error("not allowed: {0}")]
    Capability(String),
    /// The model is not one of the plugin's `access_models`, is read-only, or belongs to the kernel.
    #[error("{0}")]
    Model(String),
    /// The request was malformed (an empty record, a field name that is not a plain identifier, ...).
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// The kernel knows the call but does not provide it yet.
    #[error("not available in this kernel: {0}")]
    NotImplemented(String),
    /// The kernel has no such command.
    #[error("unknown kernel command: {0}")]
    UnknownCommand(String),
    /// The database refused or failed the operation.
    #[error("database error: {0}")]
    Database(String),
    /// Any other error the kernel reported.
    #[error("{0}")]
    Kernel(String),
    /// A value could not be turned into, or read from, JSON.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// The call could not reach the kernel at all.
    #[error("could not reach the kernel: {0}")]
    Host(String),
    /// An error of the plugin's own: `Err(Error::msg("channel is archived"))`.
    #[error("{0}")]
    Message(String),
}

impl Error {
    /// An error of the plugin's own, which the caller sees as the call's failure.
    pub fn msg(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }

    /// Keep a message meant for the caller (a refusal by a role's rule, a plugin's own message) and
    /// replace anything else with `fallback`, which says in the plugin's words what was being done.
    #[must_use]
    pub fn or(self, fallback: impl Into<String>) -> Self {
        match self {
            Self::Message(message) => Self::Message(message),
            _ => Self::Message(fallback.into()),
        }
    }

    /// The kernel's `error` text with its `kind`, as `{ "error": "...", "kind": "..." }`.
    pub(crate) fn from_kernel(kind: &str, message: String) -> Self {
        match kind {
            "capability" => Self::Capability(message),
            "model" => Self::Model(message),
            "invalid_payload" => Self::InvalidRequest(message),
            "not_implemented" => Self::NotImplemented(message),
            "unknown_command" => Self::UnknownCommand(message),
            "db" => Self::Database(message),
            // A role's rule refused it: say so to the person who asked, in the kernel's words.
            "denied" => Self::Message(message),
            _ => Self::Kernel(message),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The kernel answers a failed command with `{ "error", "kind" }` and no `ok`; a
/// successful one never has that shape. Turn the failure into an [`Error`].
pub(crate) fn check_reply(reply: Value) -> Result<Value> {
    if let Value::Object(object) = &reply {
        if !object.contains_key("ok") {
            if let (Some(Value::String(message)), Some(Value::String(kind))) =
                (object.get("error"), object.get("kind"))
            {
                return Err(Error::from_kernel(kind, message.clone()));
            }
        }
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_kernel_failure_becomes_the_matching_error() {
        let cases = [
            ("capability", "Capability"),
            ("model", "Model"),
            ("invalid_payload", "InvalidRequest"),
            ("not_implemented", "NotImplemented"),
            ("unknown_command", "UnknownCommand"),
            ("db", "Database"),
            ("anything else", "Kernel"),
        ];
        for (kind, expected) in cases {
            let error = check_reply(json!({ "error": "why", "kind": kind })).unwrap_err();
            assert!(format!("{error:?}").starts_with(expected), "{kind}: {error:?}");
        }
    }

    #[test]
    fn a_successful_reply_is_never_mistaken_for_a_failure() {
        // A record may legitimately have fields called `error` and `kind`.
        let reply = json!({ "ok": true, "data": { "error": "x", "kind": "y" } });
        assert_eq!(check_reply(reply.clone()).unwrap(), reply);
        assert_eq!(check_reply(json!({ "ok": true })).unwrap(), json!({ "ok": true }));
    }

    #[test]
    fn or_keeps_a_message_for_the_caller_and_replaces_the_rest() {
        assert!(matches!(Error::msg("not allowed: x").or("could not"), Error::Message(m) if m == "not allowed: x"));
        assert!(matches!(Error::Database("boom".into()).or("could not save"), Error::Message(m) if m == "could not save"));
    }
}
