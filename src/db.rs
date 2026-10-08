//! The plugin's data.
//!
//! A plugin never opens a database or writes a query. It names a **model** (declared in
//! `access_models` in `plugin.toml`) and the kernel does the rest, for the organization
//! the call is for. A model the plugin was not granted, or a write to a read-only one, is
//! refused, so a plugin can only ever touch the data it is allowed to. Every access is
//! recorded in the audit trail with the ids of the records involved.
//!
//! Needs the capability `db::query` (get, find) or `db::mutate` (create, update, delete).
//!
//! ```no_run
//! use aether_sdk::{db, Result};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize)]
//! struct Message { id: Option<String>, channel: String, body: String }
//!
//! impl db::Model for Message { const MODEL: &'static str = "message"; }
//!
//! fn demo() -> Result<()> {
//!     let sent = Message { id: None, channel: "general".into(), body: "hello".into() }.create()?;
//!     let recent: Vec<Message> = Message::find().filter("channel", "general").order_by("date_created").limit(20).all()?;
//!     # let _ = (sent, recent);
//!     Ok(())
//! }
//! # use db::Model;
//! ```

use std::marker::PhantomData;

use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::transport::call;

/// A record type tied to one model, so reads and writes need no model name.
pub trait Model: Serialize + DeserializeOwned {
    /// The model's name, as listed in the plugin's `access_models`.
    const MODEL: &'static str;

    /// The record with this id.
    fn get(id: &str) -> Result<Option<Self>> {
        get(Self::MODEL, id)
    }

    /// Start a search over this model.
    fn find() -> Find<Self> {
        find(Self::MODEL)
    }

    /// Store this as a new record and return it as stored (with its `id`).
    fn create(&self) -> Result<Self> {
        create(Self::MODEL, self)
    }

    /// Change fields of the record with this id; only the fields in `changes` are written.
    fn update(id: &str, changes: &impl Serialize) -> Result<Option<Self>> {
        update(Self::MODEL, id, changes)
    }

    /// Remove the record with this id, returning it.
    fn delete(id: &str) -> Result<Option<Self>> {
        delete(Self::MODEL, id)
    }

    /// Add `by` to the number field `field` of the record with this id; see [`increment`].
    fn increment(id: &str, field: &str, by: f64) -> Result<Option<Self>> {
        increment(Self::MODEL, id, field, by)
    }
}

/// The record with this id, or `None` when there is none. `id` is the record's id as the
/// kernel gave it (`"table:key"`); the part before the colon may be left out.
pub fn get<T: DeserializeOwned>(model: &str, id: &str) -> Result<Option<T>> {
    one(call("db::get", &json!({ "model": model, "id": id }))?)
}

/// Like [`get`], with the rows of these child fields put into the record (`"lines"`): they come
/// back as a list under the field's name, in order.
pub fn get_expanded<T: DeserializeOwned>(model: &str, id: &str, expand: &[&str]) -> Result<Option<T>> {
    one(call("db::get", &json!({ "model": model, "id": id, "expand": expand }))?)
}

/// A move a record can make: a transition of the model's workflow.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Transition {
    pub name: String,
    pub label: String,
    pub from: Vec<String>,
    pub to: String,
}

/// The moves of the model's workflow the caller can make with record `id` right now (see the rules
/// guide). Empty when there are none, or no workflow.
pub fn transitions(model: &str, id: &str) -> Result<Vec<Transition>> {
    rows(call("db::transitions", &json!({ "model": model, "id": id }))?)
}

/// Start a search over `model`. Nothing is read until [`Find::all`] or [`Find::first`].
pub fn find<T: DeserializeOwned>(model: &str) -> Find<T> {
    Find {
        model: model.to_string(),
        filter: Map::new(),
        limit: None,
        offset: None,
        order: None,
        expand: Vec::new(),
        record: PhantomData,
    }
}

/// Store `data` as a new record of `model` and return it as stored.
pub fn create<T: DeserializeOwned>(model: &str, data: &impl Serialize) -> Result<T> {
    let data = record_fields(data)?;
    let created: Option<T> = one(call("db::create", &json!({ "model": model, "data": data }))?)?;
    created.ok_or_else(|| Error::Kernel("the kernel did not return the created record".into()))
}

