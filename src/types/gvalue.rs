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

//! [`GValue`] — the universal value type — and its scalar variant [`Primitive`].
//!
//! Every value that flows through a traversal pipeline is represented as a `GValue`.
//! The enum covers the full range from atomic scalars up to structured containers
//! (lists, maps, paths).
//!
//! # `Primitive` vs `GValue::Scalar`
//!
//! [`Primitive`] is the leaf scalar type.  It appears as:
//!
//! - `GValue::Scalar(Primitive)` when a bare scalar is in the pipeline (e.g. the result of `values("age")`).
//! - `Property::value` when attached to a [`Property`](crate::types::Property).
//!
//! # Path representation
//!
//! `GValue::Path` stores a `Vec<(GValue, Option<SmallVec<[SmolStr; 2]>>)>`.  The
//! second element of each pair is the set of step labels (from `as("x")`) that
//! named this position, or `None` when the position is unnamed.

use std::hash::{Hash, Hasher};

use smallvec::SmallVec;
use smol_str::SmolStr;

use crate::types::{
    element::Property,
    keys::{EdgeKey, VertexKey},
};

// ── Primitive ────────────────────────────────────────────────────────────────

/// A scalar value that can appear as a property value or standalone scalar.
#[derive(Debug, Clone)]
pub enum Primitive {
    Bool(bool),
    Int32(i32),
    Int64(i64),
    UInt16(u16),
    Float32(f32),
    Float64(f64),
    String(SmolStr),
    Uuid(u128),
    Null,
}

impl PartialEq for Primitive {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Int32(a), Self::Int32(b)) => a == b,
            (Self::Int64(a), Self::Int64(b)) => a == b,
            (Self::UInt16(a), Self::UInt16(b)) => a == b,
            (Self::Float32(a), Self::Float32(b)) => a.to_bits() == b.to_bits(),
            (Self::Float64(a), Self::Float64(b)) => a.to_bits() == b.to_bits(),
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Uuid(a), Self::Uuid(b)) => a == b,
            (Self::Null, Self::Null) => true,
            _ => false,
        }
    }
}

impl Eq for Primitive {}

impl Primitive {
    #[inline]
    pub fn is_integer(&self) -> bool {
        matches!(self, Self::Int32(_) | Self::Int64(_) | Self::UInt16(_))
    }

    #[inline]
    pub fn is_numeric(&self) -> bool {
        matches!(self, Self::Int32(_) | Self::Int64(_) | Self::UInt16(_) | Self::Float32(_) | Self::Float64(_))
    }

    #[inline]
    pub fn to_i64(&self) -> Option<i64> {
        match self {
            Self::Int32(v) => Some(*v as i64),
            Self::Int64(v) => Some(*v),
            Self::UInt16(v) => Some(*v as i64),
            _ => None,
        }
    }

    #[inline]
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

    /// Equality that treats numeric values as equal across width/representation
    /// (e.g. `Int32(2) loose_eq Int64(2)` is `true`), unlike derived `PartialEq` which only
    /// matches identical variants. Mirrors the numeric normalization `partial_cmp` already
    /// does for `Gt`/`Lt`/etc., so `Eq`/`Ne`/`Within`/`Without` predicates compare like-for-like
    /// regardless of which integer width a caller's literal happened to use.
    #[inline]
    fn loose_eq(&self, other: &Self) -> bool {
        if self.is_integer() && other.is_integer() {
            return self.to_i64() == other.to_i64();
        }
        if self.is_numeric() && other.is_numeric() {
            return self.to_f64() == other.to_f64();
        }
        self == other
    }
}

impl PartialOrd for Primitive {
    #[inline]
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

impl Hash for Primitive {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Bool(v) => v.hash(state),
            Self::Int32(v) => v.hash(state),
            Self::Int64(v) => v.hash(state),
            Self::UInt16(v) => v.hash(state),
            Self::Float32(v) => v.to_bits().hash(state),
            Self::Float64(v) => v.to_bits().hash(state),
            Self::String(v) => v.hash(state),
            Self::Uuid(v) => v.hash(state),
            Self::Null => {}
        }
    }
}

// ── Primitive conversions ─────────────────────────────────────────────────────

impl From<bool> for Primitive {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}
impl From<i32> for Primitive {
    fn from(v: i32) -> Self {
        Self::Int32(v)
    }
}
impl From<i64> for Primitive {
    fn from(v: i64) -> Self {
        Self::Int64(v)
    }
}
impl From<u16> for Primitive {
    fn from(v: u16) -> Self {
        Self::UInt16(v)
    }
}
impl From<f32> for Primitive {
    fn from(v: f32) -> Self {
        Self::Float32(v)
    }
}
impl From<f64> for Primitive {
    fn from(v: f64) -> Self {
        Self::Float64(v)
    }
}
impl From<&str> for Primitive {
    fn from(v: &str) -> Self {
        Self::String(SmolStr::new(v))
    }
}
impl From<String> for Primitive {
    fn from(v: String) -> Self {
        Self::String(SmolStr::from(v))
    }
}
impl From<SmolStr> for Primitive {
    fn from(v: SmolStr) -> Self {
        Self::String(v)
    }
}

// ── GValue ───────────────────────────────────────────────────────────────────

