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
//! # Reserved keys
//!
//! `"id"`, `"label"`, and `"rank"` are reserved — `.has()`/`.values()`/`.properties()`
//! reject them. Use the dedicated steps instead: [`id()`](crate::TraversalBuilder::id) /
//! [`hasId()`](crate::TraversalBuilder::hasId), [`label()`](crate::TraversalBuilder::label) /
//! [`hasLabel()`](crate::TraversalBuilder::hasLabel), [`rank()`](crate::TraversalBuilder::rank) /
//! [`hasRank()`](crate::TraversalBuilder::hasRank). See `docs/design_reserved_keys.md`.
//!
//! ```
//! # use rocksgraph::{Graph, TraversalBuilder, Value};
//! # let dir = tempfile::tempdir().unwrap();
//! # let graph = Graph::open(dir.path()).unwrap();
//! # let mut tx = graph.begin();
//! # tx.g().addV("person").property("id", 42i64).next().unwrap();
//! # tx.commit().unwrap();
//! let mut snap = graph.read();
//! // filter by id
//! snap.g().V([]).hasId([42i64]).next().unwrap();
//! // extract id as a scalar
//! let id = snap.g().V([42]).id().next().unwrap();
//! assert_eq!(id, Some(Value::Int64(42)));
//! # graph.close().unwrap();
//! ```
//!
//! # Predicate
//!
//! [`Predicate`] wraps a comparison operator and value for filter steps.
//! Bare scalars implicitly become [`Predicate::Eq`] via `From` impls, so
//! `.has("age", 42i32)` is equivalent to `.has("age", eq(42i32))`.

use crate::types::StoreError;
use smol_str::SmolStr;
use std::collections::HashMap;

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

// Note: `Predicate` (this Value-based type) is never evaluated directly. The only evaluation
// path is `types::gvalue::PrimitivePredicate::evaluate`, which the engine's physical steps
// (has_id/has_label/has_property/scalar_filter) call exclusively — keeping `engine` free of any
// dependency on `gremlin`. `gremlin::type_bridge::predicate_to_primitive_predicate` converts a
// `Predicate` into a `PrimitivePredicate` once, at plan-build time, before it ever reaches the
// engine.

/// Any type that converts to [`Value`] also converts to [`Predicate::Eq`].
///
/// This covers all scalar Rust types (`i32`, `i64`, `bool`, `&str`, `f64`, …)
/// and lets callers write `.has("age", 42i32)` without wrapping in `eq(...)`.
impl<T: Into<Value>> From<T> for Predicate {
    fn from(v: T) -> Self {
        Predicate::Eq(v.into())
    }
}

