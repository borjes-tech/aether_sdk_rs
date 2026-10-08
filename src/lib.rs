//! # Aether SDK for Rust
//!
//! Write an Aether plugin as plain Rust. The kernel's functions are ordinary functions
//! here, so you never deal with Extism, memory offsets or JSON envelopes.
//!
//! ```ignore
//! use aether_sdk::prelude::*;
//!
//! #[derive(Serialize, Deserialize)]
//! struct Message { id: Option<String>, channel: String, body: String }
//!
//! impl Model for Message { const MODEL: &'static str = "message"; }
//!
//! #[derive(Deserialize)]
//! struct Post { channel: String, body: String }
//!
//! handler! {
//!     /// Exported as `post_message`; the page calls it.
//!     fn post_message(post: Post) -> Message {
//!         let who = context::current()?;
//!         let message = Message { id: None, channel: post.channel, body: post.body }.create()?;
//!         Notification::new("New message").link("/chat").to(Audience::Members).send()?;
//!         log::info(format!("{:?} posted", who.actor.id));
//!         Ok(message)
//!     }
//! }
//! ```
//!
//! | Module | What it gives you | Capability in `plugin.toml` |
//! |---|---|---|
//! | [`db`] | get, find, create, update, delete, increment and atomic transactions on your models | `db::query`, `db::mutate`, `db::transaction` |
//! | [`cache`] | a private key/value cache with expiry | `cache::get`, `cache::set`, `cache::invalidate`, `cache::clear` |
//! | [`storage`] | your own files (local or S3, you never know which) | `storage::read`, `storage::write`, `storage::delete`, `storage::list` |
//! | [`http`] | calls to outside web APIs, to hosts you list in `http_hosts` | `http::request` |
//! | [`communication`] | email, SMS and other messages, through the provider the administrator configured | `communication::send` plus `email::send` / `sms::send` |
//! | [`scheduler`] | background jobs (like Celery's `.delay()`) and recurring tasks | `scheduler::enqueue`, `scheduler::register`, `scheduler::cancel` |
//! | [`parallel`] | run one function over many calls at once (fan out, then join) | `scheduler::enqueue` |
//! | [`files`] | the plugin's own folder on disk, and the files that arrive in it (`[[watch]]`) | `fs::read`, `fs::list`, `fs::write`, `fs::delete` |
//! | [`bridge`] | calls to integrations the administrator set up (Paystack, OpenStreetMap), without ever seeing a key | `bridge::call` (+ `bridges` in `plugin.toml`) |
//! | [`events`] | live events for members, and announcements other plugins react to | `events::emit`, `events::subscribe` |
//! | [`plugins`] | calls to other plugins' functions, those in `dependencies` | `plugins::call` |
//! | [`notify`] | stored notifications, pushed live | `notify::send` (`notify::public` for everyone) |
//! | [`events`] | transient live events | `events::emit` |
//! | [`context`] | who is calling | none |
//! | [`log`] | the server's log | none |
//!
//! There is no way to run a query of your own: data is reached through models the plugin
//! was granted, so a plugin can never touch another's data.
//!
//! Build for `wasm32-unknown-unknown` (`cargo build --release --target wasm32-unknown-unknown`,
//! with `crate-type = ["cdylib"]`). Test your logic natively with [`testing::MockKernel`].

pub mod bridge;
pub mod cache;
pub mod communication;
pub mod context;
pub mod dates;
pub mod db;
pub mod decimal;
mod error;
pub mod events;
pub mod files;
#[doc(hidden)]
pub mod handler;
pub mod http;
pub mod log;
pub mod notify;
pub mod parallel;
pub mod plugins;
pub mod records;
pub mod scheduler;
pub mod storage;
#[cfg(not(target_arch = "wasm32"))]
pub mod testing;
pub mod transport;

pub use error::{Error, Result};
pub use serde_json::{json, Value};

/// What the kernel looks for at the start of a plugin's error to know it is meant for the
/// caller. Must match `USER_ERROR_PREFIX` in the kernel's `plugin_manager::runtime`.
#[doc(hidden)]
pub const USER_ERROR_PREFIX: &str = "aether:user-error:";

/// The input of a function that takes none. Unlike `()`, it accepts whatever the caller
/// sends (nothing, `null` or `{}`) and ignores it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Empty;

impl<'de> serde::Deserialize<'de> for Empty {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer).map(|_| Self)
    }
}

/// What most plugins want in scope.
pub mod prelude {
    pub use crate::db::Model;
    pub use crate::notify::{Audience, Level, Notification};
    pub use crate::{bridge, cache, communication, context, dates, db, decimal, events, files, handler, http, log, notify, parallel, plugins, records, scheduler, storage};
    pub use crate::{json, Empty, Error, Result, Value};
    pub use serde::{Deserialize, Serialize};
}

#[doc(hidden)]
pub mod __rt {
    pub use crate::handler::run;
}

/// Export plugin functions.
///
/// Each function takes one argument, the call's JSON input as a type that implements
/// `Deserialize` (use [`Empty`] for none), and returns `Result<Output>` where the output
/// implements `Serialize`. The function's name is the name the kernel calls.
///
/// ```ignore
/// handler! {
///     fn ping(_: Empty) -> String { Ok("pong".into()) }
///
///     fn greet(who: Greeting) -> Reply { Ok(Reply { text: format!("hello {}", who.name) }) }
/// }
/// ```
///
/// An `Err` is the call's failure. Keep the real work in an ordinary function that the
/// handler calls, so you can test it natively with [`testing::MockKernel`].
#[macro_export]
macro_rules! handler {
    ($( $(#[$meta:meta])* fn $name:ident($input:tt : $in:ty) -> $out:ty $body:block )+) => {
        $(
            $(#[$meta])*
            #[no_mangle]
            pub extern "C" fn $name() -> i32 {
                fn handler($input: $in) -> $crate::Result<$out> $body
                $crate::__rt::run(handler)
            }
        )+
    };
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn empty_accepts_any_input() {
        for input in ["null", "{}", "[]", "{\"ignored\": 1}", "\"text\""] {
            assert_eq!(serde_json::from_str::<Empty>(input).ok(), Some(Empty), "{input}");
        }
    }
}