/// Change the fields in `changes` of the record `id`, leaving the others alone. `None` when
/// there is no such record.
pub fn update<T: DeserializeOwned>(model: &str, id: &str, changes: &impl Serialize) -> Result<Option<T>> {
    let data = record_fields(changes)?;
    one(call("db::update", &json!({ "model": model, "id": id, "data": data }))?)
}

/// Remove the record `id`, returning it as it was. `None` when there is no such record.
pub fn delete<T: DeserializeOwned>(model: &str, id: &str) -> Result<Option<T>> {
    one(call("db::delete", &json!({ "model": model, "id": id }))?)
}

/// A search under construction. Conditions are all required to match (field equals value).
#[derive(Debug, Clone)]
pub struct Find<T> {
    model: String,
    filter: Map<String, Value>,
    limit: Option<u32>,
    offset: Option<u32>,
    order: Option<String>,
    expand: Vec<String>,
    record: PhantomData<fn() -> T>,
}

impl<T: DeserializeOwned> Find<T> {
    /// Only records whose `field` equals `value`. Field names are plain identifiers.
    pub fn filter(mut self, field: &str, value: impl Serialize) -> Self {
        self.filter.insert(field.to_string(), serde_json::to_value(value).unwrap_or(Value::Null));
        self
    }

    /// Only records that match `filter` as well as any conditions already given.
    pub fn matching(mut self, filter: Filter) -> Self {
        merge(&mut self.filter, filter);
        self
    }

    /// Order by this field, ascending; start the name with `-` to sort descending.
    pub fn order_by(mut self, field: &str) -> Self {
        self.order = Some(field.to_string());
        self
    }

    /// Put the rows of this child field (`"lines"`) into each record, in order.
    pub fn expand(mut self, child_field: &str) -> Self {
        self.expand.push(child_field.to_string());
        self
    }

    /// At most this many records. The kernel caps any search at 1000 whatever is asked.
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Skip this many records first.
    pub fn offset(mut self, offset: u32) -> Self {
        self.offset = Some(offset);
        self
    }

    fn request(&self, limit: Option<u32>) -> Value {
        let mut request = json!({ "model": self.model, "filter": self.filter });
        if let Some(limit) = limit.or(self.limit) {
            request["limit"] = json!(limit);
        }
        if let Some(offset) = self.offset {
            request["offset"] = json!(offset);
        }
        if let Some(order) = &self.order {
            request["order"] = json!(order);
        }
        if !self.expand.is_empty() {
            request["expand"] = json!(self.expand);
        }
        request
    }

    /// Run the search.
    pub fn all(self) -> Result<Vec<T>> {
        rows(call("db::find", &self.request(None))?)
    }

    /// How many records match, counted by the database without reading them.
    pub fn count(self) -> Result<u64> {
        count(&self.model, Filter::raw(self.filter))
    }

    /// Grouped figures over the matching records.
    pub fn aggregate(self, group_by: &[&str], figures: &[(&str, Figure)]) -> Result<Vec<Value>> {
        aggregate(&self.model, Filter::raw(self.filter), group_by, figures)
    }

    /// The first match, if any.
    pub fn first(self) -> Result<Option<T>> {
        Ok(rows(call("db::find", &self.request(Some(1)))?)?.into_iter().next())
    }
}

/// A condition on records, built from comparisons and combined with `and`, `or` and `not`. Field
/// names are the model's own; values are checked by the kernel against the field's type.
///
/// ```no_run
/// use aether_sdk::{db::{self, Filter}, Result, Value};
///
/// fn demo() -> Result<()> {
///     let senior_in_it_or_hr = Filter::gte("level", 5)
///         .and(Filter::any([Filter::eq("dept", "it"), Filter::eq("dept", "hr")]));
///     let people: Vec<Value> = db::find("employee").matching(senior_in_it_or_hr).all()?;
///     # let _ = people;
///     Ok(())
/// }
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter(Map<String, Value>);

impl Filter {
    fn op(field: &str, op: &str, value: impl Serialize) -> Self {
        let value = serde_json::to_value(value).unwrap_or(Value::Null);
        let mut map = Map::new();
        map.insert(field.to_string(), json!({ op: value }));
        Self(map)
    }

    fn raw(map: Map<String, Value>) -> Self {
        Self(map)
    }

    /// Matches every record.
    pub fn all() -> Self {
        Self::default()
    }

    pub fn eq(field: &str, value: impl Serialize) -> Self {
        Self::op(field, "eq", value)
    }