/// A fixed-size array of scalars converts to [`Predicate::Eq`] (one element) or
/// [`Predicate::Within`] (more than one) — the same collapsing rule `hasId()`/
/// `hasLabel()`/`hasRank()` used internally before they accepted a full `Predicate`,
/// now expressed once, here. Lets `.hasId([1, 2, 3])` keep working unchanged while
/// `.hasId(gt(2))`/`.hasId(within([...]))`/etc. also become valid.
impl<T: Into<Value>, const N: usize> From<[T; N]> for Predicate {
    fn from(vs: [T; N]) -> Self {
        let mut values: Vec<Value> = vs.into_iter().map(Into::into).collect();
        if values.len() == 1 {
            Predicate::Eq(values.pop().unwrap())
        } else {
            Predicate::Within(values)
        }
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
    /// Unsigned 16-bit integer. The canonical type for an edge's `rank`
    /// (see [`Edge::rank`]) — `.property("rank", 5u16)` and the value
    /// `.values(["rank"])` returns both use this variant.
    UInt16(u16),
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

impl Value {
    pub fn is_integer(&self) -> bool {
        matches!(self, Self::Int32(_) | Self::Int64(_) | Self::UInt16(_))
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self, Self::Int32(_) | Self::Int64(_) | Self::UInt16(_) | Self::Float32(_) | Self::Float64(_))
    }

    pub fn to_i64(&self) -> Option<i64> {
        match self {
            Self::Int32(v) => Some(*v as i64),
            Self::Int64(v) => Some(*v),
            Self::UInt16(v) => Some(*v as i64),
            _ => None,
        }
    }

    pub fn to_f64(&self) -> Option<f64> {
        match self {
            Self::Int32(v) => Some(*v as f64),
            Self::Int64(v) => Some(*v as f64),
            Self::UInt16(v) => Some(*v as f64),
            Self::Float32(v) => Some(*v as f64),
            Self::Float64(v) => Some(*v),
            _ => None,
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if self.is_integer() && other.is_integer() {
            return self.to_i64().unwrap().partial_cmp(&other.to_i64().unwrap());
        }
        if self.is_numeric() && other.is_numeric() {
            return self.to_f64().unwrap().partial_cmp(&other.to_f64().unwrap());
        }
        match (self, other) {
            (Self::Bool(a), Self::Bool(b)) => a.partial_cmp(b),
            (Self::String(a), Self::String(b)) => a.partial_cmp(b),
            (Self::Uuid(a), Self::Uuid(b)) => a.partial_cmp(b),
            (Self::Null, Self::Null) => Some(std::cmp::Ordering::Equal),
            _ => None,
        }
    }
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
impl From<u16> for Value {
    fn from(v: u16) -> Self {
        Value::UInt16(v)
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
/// `label` is the label's string name (e.g. `"person"`), already resolved from the
/// internal numeric `label_id` via the schema registry at materialization time —
/// consistent with what `.label()` returns.
///
/// `label` and the `properties` keys are [`SmolStr`] rather than `String`: both are drawn
/// from the schema's interned label/property-key registry (crate-internal), so materializing a
/// result can clone or move the existing `SmolStr` instead of heap-allocating a fresh `String`
/// per element.
/// `SmolStr` derefs to `&str`, so lookups/comparisons against string literals are unaffected
/// (e.g. `vertex.properties.get("name")`, `edge.label == "knows"`).
///
/// `properties` uses multi-cardinality: each key maps to a `Vec<Value>` to
/// support TinkerPop VertexProperty semantics.  For the common single-value
/// case, read `vertex.properties["name"][0]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Vertex {
    pub id: i64,
    pub label: SmolStr,
    pub properties: HashMap<SmolStr, Vec<Value>>,
}

// ── Edge ──────────────────────────────────────────────────────────────────────

/// A fully materialized directed edge with all its properties.
///
/// `label` is the label's string name, resolved from the internal numeric
/// `label_id` at materialization time (see [`Vertex::label`]).
///
/// `rank` is `u16` end to end: `.property("rank", 5u16)` on write, [`Value::UInt16`] from
/// `.values(["rank"])` on generic read, and this raw `u16` field on full materialization —
/// no widening through `Int32`/`Int64` at any point.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub id: String,
    pub out_v: i64,
    pub in_v: i64,
    pub label: SmolStr,
    pub rank: u16,
    pub properties: HashMap<SmolStr, Value>,
}

// ── Property ──────────────────────────────────────────────────────────────────

/// A property element flowing through the pipeline (output of `.properties("name")`).
///
/// `value` is `Box<Value>` because `Property` is a variant of `Value`; without
/// the indirection the type would be infinitely recursive.  In practice `value`
/// is always a scalar variant.
#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    pub key: SmolStr,
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

// ── Value Conversion Helpers & TryFrom ──────────────────────────────────────

impl Value {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Value::Int32(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int64(n) => Some(*n),
            Value::Int32(n) => Some(*n as i64),
            _ => None,
        }
    }

    pub fn as_u16(&self) -> Option<u16> {
        match self {
            Value::UInt16(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Value::Float32(f) => Some(*f),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float64(f) => Some(*f),
            Value::Float32(f) => Some(*f as f64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_uuid(&self) -> Option<u128> {
        match self {
            Value::Uuid(u) => Some(*u),
            _ => None,
        }
    }

    pub fn as_vertex(&self) -> Option<&Vertex> {
        match self {
            Value::Vertex(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_edge(&self) -> Option<&Edge> {
        match self {
            Value::Edge(e) => Some(e),
            _ => None,
        }
    }
}

impl TryFrom<Value> for bool {
    type Error = StoreError;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Bool(b) => Ok(b),
            other => Err(StoreError::UnexpectedDataType(format!("Expected Bool, got {:?}", other))),
        }
    }
}

impl TryFrom<Value> for i32 {
    type Error = StoreError;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Int32(n) => Ok(n),
            other => Err(StoreError::UnexpectedDataType(format!("Expected Int32, got {:?}", other))),
        }
    }
}

impl TryFrom<Value> for i64 {
    type Error = StoreError;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Int64(n) => Ok(n),
            Value::Int32(n) => Ok(n as i64),
            other => Err(StoreError::UnexpectedDataType(format!("Expected Int64 or Int32, got {:?}", other))),
        }
    }
}

impl TryFrom<Value> for u16 {
    type Error = StoreError;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::UInt16(n) => Ok(n),
            other => Err(StoreError::UnexpectedDataType(format!("Expected UInt16, got {:?}", other))),
        }
    }
}

impl TryFrom<Value> for f32 {
    type Error = StoreError;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Float32(f) => Ok(f),
            other => Err(StoreError::UnexpectedDataType(format!("Expected Float32, got {:?}", other))),
        }
    }
}

