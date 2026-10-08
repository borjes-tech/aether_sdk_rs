//! Notifications: something people should see now if they are here, and later if not.
//!
//! The kernel stores the notification in the organization, then pushes it to whoever is
//! connected. People get it in their notification list and as a toast.
//!
//! Needs the capability `notify::send`; sending to [`Audience::Everyone`] (anonymous
//! visitors included) also needs `notify::public`.
//!
//! ```no_run
//! use aether_sdk::notify::{Audience, Level, Notification};
//!
//! # fn demo() -> aether_sdk::Result<()> {
//! Notification::new("New message in #general")
//!     .body("Ann: lunch?")
//!     .link("/chat/general")
//!     .level(Level::Info)
//!     .to(Audience::Members)
//!     .send()?;
//! # Ok(()) }
//! ```

use serde::Serialize;
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::transport::call;

/// How a notification is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Level {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// Who receives a notification.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Audience {
    /// The organization's logged-in members. The default.
    #[default]
    Members,
    /// Members and anonymous visitors. Needs `notify::public`.
    Everyone,
    /// Whoever made the call being served: how a public page notifies its own visitor.
    Caller,
    /// Specific people: user ids (`users:…`) and visitor ids (`visitors:…`).
    Actors(Vec<String>),
}

impl Audience {
    fn to_json(&self) -> Value {
        match self {
            Self::Members => json!("members"),
            Self::Everyone => json!("everyone"),
            Self::Caller => json!("caller"),
            Self::Actors(actors) => json!({ "actors": actors }),
        }
    }
}

/// A notification being built.
#[derive(Debug, Clone)]
pub struct Notification {
    title: String,
    body: Option<String>,
    link: Option<String>,
    level: Level,
    payload: Option<Value>,
    audience: Audience,
    expires_in_secs: Option<u64>,
}

impl Notification {
    /// A notification with a `title` (1 to 200 bytes), for the organization's members.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: None,
            link: None,
            level: Level::Info,
            payload: None,
            audience: Audience::Members,
            expires_in_secs: None,
        }
    }

    /// A line of detail (at most 2000 bytes).
    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// An app path opened when the notification is clicked, such as `/chat/general`. It
    /// must start with a single `/`: a notification cannot send people to another site.
    pub fn link(mut self, link: impl Into<String>) -> Self {
        self.link = Some(link.into());
        self
    }

    pub fn level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    /// Data for the page that receives it: a JSON object of at most 16 KiB.
    pub fn payload(mut self, payload: &impl Serialize) -> Result<Self> {
        self.payload = Some(serde_json::to_value(payload)?);
        Ok(self)
    }

    pub fn to(mut self, audience: Audience) -> Self {
        self.audience = audience;
        self
    }

    /// Stop showing it after this many seconds.
    pub fn expires_in_secs(mut self, seconds: u64) -> Self {
        self.expires_in_secs = Some(seconds);
        self
    }

    /// Store it and push it to whoever is connected. Returns the notification's id.
    pub fn send(self) -> Result<String> {
        send(&self)
    }
}

/// Store `notification` and push it to whoever is connected. Returns its id.
pub fn send(notification: &Notification) -> Result<String> {
    let mut request = json!({
        "title": notification.title,
        "level": notification.level.as_str(),
        "audience": notification.audience.to_json(),
    });
    if let Some(body) = &notification.body {
        request["body"] = json!(body);
    }
    if let Some(link) = &notification.link {
        request["link"] = json!(link);
    }
    if let Some(payload) = &notification.payload {
        request["payload"] = payload.clone();
    }
    if let Some(seconds) = notification.expires_in_secs {
        request["expires_in_secs"] = json!(seconds);
    }
    let reply = call("notify::send", &request)?;
    reply
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::Kernel("the kernel did not return the notification's id".into()))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::testing::MockKernel;

    #[test]
    fn a_notification_is_sent_with_everything_set() {
        let kernel = MockKernel::new().reply("notify::send", |_| json!({ "ok": true, "id": "01ABC" }));
        let _installed = kernel.install();
        let id = Notification::new("Hello")
            .body("World")
            .link("/chat")
            .level(Level::Success)
            .payload(&json!({ "channel": "general" }))
            .unwrap()
            .to(Audience::Actors(vec!["users:ann".into()]))
            .expires_in_secs(60)
            .send()
            .unwrap();
        assert_eq!(id, "01ABC");
        assert_eq!(
            kernel.calls()[0].1,
            json!({
                "title": "Hello", "level": "success", "body": "World", "link": "/chat",
                "payload": { "channel": "general" }, "expires_in_secs": 60,
                "audience": { "actors": ["users:ann"] }
            })
        );
    }

    #[test]
    fn the_audiences_are_named_the_way_the_kernel_expects() {
        assert_eq!(Audience::Members.to_json(), json!("members"));
        assert_eq!(Audience::Everyone.to_json(), json!("everyone"));
        assert_eq!(Audience::Caller.to_json(), json!("caller"));
    }

    #[test]
    fn sending_to_everyone_without_the_capability_is_refused() {
        let kernel = MockKernel::new().fail("notify::send", "capability", "plugin does not hold `notify::public`");
        let _installed = kernel.install();
        let result = Notification::new("Hi").to(Audience::Everyone).send();
        assert!(matches!(result, Err(Error::Capability(_))));
    }
}
