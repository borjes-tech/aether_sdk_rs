# Aether SDK for Rust

Write an Aether plugin as plain Rust. The kernel's functions are ordinary functions, so you
never deal with Extism, memory offsets or JSON envelopes.

```toml
# Cargo.toml of your plugin
[lib]
crate-type = ["cdylib"]

[dependencies]
aether-sdk = { path = "../../sdks/rust" }
serde = { version = "1", features = ["derive"] }
```

```rust
use aether_sdk::prelude::*;

#[derive(Serialize, Deserialize)]
struct Message { id: Option<String>, channel: String, body: String }

impl Model for Message { const MODEL: &'static str = "message"; }

#[derive(Deserialize)]
struct Post { channel: String, body: String }

handler! {
    /// Exported as `post_message`; a page or client calls it.
    fn post_message(post: Post) -> Message {
        let who = context::current()?;
        let message = Message { id: None, channel: post.channel, body: post.body }.create()?;
        Notification::new("New message").link("/chat").to(Audience::Members).send()?;
        log::info(format!("{:?} posted", who.actor.id));
        Ok(message)
    }
}
```

Build with `make` (or `cargo build --release --target wasm32-unknown-unknown`); the plugin
generator (`aether --gen plugin --plugin-language rust`) writes the `Makefile`.

## What it gives you

| Module | What | Capability in `plugin.toml` |
|---|---|---|
| `db` | `get`, `find`, `create`, `update`, `delete`, `increment` on your models (or the `Model` trait), and atomic `transaction()` batches | `db::query`, `db::mutate`, `db::transaction` |
| `cache` | a private key/value cache with expiry | `cache::get`, `cache::set`, `cache::invalidate`, `cache::clear` |
| `storage` | your own files (local disk or S3; you never know which) | `storage::read`, `storage::write`, `storage::delete`, `storage::list` |
| `http` | calls to outside web APIs | `http::request`, plus `http_hosts` in `plugin.toml` |
| `communication` | email, SMS and more through whichever provider the administrator set up; you name the type and the content | `communication::send` + `email::send` / `sms::send` |
| `scheduler` | background jobs (like Celery's `.delay()`) with retries, and recurring tasks | `scheduler::enqueue`, `scheduler::register`, `scheduler::cancel` |
| `files` | the plugin's own folder on disk, and the files that arrive in it | `fs::read`, `fs::list`, `fs::write`, `fs::delete` (+ `[[watch]]`) |
| `bridge` | integrations the administrator set up (Paystack, OpenStreetMap); you never see a key | `bridge::call` (+ `bridges` in `plugin.toml`) |
| `plugins` | calls to other plugins' functions (WebAssembly or Rhai) | `plugins::call`, plus `dependencies` in `plugin.toml` |
| `notify` | stored notifications, pushed live to whoever is connected | `notify::send` (`notify::public` to reach everyone) |
| `events` | transient live events for connected members | `events::emit` |
| `context` | who is calling, and for which organization | none |
| `log` | the server's log | none |
| `handler!` | exports your functions to the kernel | none |

### Data

You never write a query. You name a **model** that the plugin defines in `models/<name>.json` and
`plugin.toml` grants (`access_models`), and the kernel does the rest, for the organization the call
is for. You use field *names*; the database stores ids, so renaming a field in the model never
touches data. The kernel checks every value against the model and refuses a field that is not in
it, a wrong type or a missing required value with `Error::InvalidRequest`. A model the plugin was
not granted, a write to a read-only one, or one belonging to the kernel or another plugin is
refused with `Error::Model`. Every access is recorded in the audit trail with the ids of the
records involved. There is deliberately no raw SurQL.

```rust
let recent: Vec<Message> = Message::find()
    .filter("channel", "general")
    .order_by("date_created")
    .limit(20)
    .all()?;
let one = Message::get("mdl_k3v9xq2m7a:abc")?;        // Option<Message>
let edited = Message::update("mdl_k3v9xq2m7a:abc", &json!({ "body": "edited" }))?;
Message::delete("mdl_k3v9xq2m7a:abc")?;
```

Without the `Model` trait: `db::get::<T>("message", id)`, `db::find::<T>("message")`,
`db::create`, `db::update`, `db::delete`. Records come back with `id` as `"<model id>:<key>"`, and the
key alone (the part after the colon) is accepted wherever an id is. A `null` in an update clears an
optional field.

### Functions

A `handler!` function takes the call's JSON input as any `Deserialize` type (`Empty` for no
input: it accepts `null`, nothing or `{}`) and returns `Result<Output>` for any `Serialize`
type. The function's name is the name callers use: `POST /api/plugins/<plugin>/<function>`.

- `Err(Error::msg("channel is archived"))` is shown to the caller (HTTP 422).
- Any other error (a refused model, a database failure) is internal: the caller gets a generic
  502 and the detail goes to the server log.