/// The universal in-memory value type flowing through a traversal pipeline.
///
/// `Vertex` wraps a `VertexKey`; `Edge` wraps an `EdgeKey` (direction-aware).
/// The engine calls `ctx.get_vertex(key)` / `ctx.get_edges(…)` to obtain
/// `Rc<Vertex>` / `Rc<Edge>` references when it needs property data.
///
/// Both key types are `Copy` (8 / 24 bytes), so `GValue` is cheap to clone.
#[derive(Debug, Clone)]
pub enum GValue {
    /// A vertex identified by its store key.
    Vertex(VertexKey),
    /// A directed edge.  The `EdgeKey` preserves traversal direction (Out / In)
    /// for `path()` / `select()` identity.
    Edge(EdgeKey),
    /// A property travelling through the pipeline as a standalone element.
    Property(Property),
    /// A bare scalar value (e.g. result of `values("age")`).
    Scalar(Primitive),
    /// An ordered list of values (e.g. result of `fold()`).
    List(Vec<GValue>),
    /// A key-value map (e.g. result of `valueMap()`).
    #[allow(dead_code)]
    Map(Vec<(GValue, GValue)>),
    /// A sequence of traversal positions with optional step labels.
    ///
    /// Each entry is `(value, labels)` where `labels` is the set of `as("x")`
    /// names that tagged that position, or `None` when the position is unnamed.
    Path(Vec<(GValue, Option<SmallVec<[SmolStr; 2]>>)>),
}

impl PartialEq for GValue {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Vertex(a), Self::Vertex(b)) => a == b,
            (Self::Edge(a), Self::Edge(b)) => a == b,
            (Self::Property(a), Self::Property(b)) => a == b,
            (Self::Scalar(a), Self::Scalar(b)) => a == b,
            (Self::List(a), Self::List(b)) => a == b,
            (Self::Map(a), Self::Map(b)) => a == b,
            (Self::Path(a), Self::Path(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for GValue {}

impl Hash for GValue {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Vertex(key) => key.hash(state),
            Self::Edge(key) => key.hash(state),
            Self::Property(p) => p.hash(state),
            Self::Scalar(p) => p.hash(state),
            Self::List(list) => {
                list.len().hash(state);
                for item in list.iter() {
                    item.hash(state);
                }
            }
            Self::Map(map) => {
                map.len().hash(state);
                for (k, v) in map.iter() {
                    k.hash(state);
                    v.hash(state);
                }
            }
            Self::Path(path) => {
                path.len().hash(state);
                for item in path.iter() {
                    item.hash(state);
                }
            }
        }
    }
}

// ── PrimitivePredicate ────────────────────────────────────────────────────────

/// A low-level comparison predicate evaluated against `Primitive` values in the storage and execution engine layers.
///
/// Decouples engine filters and planner steps from the top-layer user-facing `Predicate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrimitivePredicate {
    Eq(Primitive),
    Ne(Primitive),
    Gt(Primitive),
    Gte(Primitive),
    Lt(Primitive),
    Lte(Primitive),
    Between(Primitive, Primitive),
    Within(Vec<Primitive>),
    Without(Vec<Primitive>),
}

impl PrimitivePredicate {
    #[inline]
    pub fn evaluate(&self, val: &Primitive) -> bool {
        match self {
            Self::Eq(p) => val.loose_eq(p),
            Self::Ne(p) => !val.loose_eq(p),
            Self::Gt(p) => matches!(val.partial_cmp(p), Some(std::cmp::Ordering::Greater)),
            Self::Gte(p) => matches!(val.partial_cmp(p), Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)),
            Self::Lt(p) => matches!(val.partial_cmp(p), Some(std::cmp::Ordering::Less)),
            Self::Lte(p) => matches!(val.partial_cmp(p), Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)),
            Self::Between(lo, hi) => {
                matches!(val.partial_cmp(lo), Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal))
                    && matches!(val.partial_cmp(hi), Some(std::cmp::Ordering::Less))
            }
            Self::Within(vs) => vs.iter().any(|v| val.loose_eq(v)),
            Self::Without(vs) => !vs.iter().any(|v| val.loose_eq(v)),
        }
    }

    /// Applies `f` to every leaf [`Primitive`] this predicate carries, preserving its shape.
    ///
    /// Physical steps use this to normalize predicate literals into the exact runtime
    /// representation they'll be compared against — once, at construction time — instead of
    /// converting the runtime value to match the predicate's representation on every traverser.
    /// E.g. `HasPropertyStep::new` maps a `rank` literal to its canonical `UInt16` form;
    /// `PhysicalPlanBuilder` maps a label name to its interned `label_id`.
    pub fn map(self, f: impl Fn(Primitive) -> Primitive) -> Self {
        match self {
            Self::Eq(v) => Self::Eq(f(v)),
            Self::Ne(v) => Self::Ne(f(v)),
            Self::Gt(v) => Self::Gt(f(v)),
            Self::Gte(v) => Self::Gte(f(v)),
            Self::Lt(v) => Self::Lt(f(v)),
            Self::Lte(v) => Self::Lte(f(v)),
            Self::Between(lo, hi) => Self::Between(f(lo), f(hi)),
            Self::Within(vs) => Self::Within(vs.into_iter().map(f).collect()),
            Self::Without(vs) => Self::Without(vs.into_iter().map(f).collect()),
        }
    }

    /// Borrowing counterpart to [`map`](Self::map) — iterates every leaf [`Primitive`] this
    /// predicate carries without transforming it. Used for read-only checks over a predicate's
    /// values, e.g. validating that each one names a declared label.
    pub fn values(&self) -> Box<dyn Iterator<Item = &Primitive> + '_> {
        match self {
            Self::Eq(v) | Self::Ne(v) | Self::Gt(v) | Self::Gte(v) | Self::Lt(v) | Self::Lte(v) => {
                Box::new(std::iter::once(v))
            }
            Self::Between(lo, hi) => Box::new([lo, hi].into_iter()),
            Self::Within(vs) | Self::Without(vs) => Box::new(vs.iter()),
        }
    }
}
