//! Files, in the organization's media storage (a local directory or S3, whichever the
//! administrator chose; your plugin never knows which).
//!
//! Your files live in a folder of their own: keys such as `invoices/2026/a.pdf` are yours
//! alone, and no other plugin (or the organization's uploads) can be reached, whatever the
//! key. Keys with `..`, empty parts or backslashes are refused. A file is at most 10 MiB.
//!
//! Needs the capabilities `storage::read`, `storage::write`, `storage::delete` and
//! `storage::list` for the calls that use them.
//!
//! ```no_run
//! use aether_sdk::{storage, Result};
//!
//! fn archive(number: &str, pdf: &[u8]) -> Result<()> {
//!     storage::write_bytes(&format!("invoices/{number}.pdf"), pdf)?;
//!     for file in storage::list(Some("invoices"))? {
//!         let _ = (file.key, file.size);
//!     }
//!     Ok(())
//! }
//! ```

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::transport::call;

/// A stored file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct File {
    /// The key you wrote it under.
    pub key: String,
    pub size: u64,
}

/// Write `text` to `key`, replacing any file already there.
pub fn write(key: &str, text: &str) -> Result<()> {
    call("storage::write", &json!({ "key": key, "text": text }))?;
    Ok(())
}

/// Write `bytes` to `key`, replacing any file already there.
pub fn write_bytes(key: &str, bytes: &[u8]) -> Result<()> {
    call("storage::write", &json!({ "key": key, "base64": STANDARD.encode(bytes) }))?;
    Ok(())
}

/// The file at `key` as text. `None` when there is no such file; an error when it is not
/// valid UTF-8 (use [`read_bytes`]).
pub fn read(key: &str) -> Result<Option<String>> {
    let reply = call("storage::read", &json!({ "key": key }))?;
    match reply["data"].get("text") {
        Some(Value::String(text)) => Ok(Some(text.clone())),
        _ => Ok(None),
    }
}

/// The file at `key` as bytes, or `None` when there is no such file.
pub fn read_bytes(key: &str) -> Result<Option<Vec<u8>>> {
    let reply = call("storage::read", &json!({ "key": key, "encoding": "base64" }))?;
    match reply["data"].get("base64") {
        Some(Value::String(encoded)) => STANDARD
            .decode(encoded)
            .map(Some)
            .map_err(|error| Error::Kernel(format!("the kernel sent invalid base64: {error}"))),
        _ => Ok(None),
    }
}

/// Remove the file at `key`. Removing a file that is not there is not an error.
pub fn delete(key: &str) -> Result<()> {
    call("storage::delete", &json!({ "key": key }))?;
    Ok(())
}

/// Your files, optionally only those under `prefix` (such as `"invoices"`). At most 1000.
pub fn list(prefix: Option<&str>) -> Result<Vec<File>> {
    let request = match prefix {
        Some(prefix) => json!({ "prefix": prefix }),
        None => json!({}),
    };
    let reply = call("storage::list", &request)?;
    Ok(serde_json::from_value(reply["data"].clone()).unwrap_or_default())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::testing::MockKernel;

    #[test]
    fn text_and_bytes_are_sent_in_the_right_encoding() {
        let kernel = MockKernel::new().reply("storage::write", |_| json!({ "ok": true, "data": {} }));
        let _installed = kernel.install();
        write("a.txt", "hello").unwrap();
        write_bytes("b.bin", &[0, 1, 2]).unwrap();
        let calls = kernel.calls();
        assert_eq!(calls[0].1, json!({ "key": "a.txt", "text": "hello" }));
        assert_eq!(calls[1].1, json!({ "key": "b.bin", "base64": "AAEC" }));
    }

    #[test]
    fn reading_decodes_and_a_missing_file_is_none() {
        let kernel = MockKernel::new().reply("storage::read", |request| {
            match (request["key"].as_str(), request.get("encoding")) {
                (Some("gone"), _) => json!({ "ok": true, "data": null }),
                (_, Some(_)) => json!({ "ok": true, "data": { "base64": "AAEC", "size": 3 } }),
                _ => json!({ "ok": true, "data": { "text": "hello", "size": 5 } }),
            }
        });
        let _installed = kernel.install();
        assert_eq!(read("a.txt").unwrap(), Some("hello".to_string()));
        assert_eq!(read_bytes("b.bin").unwrap(), Some(vec![0, 1, 2]));
        assert_eq!(read("gone").unwrap(), None);
        assert_eq!(read_bytes("gone").unwrap(), None);
    }

    #[test]
    fn listing_returns_keys_and_sizes() {
        let kernel = MockKernel::new()
            .reply("storage::list", |_| json!({ "ok": true, "data": [{ "key": "a/b.txt", "size": 5 }] }));
        let _installed = kernel.install();
        assert_eq!(list(Some("a")).unwrap(), [File { key: "a/b.txt".into(), size: 5 }]);
        assert_eq!(list(None).unwrap().len(), 1);
        assert_eq!(kernel.calls()[0].1, json!({ "prefix": "a" }));
        assert_eq!(kernel.calls()[1].1, json!({}));
    }

    #[test]
    fn a_refused_key_is_an_invalid_request() {
        let kernel = MockKernel::new().fail("storage::write", "invalid_payload", "invalid media key `../x`");
        let _installed = kernel.install();
        assert!(matches!(write("../x", "no"), Err(Error::InvalidRequest(_))));
    }
}