    pub fn ne(field: &str, value: impl Serialize) -> Self {
        Self::op(field, "ne", value)
    }

    pub fn gt(field: &str, value: impl Serialize) -> Self {
        Self::op(field, "gt", value)
    }

    pub fn gte(field: &str, value: impl Serialize) -> Self {
        Self::op(field, "gte", value)
    }

    pub fn lt(field: &str, value: impl Serialize) -> Self {
        Self::op(field, "lt", value)
    }

    pub fn lte(field: &str, value: impl Serialize) -> Self {
        Self::op(field, "lte", value)
    }

    /// The field is one of `values`.
    pub fn one_of(field: &str, values: impl IntoIterator<Item = impl Serialize>) -> Self {
        let list: Vec<Value> = values.into_iter().map(|v| serde_json::to_value(v).unwrap_or(Value::Null)).collect();
        Self::op(field, "in", list)
    }

    /// The field is none of `values`.
    pub fn none_of(field: &str, values: impl IntoIterator<Item = impl Serialize>) -> Self {
        let list: Vec<Value> = values.into_iter().map(|v| serde_json::to_value(v).unwrap_or(Value::Null)).collect();
        Self::op(field, "nin", list)
    }

    /// The field's text contains `text`, ignoring case.
    pub fn like(field: &str, text: &str) -> Self {
        Self::op(field, "like", text)
    }

    /// The field has no value.
    pub fn is_empty(field: &str) -> Self {
        Self::op(field, "null", true)
    }

    /// The field has a value.
    pub fn is_set(field: &str) -> Self {
        Self::op(field, "null", false)
    }

    /// Both this and `other` hold.
    pub fn and(self, other: Filter) -> Self {
        Self::join("and", [self, other])
    }

    /// Either this or `other` holds.
    pub fn or(self, other: Filter) -> Self {
        Self::join("or", [self, other])
    }

    /// Any one of `filters` holds.
    pub fn any(filters: impl IntoIterator<Item = Filter>) -> Self {
        Self::join("or", filters)
    }

    /// Every one of `filters` holds.
    pub fn every(filters: impl IntoIterator<Item = Filter>) -> Self {
        Self::join("and", filters)
    }

    /// This does not hold.
    pub fn not(self) -> Self {
        let mut map = Map::new();
        map.insert("not".to_string(), Value::Object(self.0));
        Self(map)
    }

    fn join(key: &str, filters: impl IntoIterator<Item = Filter>) -> Self {
        let list: Vec<Value> = filters.into_iter().map(|f| Value::Object(f.0)).collect();
        let mut map = Map::new();
        map.insert(key.to_string(), Value::Array(list));
        Self(map)
    }
}

/// Add `filter` to `target`, requiring both. Conditions on the same field are kept together
/// rather than one replacing the other.
fn merge(target: &mut Map<String, Value>, filter: Filter) {
    if target.is_empty() {
        *target = filter.0;
        return;
    }
    let existing = std::mem::take(target);
    *target = Filter::join("and", [Filter(existing), filter]).0;
}

/// A figure to compute for each group of an [`aggregate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Figure {
    Count,
    Sum(String),
    Avg(String),
    Min(String),
    Max(String),
}

impl Figure {
    pub fn sum(field: &str) -> Self {
        Self::Sum(field.to_string())
    }

    pub fn avg(field: &str) -> Self {
        Self::Avg(field.to_string())
    }

    pub fn min(field: &str) -> Self {
        Self::Min(field.to_string())
    }

    pub fn max(field: &str) -> Self {
        Self::Max(field.to_string())
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Count => json!("count"),
            Self::Sum(field) => json!({ "sum": field }),
            Self::Avg(field) => json!({ "avg": field }),
            Self::Min(field) => json!({ "min": field }),
            Self::Max(field) => json!({ "max": field }),
        }
    }
}

/// How many records of `model` match `filter`, counted by the database. Needs `db::query`.
pub fn count(model: &str, filter: Filter) -> Result<u64> {
    let reply = call("db::count", &json!({ "model": model, "filter": filter.0 }))?;
    reply
        .get("data")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::Kernel("the kernel did not return a count".into()))
}