### Errors

`aether_sdk::Error` says what happened: `Capability` (declare it in `plugin.toml`), `Model`,
`InvalidRequest`, `NotImplemented`, `UnknownCommand`, `Database`, `Json`, and `Message` for
your own.

## Testing your logic natively

Keep each function's work in an ordinary function and test it against a mock kernel:

```rust
use aether_sdk::testing::MockKernel;

let kernel = MockKernel::new()
    .reply("db::create", |request| json!({ "ok": true, "data": request["data"] }))
    .fail("notify::send", "capability", "plugin does not hold `notify::send`");
let _installed = kernel.install();
// ... call your function, then inspect kernel.calls() ...
```

`aether_sdk::testing` exists only when building for the host, not for WebAssembly.

## Cache, files, web APIs and other plugins

```rust
// A private cache; entries may disappear, so keep only what you can recompute.
cache::set("usd_eur", &0.92, Some(3600))?;
let rate: Option<f64> = cache::get("usd_eur")?;

// Your own files. A missing file is `None`, not an error.
storage::write("invoices/A-1.txt", "total: 500")?;
let text = storage::read("invoices/A-1.txt")?;
let bytes = storage::read_bytes("logo.png")?;
let files = storage::list(Some("invoices"))?;

// Outside web APIs, to the hosts in `http_hosts = ["api.example.com"]` only.
let answer = http::post("https://api.example.com/charges")
    .header("authorization", "Bearer ...")
    .json(&json!({ "amount": 500 }))
    .send()?;
if !answer.ok { return Err(Error::msg("the payment service refused it")); }

// Another plugin, listed in `dependencies = ["currency"]`. It runs as the same person, under
// its own permissions, so nothing is lent in either direction.
let euro: Value = plugins::call("currency", "convert", &json!({ "amount": 10, "to": "EUR" }))?;

// Writes that must all happen or not at all.
db::transaction()
    .create("invoice", &json!({ "number": "A-1" }))
    .increment("counter", "invoices", "next", 1.0)
    .run()?;
```

The kernel enforces the limits (what each call may reach, sizes, timeouts); see
`docs/plugin/commands.md` in the Aether repository for every rule. An `http` call to a host that
is not listed, a `plugins::call` to a plugin that is not a dependency, or a missing capability is
an `Err`, never a silent no-op.

## Messages and background work

```rust
// One function for every kind of message: say the type and what is in it. The kernel sends it
// through the provider the administrator configured; you never see a key or choose the sender.
communication::email(["ann@example.com"]).subject("Welcome").text("Thanks for joining").send()?;
communication::sms(["+2348012345678"]).text("Your code is 1234").send()?;

// Run one of your functions later, in the background, with retries.
scheduler::enqueue("send_onboarding")
    .payload(&json!({ "user": "u1" }))
    .delay_secs(3600)
    .unique_key("onboarding-u1")   // never queue the same work twice
    .send()?;

// A recurring task (or declare it in plugin.toml with [[schedule]]).
scheduler::register("sweep", "sweep_stale").every("30m").save()?;
```

Sending is queued: it returns when the kernel has accepted the message, and delivery happens in the
scheduler, retried if the provider is down. Jobs are delivered at least once, so a background function
should be safe to run twice.

## Files that arrive in a folder

```toml
# plugin.toml: tell me when a CSV lands in my `inbox` folder
[[watch]]
name = "invoices"
path = "inbox"
pattern = "*.csv"
events = ["created"]
function = "import_invoice"
```

```rust
fn import_invoice(change: files::Changed) -> Result<()> {
    let Some(csv) = files::read(&change.path)? else { return Ok(()) };
    // ... parse and store the rows ...
    files::rename(&change.path, &format!("done/{}", change.file_name()))   // so it is handled once
}
```

The kernel listens with the operating system's file notifications (inotify on Linux), waits for a
file to be quiet before reporting it, and runs your function as a background job, so it is retried if
it fails. Paths are relative to your folder, and nothing outside it can be reached.

## Events between plugins, and bridges

```rust
// chat announces; any plugin that depends on chat and listens gets a background job
events::emit("message_posted", &json!({ "channel": "general" }))?;

// plugin.toml of a listener:  [[events]] name = "chat.message_posted" direction = "listen" handler = "on_message"
fn on_message(received: events::Received) -> Result<()> { /* ... */ Ok(()) }

// an integration the administrator set up; `bridges = ["paystack"]` in plugin.toml
let started: Value = bridge::call("paystack", "initialize_transaction", &json!({ "email": email, "amount": 50000 }))?;
```

## Other kernel commands

`aether_sdk::transport::call("command::name", &payload)` sends any kernel command. Every kernel command has an SDK module now.
