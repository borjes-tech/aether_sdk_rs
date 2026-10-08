//! Sending messages: email, SMS and whatever else the kernel can route.
//!
//! One function does it all. You say what **type** of message it is and what is in it; the kernel
//! sends it through whichever provider the administrator configured for that type (SMTP or Resend
//! for email; Twilio, Termii or Africa's Talking for SMS) with that organization's credentials. A
//! plugin never names a provider, never holds a key and never chooses the sender.
//!
//! Sending is **queued**: this returns as soon as the message is accepted, and the scheduler
//! delivers it in the background, retrying if the provider is down. One job is made per recipient,
//! so recipients never see each other and a retry never re-sends to someone already reached.
//! The answer lists the job ids ([`Sent::jobs`]); they are for tracing, not something to wait on.
//!
//! Needs the capability `communication::send` **and** the capability of the type: `email::send`
//! for email, `sms::send` for SMS.
//!
//! ```no_run
//! use aether_sdk::{communication, Result};
//!
//! fn welcome(address: &str, phone: &str) -> Result<()> {
//!     communication::email([address])
//!         .subject("Welcome")
//!         .text("Thanks for joining.")
//!         .send()?;
//!     communication::sms([phone]).text("Welcome! Your code is 1234").send()?;
//!     Ok(())
//! }
//! ```

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::error::Result;
use crate::transport::call;

/// What the kernel accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    /// One queued job per recipient.
    pub jobs: Vec<String>,
}

/// Send a message described as JSON: `{ "type": "email", "to": ..., ... }`. Use this for a type
/// the SDK has no builder for yet; [`email`] and [`sms`] are friendlier for the usual ones.
pub fn send(message: &impl Serialize) -> Result<Sent> {
    let reply = call("communication::send", &serde_json::to_value(message)?)?;
    let jobs = reply["data"]["jobs"]
        .as_array()
        .map(|jobs| jobs.iter().filter_map(|job| job.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Ok(Sent { jobs })
}

fn recipients<I, S>(to: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    to.into_iter().map(Into::into).collect()
}

/// An email to start building. Up to 50 recipients (each gets their own copy).
pub fn email<I, S>(to: I) -> Email
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    Email { to: recipients(to), subject: String::new(), text: None, html: None, reply_to: None }
}

/// An email under construction. Give it a subject and a `text` and/or `html` body.
#[derive(Debug, Clone)]
pub struct Email {
    to: Vec<String>,
    subject: String,
    text: Option<String>,
    html: Option<String>,
    reply_to: Option<String>,
}

impl Email {
    /// One line, at most 300 characters.
    #[must_use]
    pub fn subject(mut self, subject: &str) -> Self {
        self.subject = subject.to_string();
        self
    }

    /// The plain-text body.
    #[must_use]
    pub fn text(mut self, text: &str) -> Self {
        self.text = Some(text.to_string());
        self
    }

    /// The HTML body. Give a text body as well for mail programs that show only text.
    #[must_use]
    pub fn html(mut self, html: &str) -> Self {
        self.html = Some(html.to_string());
        self
    }

    /// Where replies go. The sender itself is set in the administrator's settings.
    #[must_use]
    pub fn reply_to(mut self, address: &str) -> Self {
        self.reply_to = Some(address.to_string());
        self
    }

    /// Queue it.
    pub fn send(self) -> Result<Sent> {
        let mut message = Map::new();
        message.insert("type".into(), json!("email"));
        message.insert("to".into(), json!(self.to));
        message.insert("subject".into(), json!(self.subject));
        for (name, value) in [("text", self.text), ("html", self.html), ("reply_to", self.reply_to)] {
            if let Some(value) = value {
                message.insert(name.into(), json!(value));
            }
        }
        send(&Value::Object(message))
    }
}

/// An SMS to start building. Numbers are in international form (`+2348012345678`); up to 50.
pub fn sms<I, S>(to: I) -> Sms
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    Sms { to: recipients(to), text: String::new() }
}

/// A text message under construction.
#[derive(Debug, Clone)]
pub struct Sms {
    to: Vec<String>,
    text: String,
}

impl Sms {
    /// The message, at most 1600 characters.
    #[must_use]
    pub fn text(mut self, text: &str) -> Self {
        self.text = text.to_string();
        self
    }

    /// Queue it.
    pub fn send(self) -> Result<Sent> {
        send(&json!({ "type": "sms", "to": self.to, "text": self.text }))
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::testing::MockKernel;
    use crate::Error;

    fn kernel() -> MockKernel {
        MockKernel::new().reply("communication::send", |request| {
            let count = request["to"].as_array().map_or(1, Vec::len);
            json!({ "ok": true, "data": { "jobs": (0..count).map(|n| format!("job{n}")).collect::<Vec<_>>() } })
        })
    }

    #[test]
    fn an_email_is_one_command_with_its_type() {
        let kernel = kernel();
        let _installed = kernel.install();
        let sent = email(["ann@example.com", "bob@example.com"])
            .subject("Welcome")
            .text("Hi")
            .html("<b>Hi</b>")
            .reply_to("help@acme.test")
            .send()
            .unwrap();
        assert_eq!(sent.jobs, ["job0", "job1"]);
        assert_eq!(
            kernel.calls(),
            [(
                "communication::send".to_string(),
                json!({
                    "type": "email", "to": ["ann@example.com", "bob@example.com"], "subject": "Welcome",
                    "text": "Hi", "html": "<b>Hi</b>", "reply_to": "help@acme.test"
                })
            )]
        );
    }

    #[test]
    fn an_sms_is_the_same_command_with_another_type() {
        let kernel = kernel();
        let _installed = kernel.install();
        sms(vec!["+2348012345678".to_string()]).text("Code 1234").send().unwrap();
        assert_eq!(
            kernel.calls()[0].1,
            json!({ "type": "sms", "to": ["+2348012345678"], "text": "Code 1234" })
        );
    }

    #[test]
    fn any_other_type_goes_through_send() {
        let kernel = kernel();
        let _installed = kernel.install();
        send(&json!({ "type": "whatsapp", "to": "+2348012345678", "text": "hi" })).unwrap();
        assert_eq!(kernel.calls()[0].1["type"], "whatsapp");
    }

    #[test]
    fn the_kernels_refusals_are_errors() {
        let kernel = MockKernel::new()
            .fail("communication::send", "capability", "plugin does not hold `email::send`");
        let _installed = kernel.install();
        assert!(matches!(email(["a@x.com"]).subject("S").text("T").send(), Err(Error::Capability(_))));
        let kernel = MockKernel::new().fail("communication::send", "error", "sms: no provider is chosen in the settings");
        let _installed = kernel.install();
        assert!(matches!(sms(["+2348012345678"]).text("T").send(), Err(Error::Kernel(_))));
    }
}