/// Grouped figures over the records of `model` that match `filter`: one row per value of the
/// `group_by` fields (one row in all when there are none), holding those values and each
/// figure under the name given. Needs `db::query`.
///
/// ```no_run
/// use aether_sdk::{db::{self, Figure, Filter}, Result};
///
/// fn headcount_and_payroll() -> Result<()> {
///     let rows = db::aggregate("employee", Filter::eq("status", "active"), &["dept"],
///         &[("people", Figure::Count), ("salaries", Figure::sum("salary"))])?;
///     # let _ = rows;
///     Ok(())
/// }
/// ```
pub fn aggregate(model: &str, filter: Filter, group_by: &[&str], figures: &[(&str, Figure)]) -> Result<Vec<Value>> {
    let aggs: Map<String, Value> = figures.iter().map(|(name, figure)| (name.to_string(), figure.to_json())).collect();
    rows(call(
        "db::aggregate",
        &json!({ "model": model, "filter": filter.0, "group_by": group_by, "aggs": aggs }),
    )?)
}

/// Add `by` (negative subtracts) to the number field `field` of the record `id`, in the
/// database itself, and return the record. Two callers incrementing at once both count, which
/// is what a counter or a sequence needs; never read, add and write back instead. A field that
/// has no value yet counts as zero. `None` when there is no such record.
pub fn increment<T: DeserializeOwned>(model: &str, id: &str, field: &str, by: f64) -> Result<Option<T>> {
    one(call("db::increment", &json!({ "model": model, "id": id, "field": field, "by": by }))?)
}

/// Link the record `id` of `model` to each of `to` through its many2many field `field`. Linking
/// twice is harmless. Returns the record. Needs `db::mutate`.
pub fn relate<T: DeserializeOwned>(model: &str, field: &str, id: &str, to: &[&str]) -> Result<Option<T>> {
    one(call("db::relate", &json!({ "model": model, "field": field, "id": id, "to": to }))?)
}

/// Remove those links; ones that do not exist are ignored. Needs `db::mutate`.
pub fn unrelate<T: DeserializeOwned>(model: &str, field: &str, id: &str, to: &[&str]) -> Result<Option<T>> {
    one(call("db::unrelate", &json!({ "model": model, "field": field, "id": id, "to": to }))?)
}

/// The records linked to `id` through a many2many field: records of the field's target model
/// (which the plugin must be able to read).
pub fn related<T: DeserializeOwned>(model: &str, field: &str, id: &str) -> Result<Vec<T>> {
    rows(call("db::related", &json!({ "model": model, "field": field, "id": id }))?)
}

/// The other way round: `id` is a record of the field's target, and the answer is the records of
/// `model` linked to it.
pub fn related_reverse<T: DeserializeOwned>(model: &str, field: &str, id: &str) -> Result<Vec<T>> {
    rows(call("db::related", &json!({ "model": model, "field": field, "id": id, "reverse": true }))?)
}

/// Which way to walk a hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Walk {
    /// Descendants: children, their children, and so on.
    Down,
    /// Ancestors: the parent, its parent, and so on.
    Up,
}

/// A walk through a tree built with a `hierarchy` link, in one query whatever the depth.
///
/// ```no_run
/// use aether_sdk::{db::{self, Walk}, Result, Value};
///
/// fn everything_under(ministry: &str) -> Result<Vec<Value>> {
///     db::tree("unit", "parent", ministry).walk(Walk::Down).include_self().all()
/// }
/// ```
pub fn tree(model: &str, field: &str, id: &str) -> Tree {
    Tree { model: model.into(), field: field.into(), id: id.into(), walk: Walk::Down, depth: None, include_self: false }
}

/// A hierarchy walk under construction.
#[derive(Debug, Clone)]
pub struct Tree {
    model: String,
    field: String,
    id: String,
    walk: Walk,
    depth: Option<u32>,
    include_self: bool,
}

impl Tree {
    /// Down to descendants (the default) or up to ancestors.
    pub fn walk(mut self, walk: Walk) -> Self {
        self.walk = walk;
        self
    }

    /// Only this many levels (the kernel allows at most 64); `1` is the children (or the parent).
    pub fn depth(mut self, depth: u32) -> Self {
        self.depth = Some(depth);
        self
    }

    /// Include the record itself.
    pub fn include_self(mut self) -> Self {
        self.include_self = true;
        self
    }