impl TryFrom<Value> for f64 {
    type Error = StoreError;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Float64(f) => Ok(f),
            Value::Float32(f) => Ok(f as f64),
            other => Err(StoreError::UnexpectedDataType(format!("Expected Float64 or Float32, got {:?}", other))),
        }
    }
}

impl TryFrom<Value> for String {
    type Error = StoreError;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::String(s) => Ok(s),
            other => Err(StoreError::UnexpectedDataType(format!("Expected String, got {:?}", other))),
        }
    }
}

impl TryFrom<Value> for u128 {
    type Error = StoreError;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Uuid(u) => Ok(u),
            other => Err(StoreError::UnexpectedDataType(format!("Expected Uuid, got {:?}", other))),
        }
    }
}

impl TryFrom<Value> for Vertex {
    type Error = StoreError;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Vertex(v) => Ok(v),
            other => Err(StoreError::UnexpectedDataType(format!("Expected Vertex, got {:?}", other))),
        }
    }
}

impl TryFrom<Value> for Edge {
    type Error = StoreError;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Edge(e) => Ok(e),
            other => Err(StoreError::UnexpectedDataType(format!("Expected Edge, got {:?}", other))),
        }
    }
}

impl TryFrom<Value> for Property {
    type Error = StoreError;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Property(p) => Ok(p),
            other => Err(StoreError::UnexpectedDataType(format!("Expected Property, got {:?}", other))),
        }
    }
}

impl TryFrom<Value> for Vec<Value> {
    type Error = StoreError;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::List(l) => Ok(l),
            other => Err(StoreError::UnexpectedDataType(format!("Expected List, got {:?}", other))),
        }
    }
}

impl TryFrom<Value> for Map {
    type Error = StoreError;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Map(m) => Ok(m),
            other => Err(StoreError::UnexpectedDataType(format!("Expected Map, got {:?}", other))),
        }
    }
}

