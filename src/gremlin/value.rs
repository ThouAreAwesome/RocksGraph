// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of RocksGraph.
//
// RocksGraph is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 2 of the License, or
// (at your option) any later version.
//
// RocksGraph is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with RocksGraph.  If not, see <https://www.gnu.org/licenses/>.

//! Public-facing value type system for traversal inputs and outputs.
//!
//! [`Value`] is the **single** type used for both traversal inputs (filter
//! values, property write values) and traversal outputs (query results).
//! The same literal that appears in an input step appears as the same variant
//! in the output, so no conversion is needed on the caller side.
//!
//! # Key
//!
//! [`Key`] distinguishes system attributes (`id`, `label`) from user-defined
//! property names.  This lets the same key be used symmetrically in input
//! steps and output extraction:
//!
//! ```ignore
//! // filter by id
//! snap.g().V([]).has(Key::Id, 42i64).next()?
//! // extract id as a scalar
//! snap.g().V([42]).values([Key::Id]).next()?  // → Some(Value::Int64(42))
//! ```
//!
//! # Predicate
//!
//! [`Predicate`] wraps a comparison operator and value for filter steps.
//! Bare scalars implicitly become [`Predicate::Eq`] via `From` impls, so
//! `.has("age", 42i32)` is equivalent to `.has("age", eq(42i32))`.

use std::collections::HashMap;

// ── Key ───────────────────────────────────────────────────────────────────────

/// A step key: either a system attribute or a user-defined property name.
///
/// Used in `.has(key, pred)` and `.values(keys)`.
/// `Key::Property("name")` can be constructed directly from `&str` or `String`
/// via the `From` impls, so callers rarely need to write `Key::Property(...)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// The element's system identifier (vertex `id`, or an edge's composite key).
    Id,
    /// The element's system label (numeric `label_id`).
    Label,
    /// A user-defined property name.
    Property(String),
}

impl From<&str> for Key {
    fn from(s: &str) -> Self {
        Key::Property(s.to_owned())
    }
}

impl From<String> for Key {
    fn from(s: String) -> Self {
        Key::Property(s)
    }
}

// ── Predicate ─────────────────────────────────────────────────────────────────

/// A filter predicate for `.has()` and `.is()` steps.
///
/// Bare scalars implicitly become [`Predicate::Eq`] via `From` impls:
/// `.has("age", 42i32)` is equivalent to `.has("age", eq(42i32))`.
///
/// Use the free functions [`eq`], [`gt`], [`lt`], [`between`], [`within`], etc.
/// to construct non-equality predicates.
#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    Eq(Value),
    Ne(Value),
    Gt(Value),
    Gte(Value),
    Lt(Value),
    Lte(Value),
    /// `Between(lo, hi)` — inclusive `lo`, exclusive `hi`.
    Between(Value, Value),
    /// Value is one of the given set.
    Within(Vec<Value>),
    /// Value is none of the given set.
    Without(Vec<Value>),
}

/// Filter: equal to `v`.
pub fn eq(v: impl Into<Value>) -> Predicate {
    Predicate::Eq(v.into())
}
/// Filter: not equal to `v`.
pub fn ne(v: impl Into<Value>) -> Predicate {
    Predicate::Ne(v.into())
}
/// Filter: greater than `v`.
pub fn gt(v: impl Into<Value>) -> Predicate {
    Predicate::Gt(v.into())
}
/// Filter: greater than or equal to `v`.
pub fn gte(v: impl Into<Value>) -> Predicate {
    Predicate::Gte(v.into())
}
/// Filter: less than `v`.
pub fn lt(v: impl Into<Value>) -> Predicate {
    Predicate::Lt(v.into())
}
/// Filter: less than or equal to `v`.
pub fn lte(v: impl Into<Value>) -> Predicate {
    Predicate::Lte(v.into())
}
/// Filter: `lo` ≤ value < `hi`.
pub fn between(lo: impl Into<Value>, hi: impl Into<Value>) -> Predicate {
    Predicate::Between(lo.into(), hi.into())
}
/// Filter: value is one of `vs`.
pub fn within(vs: impl IntoIterator<Item = impl Into<Value>>) -> Predicate {
    Predicate::Within(vs.into_iter().map(Into::into).collect())
}
/// Filter: value is none of `vs`.
pub fn without(vs: impl IntoIterator<Item = impl Into<Value>>) -> Predicate {
    Predicate::Without(vs.into_iter().map(Into::into).collect())
}

/// Any type that converts to [`Value`] also converts to [`Predicate::Eq`].
///
/// This covers all scalar Rust types (`i32`, `i64`, `bool`, `&str`, `f64`, …)
/// and lets callers write `.has("age", 42i32)` without wrapping in `eq(...)`.
impl<T: Into<Value>> From<T> for Predicate {
    fn from(v: T) -> Self {
        Predicate::Eq(v.into())
    }
}

// ── Value ─────────────────────────────────────────────────────────────────────