    /// Run the walk. Needs `db::query`.
    pub fn all<T: DeserializeOwned>(self) -> Result<Vec<T>> {
        let direction = match self.walk {
            Walk::Down => "down",
            Walk::Up => "up",
        };
        let mut request = json!({
            "model": self.model, "field": self.field, "id": self.id,
            "direction": direction, "include_self": self.include_self,
        });
        if let Some(depth) = self.depth {
            request["depth"] = json!(depth);
        }
        rows(call("db::tree", &request)?)
    }
}

/// Add an exact amount, such as `"-1.5"`, to the decimal field `field` of the record `id`, in
/// the database itself. `None` when there is no such record.
pub fn increment_decimal<T: DeserializeOwned>(model: &str, id: &str, field: &str, by: &str) -> Result<Option<T>> {
    one(call("db::increment", &json!({ "model": model, "id": id, "field": field, "by": by }))?)
}

/// Start an atomic batch of writes: all of them are applied or none is. Needs the capability
/// `db::transaction` as well as `db::mutate`, and every write is checked against the plugin's
/// model grants and recorded in the audit trail as when made alone.
///
/// ```no_run
/// use aether_sdk::{db, json, Result};
///
/// fn issue(number: &str) -> Result<()> {
///     db::transaction()
///         .create("invoice", &json!({ "number": number }))
///         .increment("counter", "invoices", "next", 1.0)
///         .run()?;
///     Ok(())
/// }
/// ```
pub fn transaction() -> Transaction {
    Transaction { ops: Vec::new() }
}

/// An atomic batch of writes under construction. Nothing happens until [`Transaction::run`].
#[derive(Debug, Clone, Default)]
pub struct Transaction {
    ops: Vec<Value>,
}

impl Transaction {
    /// Create a record of `model`.
    #[must_use]
    pub fn create(mut self, model: &str, data: &impl Serialize) -> Self {
        let data = record_fields(data).map(Value::Object).unwrap_or(Value::Null);
        self.ops.push(json!({ "op": "create", "model": model, "data": data }));
        self
    }

    /// Change fields of the record `id`; only the fields in `changes` are written.
    #[must_use]
    pub fn update(mut self, model: &str, id: &str, changes: &impl Serialize) -> Self {
        let data = record_fields(changes).map(Value::Object).unwrap_or(Value::Null);
        self.ops.push(json!({ "op": "update", "model": model, "id": id, "data": data }));
        self
    }

    /// Remove the record `id`.
    #[must_use]
    pub fn delete(mut self, model: &str, id: &str) -> Self {
        self.ops.push(json!({ "op": "delete", "model": model, "id": id }));
        self
    }

    /// Add `by` to the number field `field` of the record `id`; see [`increment`].
    #[must_use]
    pub fn increment(mut self, model: &str, id: &str, field: &str, by: f64) -> Self {
        self.ops.push(json!({ "op": "increment", "model": model, "id": id, "field": field, "by": by }));
        self
    }

    /// Apply the writes together. Returns each write's record, in the order added (`null`
    /// for a record that did not exist). 1 to 50 writes; if any fails, none is applied and
    /// this returns the error.
    pub fn run(self) -> Result<Vec<Value>> {
        if self.ops.iter().any(|op| op["data"].is_null() && op["op"] != "delete" && op["op"] != "increment") {
            return Err(Error::InvalidRequest("a record must serialize to a JSON object".into()));
        }
        let reply = call("db::transaction", &json!({ "ops": self.ops }))?;
        rows(reply)
    }
}

/// A record's fields as the JSON object the kernel expects.
fn record_fields(record: &impl Serialize) -> Result<Map<String, Value>> {
    match serde_json::to_value(record)? {
        Value::Object(mut fields) => {
            // The kernel assigns ids; a null `id` from `Option<String>` would be a field.
            if matches!(fields.get("id"), Some(Value::Null)) {
                fields.remove("id");
            }
            Ok(fields)
        }
        _ => Err(Error::InvalidRequest("a record must serialize to a JSON object".into())),
    }
}

/// `data` of a reply, as zero or one record.
fn one<T: DeserializeOwned>(reply: Value) -> Result<Option<T>> {
    match reply.get("data") {
        None | Some(Value::Null) => Ok(None),
        Some(record) => Ok(Some(serde_json::from_value(record.clone())?)),
    }
}