impl TryFrom<Value> for Path {
    type Error = StoreError;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::Path(p) => Ok(p),
            other => Err(StoreError::UnexpectedDataType(format!("Expected Path, got {:?}", other))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_predicate_constructors() {
        assert_eq!(eq(10i32), Predicate::Eq(Value::Int32(10)));
        assert_eq!(ne(10i32), Predicate::Ne(Value::Int32(10)));
        assert_eq!(gt(10i32), Predicate::Gt(Value::Int32(10)));
        assert_eq!(gte(10i32), Predicate::Gte(Value::Int32(10)));
        assert_eq!(lt(10i32), Predicate::Lt(Value::Int32(10)));
        assert_eq!(lte(10i32), Predicate::Lte(Value::Int32(10)));
        assert_eq!(between(1, 10), Predicate::Between(Value::Int32(1), Value::Int32(10)));
        assert_eq!(within(vec![1, 2]), Predicate::Within(vec![Value::Int32(1), Value::Int32(2)]));
        assert_eq!(without(vec![1, 2]), Predicate::Without(vec![Value::Int32(1), Value::Int32(2)]));

        let p: Predicate = 10i32.into();
        assert_eq!(p, Predicate::Eq(Value::Int32(10)));
    }

    #[test]
    fn test_value_from_impls() {
        assert_eq!(Value::from(true), Value::Bool(true));
        assert_eq!(Value::from(42i32), Value::Int32(42));
        assert_eq!(Value::from(42i64), Value::Int64(42));
        assert_eq!(Value::from(5u16), Value::UInt16(5));
        assert_eq!(Value::from(1.23f32), Value::Float32(1.23));
        assert_eq!(Value::from(1.23f64), Value::Float64(1.23));
        assert_eq!(Value::from("hello"), Value::String("hello".to_string()));
        assert_eq!(Value::from("hello".to_string()), Value::String("hello".to_string()));
        assert_eq!(Value::from(123u128), Value::Uuid(123));
    }

    #[test]
    fn test_map_operations() {
        let mut map = Map::new();
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);

        map.insert("name", "alice");
        map.insert("age", 30i32);

        assert!(!map.is_empty());
        assert_eq!(map.len(), 2);

        assert_eq!(map.get_str("name"), Some(&Value::String("alice".to_string())));
        assert_eq!(map.get_str("age"), Some(&Value::Int32(30)));
        assert_eq!(map.get_str("nonexistent"), None);

        assert_eq!(map.get(&Value::String("name".to_string())), Some(&Value::String("alice".to_string())));
        assert_eq!(map.get(&Value::String("nonexistent".to_string())), None);

        let keys_vals: Vec<_> = map.iter().collect();
        assert_eq!(keys_vals.len(), 2);
    }

    #[test]
    fn test_path_operations() {
        let mut path = Path::default();
        assert!(path.is_empty());
        assert_eq!(path.len(), 0);

        path.objects.push(Value::Int32(42));
        path.labels.push(vec!["a".to_string()]);

        assert!(!path.is_empty());
        assert_eq!(path.len(), 1);

        assert_eq!(path.select("a"), Some(&Value::Int32(42)));
        assert_eq!(path.select("b"), None);
    }

    #[test]
    fn test_value_conversion_helpers() {
        // Bool
        assert_eq!(Value::Bool(true).as_bool(), Some(true));
        assert_eq!(Value::Null.as_bool(), None);

        // Int32
        assert_eq!(Value::Int32(42).as_i32(), Some(42));
        assert_eq!(Value::Null.as_i32(), None);

        // Int64
        assert_eq!(Value::Int64(42).as_i64(), Some(42));
        assert_eq!(Value::Int32(42).as_i64(), Some(42));
        assert_eq!(Value::Null.as_i64(), None);

        // UInt16
        assert_eq!(Value::UInt16(5).as_u16(), Some(5));
        assert_eq!(Value::Null.as_u16(), None);

        // Float32
        assert_eq!(Value::Float32(1.23).as_f32(), Some(1.23));
        assert_eq!(Value::Null.as_f32(), None);

        // Float64
        assert_eq!(Value::Float64(1.23).as_f64(), Some(1.23));
        assert_eq!(Value::Float32(1.23).as_f64(), Some(1.23f32 as f64));
        assert_eq!(Value::Null.as_f64(), None);

        // String
        assert_eq!(Value::String("hello".to_string()).as_str(), Some("hello"));
        assert_eq!(Value::Null.as_str(), None);

        // Uuid
        assert_eq!(Value::Uuid(123).as_uuid(), Some(123));
        assert_eq!(Value::Null.as_uuid(), None);

        // Vertex & Edge
        let v = Vertex { id: 1, label: "person".into(), properties: HashMap::new() };
        let ev = Value::Vertex(v.clone());
        assert_eq!(ev.as_vertex(), Some(&v));
        assert_eq!(Value::Null.as_vertex(), None);

        let e = Edge { id: "".into(), out_v: 1, in_v: 2, label: "knows".into(), rank: 0, properties: HashMap::new() };
        let ee = Value::Edge(e.clone());
        assert_eq!(ee.as_edge(), Some(&e));
        assert_eq!(Value::Null.as_edge(), None);
    }

