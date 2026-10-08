//! The plugin's own folder on disk, and the files that arrive in it.
//!
//! Each plugin has a folder per organization on the server's local disk. An administrator or another
//! program drops files there; a `[[watch]]` in `plugin.toml` tells the plugin when one appears, and
//! these functions read, write and move files in it. Paths are **relative to the folder**: `..`,
//! absolute paths and links that lead out of it are refused, so a plugin cannot reach anything else.
//!
//! ```toml
//! [[watch]]
//! name = "invoices"
//! path = "inbox"             # relative to the plugin's folder
//! pattern = "*.csv"          # optional; `**/*.csv` also matches subfolders
//! events = ["created"]       # created, modified, deleted; default created and modified
//! function = "import_invoice"
//! # recursive = true         # also watch subfolders
//! # catch_up = false         # on by default: on start, report what arrived or changed while nothing listened
//! # poll_secs = 10           # for network file systems, which send no notifications
//! ```
//!
//! ```no_run
//! use aether_sdk::{files, handler, Result};
//!
//! fn import_invoice(change: files::Changed) -> Result<()> {
//!     let Some(csv) = files::read(&change.path)? else { return Ok(()) }; // already gone
//!     // ... parse and store the rows ...
//!     # let _ = csv;
//!     // Move it away so it is handled once, even if the scheduler restarts.
//!     files::rename(&change.path, &format!("done/{}", change.file_name()))?;
//!     Ok(())
//! }
//! ```
//!
//! The kernel waits until a file has been quiet for `debounce_ms` (500 by default) before it tells the
//! plugin, so a file that is still being copied is not reported half written. Names starting with `.`
//! and ending in `~`, `.part`, `.tmp`, `.swp`, `.crdownload` or `.partial` are ignored. Files that arrive
//! while the scheduler is down are reported when it starts again (`catch_up`). You are told at
//! least once: a handler should cope with being run twice for the same change.
//!
//! Needs the capabilities `fs::read`, `fs::list`, `fs::write` and `fs::delete` for the calls that use
//! them. For files that belong to the organization and may live in S3, use [`storage`](crate::storage).

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::transport::call;

/// What a watch's function is given when a file changes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Changed {
    /// The watch's name in `plugin.toml`.
    pub watch: String,
    /// Relative to the plugin's folder, ready to pass to [`read`].
    pub path: String,
    /// `"created"`, `"modified"` or `"deleted"`.
    pub event: String,
    /// The file's size when it settled; none for a deletion.
    pub size: Option<u64>,
    /// The file's modification time in nanoseconds since the epoch (as text, to be exact).
    pub modified_ns: Option<String>,
}

impl Changed {
    /// The last part of the path: `inbox/a.csv` gives `a.csv`.
    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    pub fn is_deleted(&self) -> bool {
        self.event == "deleted"
    }
}

/// A file or folder in the plugin's folder.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_ns: Option<String>,
}

/// The file as text. `None` when there is no such file; an error when it is not valid UTF-8 (use
/// [`read_bytes`]) or is larger than 10 MiB.
pub fn read(path: &str) -> Result<Option<String>> {
    let reply = call("fs::read", &json!({ "path": path }))?;
    Ok(reply["data"].get("text").and_then(Value::as_str).map(str::to_string))
}

/// The file as bytes, or `None` when there is no such file.
pub fn read_bytes(path: &str) -> Result<Option<Vec<u8>>> {
    let reply = call("fs::read", &json!({ "path": path, "encoding": "base64" }))?;
    match reply["data"].get("base64").and_then(Value::as_str) {
        Some(encoded) => STANDARD
            .decode(encoded)
            .map(Some)
            .map_err(|error| Error::Kernel(format!("the kernel sent invalid base64: {error}"))),
        None => Ok(None),
    }
}

/// Write `text` to the file, creating folders as needed and replacing any file there. The file
/// appears complete: a watcher never sees it half written.
pub fn write(path: &str, text: &str) -> Result<()> {
    call("fs::write", &json!({ "path": path, "text": text }))?;
    Ok(())
}

/// Write `bytes` to the file.
pub fn write_bytes(path: &str, bytes: &[u8]) -> Result<()> {
    call("fs::write", &json!({ "path": path, "base64": STANDARD.encode(bytes) }))?;
    Ok(())
}

/// What is in a folder (`""` is the plugin's folder), at most 1000 entries, not looking inside
/// subfolders.
pub fn list(folder: &str) -> Result<Vec<Entry>> {
    let reply = call("fs::list", &json!({ "path": folder }))?;
    Ok(serde_json::from_value(reply["data"].clone())?)
}

