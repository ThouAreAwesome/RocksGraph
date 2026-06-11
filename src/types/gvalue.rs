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

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::Arc,
};

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
    Float32(f32),
    Float64(f64),
    String(SmolStr),
    Uuid(u128),
    Null,
}

impl PartialEq for Primitive {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Int32(a), Self::Int32(b)) => a == b,
            (Self::Int64(a), Self::Int64(b)) => a == b,
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

impl Hash for Primitive {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Bool(v) => v.hash(state),
            Self::Int32(v) => v.hash(state),
            Self::Int64(v) => v.hash(state),
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
/// `Arc<Vertex>` / `Arc<Edge>` references when it needs property data.
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
    Scalar(Primitive),
    List(Arc<Vec<GValue>>),
    Map(Arc<HashMap<GValue, GValue>>),
    Path(Arc<Vec<GValue>>),
}

impl PartialEq for GValue {
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