    #[test]
    fn test_value_try_from() {
        assert!(bool::try_from(Value::Bool(true)).unwrap());
        assert!(bool::try_from(Value::Null).is_err());

        assert_eq!(i32::try_from(Value::Int32(42)).unwrap(), 42);
        assert!(i32::try_from(Value::Null).is_err());

        assert_eq!(i64::try_from(Value::Int64(42)).unwrap(), 42);
        assert_eq!(i64::try_from(Value::Int32(42)).unwrap(), 42);
        assert!(i64::try_from(Value::Null).is_err());

        assert_eq!(u16::try_from(Value::UInt16(5)).unwrap(), 5);
        assert!(u16::try_from(Value::Null).is_err());

        assert_eq!(f32::try_from(Value::Float32(1.23)).unwrap(), 1.23);
        assert!(f32::try_from(Value::Null).is_err());

        assert_eq!(f64::try_from(Value::Float64(1.23)).unwrap(), 1.23);
        assert_eq!(f64::try_from(Value::Float32(1.23f32)).unwrap(), 1.23f32 as f64);
        assert!(f64::try_from(Value::Null).is_err());

        assert_eq!(String::try_from(Value::String("hello".to_string())).unwrap(), "hello");
        assert!(String::try_from(Value::Null).is_err());

        assert_eq!(u128::try_from(Value::Uuid(123)).unwrap(), 123);
        assert!(u128::try_from(Value::Null).is_err());

        let v = Vertex { id: 1, label: "person".into(), properties: HashMap::new() };
        assert_eq!(Vertex::try_from(Value::Vertex(v.clone())).unwrap(), v);
        assert!(Vertex::try_from(Value::Null).is_err());

        let e = Edge { id: "".into(), out_v: 1, in_v: 2, label: "knows".into(), rank: 0, properties: HashMap::new() };
        assert_eq!(Edge::try_from(Value::Edge(e.clone())).unwrap(), e);
        assert!(Edge::try_from(Value::Null).is_err());

        let p = Property { key: "age".into(), value: Box::new(Value::Int32(30)) };
        assert_eq!(Property::try_from(Value::Property(p.clone())).unwrap(), p);
        assert!(Property::try_from(Value::Null).is_err());

        let list = vec![Value::Int32(1), Value::Int32(2)];
        assert_eq!(Vec::<Value>::try_from(Value::List(list.clone())).unwrap(), list);
        assert!(Vec::<Value>::try_from(Value::Null).is_err());

        let mut map = Map::new();
        map.insert("name", "alice");
        assert_eq!(Map::try_from(Value::Map(map.clone())).unwrap(), map);
        assert!(Map::try_from(Value::Null).is_err());

        let mut path = Path::default();
        path.objects.push(Value::Int32(42));
        path.labels.push(vec!["a".to_string()]);
        assert_eq!(Path::try_from(Value::Path(path.clone())).unwrap(), path);
        assert!(Path::try_from(Value::Null).is_err());
    }
}