/// Size and date of a file or folder, or `None` if it does not exist.
pub fn stat(path: &str) -> Result<Option<Entry>> {
    let reply = call("fs::stat", &json!({ "path": path }))?;
    match &reply["data"] {
        Value::Null => Ok(None),
        data => Ok(Some(Entry {
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            path: path.to_string(),
            is_dir: data["is_dir"].as_bool().unwrap_or(false),
            size: data["size"].as_u64().unwrap_or(0),
            modified_ns: data["modified_ns"].as_str().map(str::to_string),
        })),
    }
}

/// Move a file, creating the destination's folders. Refuses to replace something that is already
/// there; see [`rename_over`].
pub fn rename(from: &str, to: &str) -> Result<()> {
    call("fs::rename", &json!({ "from": from, "to": to }))?;
    Ok(())
}

/// Move a file, replacing the destination if there is one.
pub fn rename_over(from: &str, to: &str) -> Result<()> {
    call("fs::rename", &json!({ "from": from, "to": to, "overwrite": true }))?;
    Ok(())
}

/// Delete a file or an empty folder. Deleting what is not there is not an error.
pub fn delete(path: &str) -> Result<()> {
    call("fs::delete", &json!({ "path": path }))?;
    Ok(())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::testing::MockKernel;

    #[test]
    fn a_watch_event_reads_as_a_handler_input() {
        let change: Changed = serde_json::from_value(json!({
            "watch": "invoices", "path": "inbox/2026/a.csv", "event": "created", "size": 15, "modified_ns": "1790000000000000000"
        }))
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!((change.file_name(), change.is_deleted(), change.size), ("a.csv", false, Some(15)));
        let gone: Changed = serde_json::from_value(json!({ "watch": "w", "path": "a.csv", "event": "deleted", "size": null, "modified_ns": null }))
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(gone.is_deleted() && gone.size.is_none());
    }

    #[test]
    fn reading_and_writing_use_the_right_encoding_and_a_missing_file_is_none() {
        let kernel = MockKernel::new()
            .reply("fs::read", |request| match (request["path"].as_str(), request.get("encoding")) {
                (Some("gone"), _) => json!({ "ok": true, "data": null }),
                (_, Some(_)) => json!({ "ok": true, "data": { "base64": "//4=", "size": 2 } }),
                _ => json!({ "ok": true, "data": { "text": "hello", "size": 5 } }),
            })
            .reply("fs::write", |_| json!({ "ok": true, "data": {} }));
        let _installed = kernel.install();
        assert_eq!(read("a.txt").unwrap(), Some("hello".to_string()));
        assert_eq!(read_bytes("b.bin").unwrap(), Some(vec![0xff, 0xfe]));
        assert_eq!(read("gone").unwrap(), None);
        write("out/a.txt", "x").unwrap();
        write_bytes("out/b.bin", &[0xff, 0xfe]).unwrap();
        let calls = kernel.calls();
        assert_eq!(calls[3].1, json!({ "path": "out/a.txt", "text": "x" }));
        assert_eq!(calls[4].1, json!({ "path": "out/b.bin", "base64": "//4=" }));
    }

    #[test]
    fn listing_stat_move_and_delete() {
        let kernel = MockKernel::new()
            .reply("fs::list", |_| json!({ "ok": true, "data": [{ "name": "a.csv", "path": "in/a.csv", "is_dir": false, "size": 3, "modified_ns": "1" }] }))
            .reply("fs::stat", |request| {
                if request["path"] == "gone" { json!({ "ok": true, "data": null }) } else { json!({ "ok": true, "data": { "is_dir": true, "size": 0, "modified_ns": "2" } }) }
            })
            .reply("fs::rename", |_| json!({ "ok": true, "data": {} }))
            .reply("fs::delete", |_| json!({ "ok": true, "data": null }));
        let _installed = kernel.install();
        let entries = list("in").unwrap();
        assert_eq!((entries[0].name.as_str(), entries[0].size, entries[0].is_dir), ("a.csv", 3, false));
        assert!(stat("done").unwrap().is_some_and(|e| e.is_dir));
        assert!(stat("gone").unwrap().is_none());
        rename("in/a.csv", "done/a.csv").unwrap();
        rename_over("in/a.csv", "done/a.csv").unwrap();
        delete("done/a.csv").unwrap();
        let calls = kernel.calls();
        assert_eq!(calls[3].1, json!({ "from": "in/a.csv", "to": "done/a.csv" }));
        assert_eq!(calls[4].1, json!({ "from": "in/a.csv", "to": "done/a.csv", "overwrite": true }));
    }

    #[test]
    fn a_path_that_leaves_the_folder_is_an_invalid_request() {
        let kernel = MockKernel::new().fail("fs::read", "invalid_payload", "`../x` is not a path inside the plugin's folder");
        let _installed = kernel.install();
        assert!(matches!(read("../x"), Err(Error::InvalidRequest(_))));
    }
}