/// The single user-facing value type for all traversal inputs and outputs.
///
/// Scalar primitives convert automatically via `From` impls:
/// - `.has("age", 42i32)` — `42i32` becomes `Value::Int32(42)`
/// - `.property("name", "alice")` — `"alice"` becomes `Value::String("alice")`
///
/// Query results come back as the same variants:
/// - `.values("age").next()` → `Some(Value::Int32(42))`
/// - `.V([1]).next()` → `Some(Value::Vertex(Vertex { id: 1, … }))`
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    // ── Scalars ───────────────────────────────────────────────────────────────
    Null,
    Bool(bool),
    Int32(i32),
    Int64(i64),
    Float32(f32),
    Float64(f64),
    String(String),
    Uuid(u128),

    // ── Graph elements ────────────────────────────────────────────────────────
    /// A fully materialized vertex. Produced by `.V()`, `.out()`, etc.
    Vertex(Vertex),
    /// A fully materialized directed edge. Produced by `.outE()`, `.inE()`, etc.
    Edge(Edge),
    /// A property element. Produced by `.properties("name")`.
    Property(Property),

    // ── Containers ────────────────────────────────────────────────────────────
    /// Ordered list. Produced by `fold()` or `to_list()` used as a pipeline step.
    List(Vec<Value>),
    /// Ordered key-value map. Produced by `valueMap()`, `elementMap()`, `group()`.
    Map(Map),
    /// Traversal path with per-position step labels. Produced by `path()`.
    Path(Path),
}

// ── From impls for scalar Rust types ─────────────────────────────────────────

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::Int32(v)
    }
}
impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Int64(v)
    }
}
impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Value::Float32(v)
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Float64(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::String(v.to_owned())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::String(v)
    }
}
impl From<u128> for Value {
    fn from(v: u128) -> Self {
        Value::Uuid(v)
    }
}

// ── Vertex ────────────────────────────────────────────────────────────────────

/// A fully materialized vertex with all its properties.
///
/// `label_id` is the numeric label identifier from the schema registry.
/// To resolve it to a human-readable string, use
/// [`Schema::vertex_label_str`](crate::schema::definition::Schema::vertex_label_str).
///
/// `properties` uses multi-cardinality: each key maps to a `Vec<Value>` to
/// support TinkerPop VertexProperty semantics.  For the common single-value
/// case, read `vertex.properties["name"][0]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Vertex {
    pub id: i64,
    pub label_id: u16,
    pub properties: HashMap<String, Vec<Value>>,
}

// ── Edge ──────────────────────────────────────────────────────────────────────

/// A fully materialized directed edge with all its properties.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub out_v: i64,
    pub in_v: i64,
    pub label_id: u16,
    pub rank: u16,
    pub properties: HashMap<String, Value>,
}

// ── Property ──────────────────────────────────────────────────────────────────

/// A property element flowing through the pipeline (output of `.properties("name")`).
///
/// `value` is `Box<Value>` because `Property` is a variant of `Value`; without
/// the indirection the type would be infinitely recursive.  In practice `value`
/// is always a scalar variant.
#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    pub key: String,
    pub value: Box<Value>,
}

// ── Map ───────────────────────────────────────────────────────────────────────

/// An ordered key-value map (output of `valueMap()`, `elementMap()`, etc.).
///
/// Uses `Vec<(Value, Value)>` rather than `HashMap` to:
/// - Preserve insertion order (required by TinkerPop semantics)
/// - Avoid requiring `Hash + Eq` on `Value` (floats are not `Hash`)
/// - Support non-string keys (needed for `elementMap()` and `group()`)
///
/// Use [`Map::get_str`] for the dominant case of string-keyed property maps.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Map {
    pub entries: Vec<(Value, Value)>,
}

impl Map {
    pub fn new() -> Self {
        Map { entries: Vec::new() }
    }

    pub fn insert(&mut self, key: impl Into<Value>, value: impl Into<Value>) {
        self.entries.push((key.into(), value.into()));
    }

    /// Look up by string key — shortcut for the dominant string-keyed case.
    pub fn get_str(&self, key: &str) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| matches!(k, Value::String(s) if s == key)).map(|(_, v)| v)
    }

    pub fn get(&self, key: &Value) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Value, &Value)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ── Path ──────────────────────────────────────────────────────────────────────

/// A sequence of traversal positions, each tagged with zero or more step labels.
///
/// `labels[i]` is the set of `as("x")` names that tagged position `i`,
/// or an empty `Vec` when the position is unnamed.
///
/// Use [`Path::select`] to retrieve the value at a named position.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Path {
    pub objects: Vec<Value>,
    /// Parallel to `objects`; each element is the set of step labels at that position.
    pub labels: Vec<Vec<String>>,
}

impl Path {
    /// Return the value at the first position tagged with `label`.
    pub fn select(&self, label: &str) -> Option<&Value> {
        self.labels.iter().position(|ls| ls.iter().any(|l| l == label)).map(|i| &self.objects[i])
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }
}