/// `data` of a reply, as a list of records.
fn rows<T: DeserializeOwned>(reply: Value) -> Result<Vec<T>> {
    match reply.get("data") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(records) => Ok(serde_json::from_value(records.clone())?),
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::testing::MockKernel;

    #[derive(Debug, PartialEq, Serialize, serde::Deserialize)]
    struct Message {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        body: String,
    }

    impl Model for Message {
        const MODEL: &'static str = "message";
    }

    #[test]
    fn filters_combine_into_the_json_the_kernel_reads() {
        let filter = Filter::gte("level", 5).and(Filter::any([Filter::eq("dept", "it"), Filter::like("name", "jo").not()]));
        assert_eq!(
            Value::Object(filter.0),
            json!({ "and": [
                { "level": { "gte": 5 } },
                { "or": [ { "dept": { "eq": "it" } }, { "not": { "name": { "like": "jo" } } } ] }
            ] })
        );
    }

    #[test]
    fn matching_keeps_earlier_conditions() -> Result<()> {
        let kernel = MockKernel::new().reply("db::find", |_| json!({ "ok": true, "data": [] }));
        let _installed = kernel.install();
        let _: Vec<Value> = find("m").filter("a", 1).matching(Filter::gt("b", 2)).all()?;
        let calls = kernel.calls();
        assert_eq!(
            calls[0].1["filter"],
            json!({ "and": [ { "a": 1 }, { "b": { "gt": 2 } } ] })
        );
        Ok(())
    }

    #[test]
    fn count_and_aggregate_send_their_commands() -> Result<()> {
        let kernel = MockKernel::new()
            .reply("db::count", |_| json!({ "ok": true, "data": 7 }))
            .reply("db::aggregate", |_| json!({ "ok": true, "data": [{ "dept": "it", "n": 2 }] }));
        let _installed = kernel.install();
        assert_eq!(count("m", Filter::eq("a", 1))?, 7);
        let rows = aggregate("m", Filter::all(), &["dept"], &[("n", Figure::Count), ("t", Figure::sum("pay"))])?;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            kernel.calls()[1].1,
            json!({ "model": "m", "filter": {}, "group_by": ["dept"], "aggs": { "n": "count", "t": { "sum": "pay" } } })
        );
        Ok(())
    }

    #[test]
    fn relations_and_trees_send_their_commands() -> Result<()> {
        let kernel = MockKernel::new()
            .reply("db::relate", |_| json!({ "ok": true, "data": { "id": "u:1" } }))
            .reply("db::related", |_| json!({ "ok": true, "data": [{ "id": "s:1" }] }))
            .reply("db::tree", |_| json!({ "ok": true, "data": [] }));
        let _installed = kernel.install();
        let _: Option<Value> = relate("unit", "skills", "u:1", &["s:1"])?;
        let skills: Vec<Value> = related("unit", "skills", "u:1")?;
        assert_eq!(skills.len(), 1);
        let _: Vec<Value> = related_reverse("unit", "skills", "s:1")?;
        let _: Vec<Value> = tree("unit", "parent", "u:1").walk(Walk::Up).depth(2).include_self().all()?;
        let calls = kernel.calls();
        assert_eq!(calls[0].1, json!({ "model": "unit", "field": "skills", "id": "u:1", "to": ["s:1"] }));
        assert_eq!(calls[2].1["reverse"], json!(true));
        assert_eq!(
            calls[3].1,
            json!({ "model": "unit", "field": "parent", "id": "u:1", "direction": "up", "include_self": true, "depth": 2 })
        );
        Ok(())
    }

    #[test]
    fn create_sends_the_fields_and_returns_the_stored_record() {
        let kernel = MockKernel::new().reply("db::create", |request| {
            let mut data = request["data"].clone();
            data["id"] = json!("chat_message:abc");
            json!({ "ok": true, "data": data })
        });
        let _installed = kernel.install();
        let stored = Message { id: None, body: "hi".into() }.create().unwrap();
        assert_eq!(stored, Message { id: Some("chat_message:abc".into()), body: "hi".into() });
        assert_eq!(
            kernel.calls(),
            [("db::create".to_string(), json!({ "model": "message", "data": { "body": "hi" } }))]
        );
    }

    #[test]
    fn find_builds_the_request_and_reads_the_rows() {
        let kernel = MockKernel::new().reply("db::find", |_| {
            json!({ "ok": true, "data": [{ "id": "m:1", "body": "a" }, { "id": "m:2", "body": "b" }] })
        });
        let _installed = kernel.install();
        let found = Message::find().filter("channel", "general").order_by("body").limit(5).offset(10).all().unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(
            kernel.calls()[0].1,
            json!({ "model": "message", "filter": { "channel": "general" }, "limit": 5, "offset": 10, "order": "body" })
        );
    }

    #[test]
    fn first_asks_for_one_row_and_get_handles_a_missing_record() {
        let kernel = MockKernel::new()
            .reply("db::find", |_| json!({ "ok": true, "data": [] }))
            .reply("db::get", |_| json!({ "ok": true, "data": null }));
        let _installed = kernel.install();
        assert_eq!(Message::find().first().unwrap(), None);
        assert_eq!(kernel.calls()[0].1["limit"], 1);
        assert_eq!(Message::get("m:nope").unwrap(), None);
    }

    #[test]
    fn update_and_delete_name_the_record() {
        let kernel = MockKernel::new()
            .reply("db::update", |_| json!({ "ok": true, "data": { "id": "m:1", "body": "edited" } }))
            .reply("db::delete", |_| json!({ "ok": true, "data": { "id": "m:1", "body": "edited" } }));
        let _installed = kernel.install();
        let edited = Message::update("m:1", &json!({ "body": "edited" })).unwrap();
        assert_eq!(edited.map(|m| m.body), Some("edited".to_string()));
        assert!(Message::delete("m:1").unwrap().is_some());
        let calls = kernel.calls();
        assert_eq!(calls[0].1, json!({ "model": "message", "id": "m:1", "data": { "body": "edited" } }));
        assert_eq!(calls[1].1, json!({ "model": "message", "id": "m:1" }));
    }

    #[test]
    fn the_kernels_refusals_come_back_as_errors() {
        let kernel = MockKernel::new()
            .fail("db::find", "model", "model `secret` is not in this plugin's access_models")
            .fail("db::create", "capability", "plugin does not hold `db::mutate`");
        let _installed = kernel.install();
        assert!(matches!(find::<Value>("secret").all(), Err(Error::Model(_))));
        assert!(matches!(Message { id: None, body: "x".into() }.create(), Err(Error::Capability(_))));
    }

    #[test]
    fn increment_asks_the_database_to_add() {
        let kernel = MockKernel::new()
            .reply("db::increment", |_| json!({ "ok": true, "data": { "id": "m:1", "body": "x", "n": 5 } }));
        let _installed = kernel.install();
        let counted: Option<Value> = Message::increment("m:1", "n", 4.0).unwrap().map(|m| json!({ "body": m.body }));
        assert_eq!(counted, Some(json!({ "body": "x" })));
        assert_eq!(
            kernel.calls()[0].1,
            json!({ "model": "message", "id": "m:1", "field": "n", "by": 4.0 })
        );
    }

    #[test]
    fn a_transaction_sends_every_write_at_once() {
        let kernel = MockKernel::new()
            .reply("db::transaction", |request| {
                let count = request["ops"].as_array().map_or(0, Vec::len);
                json!({ "ok": true, "data": (0..count).map(|n| json!({ "id": format!("m:{n}") })).collect::<Vec<_>>() })
            });
        let _installed = kernel.install();
        let made = transaction()
            .create("message", &json!({ "body": "a" }))
            .update("message", "m:9", &json!({ "body": "b" }))
            .increment("counter", "c", "next", 1.0)
            .delete("message", "m:1")
            .run()
            .unwrap();
        assert_eq!(made.len(), 4);
        let calls = kernel.calls();
        assert_eq!(calls.len(), 1, "one kernel call for the whole batch");
        let ops = calls[0].1["ops"].as_array().unwrap();
        let names: Vec<_> = ops.iter().map(|op| op["op"].as_str().unwrap()).collect();
        assert_eq!(names, ["create", "update", "increment", "delete"]);
        assert_eq!(ops[0]["data"], json!({ "body": "a" }));
    }

    #[test]
    fn a_transaction_with_a_non_object_record_is_refused_before_sending() {
        let kernel = MockKernel::new();
        let _installed = kernel.install();
        assert!(matches!(transaction().create("message", &"text").run(), Err(Error::InvalidRequest(_))));
        assert!(kernel.calls().is_empty());
    }

    #[test]
    fn a_record_must_be_an_object() {
        let _installed = MockKernel::new().install();
        assert!(matches!(create::<Value>("message", &"just text"), Err(Error::InvalidRequest(_))));
    }
}
