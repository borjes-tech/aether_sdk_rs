//! Calling outside web APIs.
//!
//! A plugin has no network of its own: the kernel makes the request, and only to hosts the
//! plugin lists in `plugin.toml`:
//!
//! ```toml
//! http_hosts = ["api.example.com", "*.example.org"]   # a wildcard covers subdomains only
//! ```
//!
//! The kernel also refuses addresses on private or internal networks, does not follow
//! redirects (you get the `3xx` answer and its `location`), allows `https` only, and limits
//! requests and answers to 1 MiB and the wait to 30 seconds (10 by default).
//!
//! A `404` or `500` from the other side is an ordinary [`Response`], not an error; an
//! [`Error`](crate::Error) means the request was refused or could not be made.
//!
//! Needs the capability `http::request`.
//!
//! ```no_run
//! use aether_sdk::{http, json, Result};
//!
//! fn charge() -> Result<String> {
//!     let answer = http::post("https://api.example.com/charges")
//!         .header("authorization", "Bearer ...")
//!         .json(&json!({ "amount": 500 }))
//!         .send()?;
//!     if !answer.ok {
//!         return Err(aether_sdk::Error::msg("the payment service refused it"));
//!     }
//!     Ok(answer.text().unwrap_or_default().to_string())
//! }
//! ```

use std::collections::HashMap;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::transport::call;

/// A request under construction. Nothing is sent until [`Request::send`].
#[derive(Debug, Clone)]
pub struct Request {
    payload: Map<String, Value>,
}

/// A GET request to `url`.
pub fn get(url: &str) -> Request {
    Request::new("GET", url)
}

/// A POST request to `url`.
pub fn post(url: &str) -> Request {
    Request::new("POST", url)
}

/// A PUT request to `url`.
pub fn put(url: &str) -> Request {
    Request::new("PUT", url)
}

/// A PATCH request to `url`.
pub fn patch(url: &str) -> Request {
    Request::new("PATCH", url)
}

/// A DELETE request to `url`.
pub fn delete(url: &str) -> Request {
    Request::new("DELETE", url)
}

impl Request {
    /// A request with any of GET, POST, PUT, PATCH, DELETE or HEAD.
    pub fn new(method: &str, url: &str) -> Self {
        let mut payload = Map::new();
        payload.insert("method".into(), json!(method));
        payload.insert("url".into(), json!(url));
        Self { payload }
    }

    /// Add a header. The kernel sets `host`, `content-length` and the like itself.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        let headers = self.payload.entry("headers").or_insert_with(|| json!({}));
        headers[name] = json!(value);
        self
    }

    /// Send `value` as the JSON body (and `content-type: application/json` unless you set one).
    #[must_use]
    pub fn json(mut self, value: &impl Serialize) -> Self {
        self.payload.insert("json".into(), serde_json::to_value(value).unwrap_or(Value::Null));
        self
    }

    /// Send `text` as the body.
    #[must_use]
    pub fn body(mut self, text: &str) -> Self {
        self.payload.insert("body".into(), json!(text));
        self
    }

    /// Send raw bytes as the body.
    #[must_use]
    pub fn bytes(mut self, bytes: &[u8]) -> Self {
        self.payload.insert("base64".into(), json!(STANDARD.encode(bytes)));
        self
    }

    /// How long to wait for an answer, 1 to 30 seconds (10 when not set).
    #[must_use]
    pub fn timeout_secs(mut self, seconds: u64) -> Self {
        self.payload.insert("timeout_secs".into(), json!(seconds));
        self
    }

    /// Make the request.
    pub fn send(self) -> Result<Response> {
        let reply = call("http::request", &Value::Object(self.payload))?;
        Ok(serde_json::from_value(reply["data"].clone())?)
    }
}

/// The other side's answer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Response {
    /// The HTTP status code.
    pub status: u16,
    /// Whether the status is a success (2xx).
    pub ok: bool,
    /// Header names are lower case; a header sent twice keeps its last value.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    body: Option<String>,
    body_base64: Option<String>,
}

impl Response {
    /// The body as text, when it is text.
    pub fn text(&self) -> Option<&str> {
        self.body.as_deref()
    }

    /// The body parsed as JSON.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        let text = self.body.as_deref().ok_or_else(|| Error::Kernel("the answer has no text body".into()))?;
        Ok(serde_json::from_str(text)?)
    }

    /// The body as bytes, whatever it holds.
    pub fn bytes(&self) -> Vec<u8> {
        match (&self.body, &self.body_base64) {
            (Some(text), _) => text.clone().into_bytes(),
            (None, Some(encoded)) => STANDARD.decode(encoded).unwrap_or_default(),
            (None, None) => Vec::new(),
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::testing::MockKernel;

    #[test]
    fn a_request_is_described_to_the_kernel() {
        let kernel = MockKernel::new().reply("http::request", |_| {
            json!({ "ok": true, "data": { "status": 201, "ok": true, "headers": { "x-id": "7" }, "body": "{\"id\":7}" } })
        });
        let _installed = kernel.install();
        let answer = post("https://api.example.com/things")
            .header("authorization", "Bearer k")
            .json(&json!({ "a": 1 }))
            .timeout_secs(5)
            .send()
            .unwrap();
        assert_eq!(answer.status, 201);
        assert!(answer.ok);
        assert_eq!(answer.headers["x-id"], "7");
        assert_eq!(answer.json::<Value>().unwrap(), json!({ "id": 7 }));
        assert_eq!(
            kernel.calls()[0].1,
            json!({
                "method": "POST", "url": "https://api.example.com/things",
                "headers": { "authorization": "Bearer k" }, "json": { "a": 1 }, "timeout_secs": 5
            })
        );
    }

    #[test]
    fn an_unsuccessful_status_is_an_answer_not_an_error() {
        let kernel = MockKernel::new()
            .reply("http::request", |_| json!({ "ok": true, "data": { "status": 404, "ok": false, "headers": {}, "body": "nope" } }));
        let _installed = kernel.install();
        let answer = get("https://api.example.com/x").send().unwrap();
        assert!(!answer.ok);
        assert_eq!(answer.text(), Some("nope"));
    }

    #[test]
    fn binary_answers_and_bodies_use_base64() {
        let kernel = MockKernel::new().reply("http::request", |_| {
            json!({ "ok": true, "data": { "status": 200, "ok": true, "headers": {}, "body_base64": "AAEC" } })
        });
        let _installed = kernel.install();
        let answer = put("https://api.example.com/x").bytes(&[9, 9]).send().unwrap();
        assert_eq!(answer.text(), None);
        assert_eq!(answer.bytes(), vec![0, 1, 2]);
        assert_eq!(kernel.calls()[0].1["base64"], "CQk=");
    }

    #[test]
    fn a_refused_host_is_an_error() {
        let kernel = MockKernel::new().fail(
            "http::request",
            "error",
            "this plugin may not call `evil.example.net`; list it under `http_hosts` in plugin.toml",
        );
        let _installed = kernel.install();
        assert!(matches!(get("https://evil.example.net/").send(), Err(Error::Kernel(_))));
    }
}
