//! Who is calling.
//!
//! Needs no capability: it only describes the request the plugin is already serving.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::Result;
use crate::transport::call;

/// The kind of caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// A logged-in member of the organization.
    User,
    /// An anonymous visitor the kernel has given an identity to.
    Visitor,
    /// Nobody identified (a public page opened with no visitor identity yet).
    Anonymous,
    /// The kernel itself: an event handler, a scheduled job. Rules are not applied to it.
    System,
}

/// Who made the call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub kind: ActorKind,
    /// `users:…` or `visitors:…`; none for [`ActorKind::Anonymous`].
    pub id: Option<String>,
}

/// The call being served.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    /// This plugin's name.
    pub plugin: String,
    /// The exported function that was called.
    pub function: String,
    /// The organization's database name.
    pub organization: String,
    /// Ties together everything one request did in the audit trail.
    pub request_id: String,
    pub actor: Actor,
    /// When the request started, as an RFC 3339 UTC timestamp. A plugin has no clock of its own
    /// (WebAssembly cannot read one), so this is how it learns the time.
    #[serde(default)]
    pub now: String,
    /// The roles an administrator gave the caller, such as `hr.hr_manager`. Empty for visitors.
    #[serde(default)]
    pub roles: Vec<String>,
}

impl Context {
    /// Today's date in UTC as `YYYY-MM-DD`; empty when the kernel did not say.
    pub fn today(&self) -> &str {
        self.now.get(..10).unwrap_or("")
    }

    /// Whether the caller holds `role` (written `<plugin>.<name>`). An administrator of the
    /// organization (`org_admin`) holds every role.
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|held| held == role || held == "org_admin")
    }

    /// Fail with a message for the caller unless they hold `role`.
    pub fn require_role(&self, role: &str) -> Result<()> {
        if self.has_role(role) {
            Ok(())
        } else {
            Err(crate::Error::msg(format!("you need the role `{role}` to do this")))
        }
    }

    /// The caller is a logged-in member of the organization.
    pub fn is_member(&self) -> bool {
        self.actor.kind == ActorKind::User
    }
}

/// The context of the call being served.
pub fn current() -> Result<Context> {
    Ok(serde_json::from_value(call("context::get", &json!({}))?)?)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::testing::MockKernel;

    #[test]
    fn reads_who_is_calling() {
        let kernel = MockKernel::new().reply("context::get", |_| {
            json!({
                "ok": true, "plugin": "chat", "function": "post", "organization": "acme",
                "request_id": "r1", "actor": { "kind": "visitor", "id": "visitors:v1" }
            })
        });
        let _installed = kernel.install();
        let context = current().unwrap();
        assert_eq!(context.actor, Actor { kind: ActorKind::Visitor, id: Some("visitors:v1".into()) });
        assert!(!context.is_member());
        assert_eq!(context.organization, "acme");
    }

    #[test]
    fn an_administrator_holds_every_role() {
        let mut context = Context {
            plugin: "hr".into(),
            function: "f".into(),
            organization: "acme".into(),
            request_id: "r".into(),
            actor: Actor { kind: ActorKind::User, id: Some("users:a".into()) },
            now: String::new(),
            roles: vec!["hr.hr_manager".into()],
        };
        assert!(context.has_role("hr.hr_manager"));
        assert!(!context.has_role("hr_leave.approver"));
        assert!(context.require_role("hr_leave.approver").is_err());
        context.roles = vec!["org_admin".into()];
        assert!(context.has_role("anything.at_all"));
        assert!(context.require_role("hr.hr_manager").is_ok());
    }
}
