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

//! Type-bridge utilities shared by the Rust DSL and any future query front-end.
//!
//! These functions sit between the user-facing [`gremlin::value`](super::value)
//! types and the internal [`Primitive`] / [`PropKey`] / [`LogicalStep`] types.
//! Nothing here depends on the traversal builder or execution context.

use smallvec::{smallvec, SmallVec};
use smol_str::SmolStr;

use crate::{
    gremlin::value::{Key, Predicate, Value},
    planner::logical_step::{HasIdStep, HasLabelStep, HasPropertyStep, LogicalStep},
    types::{
        gvalue::Primitive,
        prop_key::{PropKey, ID, LABEL},
        StoreError,
    },
};

/// Convert a user-facing [`Value`] scalar to the internal [`Primitive`].
///
/// Returns `None` for non-scalar values (Vertex, Edge, Property, List, Map, Path)
/// which cannot be stored as property values or used as filter scalars.
pub(crate) fn value_to_primitive(v: Value) -> Option<Primitive> {
    match v {
        Value::Null => Some(Primitive::Null),
        Value::Bool(b) => Some(Primitive::Bool(b)),
        Value::Int32(n) => Some(Primitive::Int32(n)),
        Value::Int64(n) => Some(Primitive::Int64(n)),
        Value::UInt16(n) => Some(Primitive::UInt16(n)),
        Value::Float32(f) => Some(Primitive::Float32(f)),
        Value::Float64(f) => Some(Primitive::Float64(f)),
        Value::String(s) => Some(Primitive::String(SmolStr::from(s))),
        Value::Uuid(u) => Some(Primitive::Uuid(u)),
        _ => None,
    }
}

/// Convert the internal [`Primitive`] to a user-facing [`Value`] scalar.
pub(crate) fn primitive_to_value(p: Primitive) -> Value {
    match p {
        Primitive::Null => Value::Null,
        Primitive::Bool(b) => Value::Bool(b),
        Primitive::Int32(n) => Value::Int32(n),
        Primitive::Int64(n) => Value::Int64(n),
        Primitive::UInt16(n) => Value::UInt16(n),
        Primitive::Float32(f) => Value::Float32(f),
        Primitive::Float64(f) => Value::Float64(f),
        Primitive::String(s) => Value::String(s.to_string()),
        Primitive::Uuid(u) => Value::Uuid(u),
    }
}

/// Convert a [`Key`] to the internal [`PropKey`].
///
/// `Key::Id` → `"id"`, `Key::Label` → `"label"` — the reserved strings that
/// [`element::Vertex::get_value`](crate::types::element::Vertex::get_value)
/// and [`element::Edge::get_value`](crate::types::element::Edge::get_value)
/// handle specially without a props scan.
pub(crate) fn key_to_prop_key(k: Key) -> PropKey {
    match k {
        Key::Id => ID.clone(),
        Key::Label => LABEL.clone(),
        Key::Property(s) => s,
    }
}

/// Push the appropriate [`LogicalStep`] for a `.has(key, pred)` call.
///
/// Routing:
/// - `Key::Id`  + `Predicate::Eq(Int64)` or `Within([Int64…])` → `HasIdStep`
/// - `Key::Label` + `Predicate::Eq(String|Int32|Int64)` or `Within` → `HasLabelStep` (the usual case is a string label
///   name, e.g. `.has(Key::Label, "person")`)
/// - `Key::Property(s)` + `Predicate::Eq(scalar)` → `HasPropertyStep`
/// - Other combos → no-op (use dedicated step methods instead)
pub(crate) fn push_has_step(steps: &mut Vec<LogicalStep>, key: Key, pred: Predicate) -> Result<(), StoreError> {
    match key {
        Key::Id => {
            let ids: SmallVec<[i64; 4]> = match pred {
                Predicate::Eq(Value::Int64(n)) => smallvec![n],
                Predicate::Eq(Value::Int32(n)) => smallvec![n as i64],
                Predicate::Within(vs) => {
                    let mut parsed = SmallVec::new();
                    for v in vs {
                        match v {
                            Value::Int64(n) => parsed.push(n),
                            Value::Int32(n) => parsed.push(n as i64),
                            other => {
                                return Err(StoreError::UnexpectedDataType(format!(
                                    "ID has-filter expects i32 or i64, got {:?}",
                                    other
                                )))
                            }
                        }
                    }
                    parsed
                }
                other => {
                    return Err(StoreError::UnsupportedOperation(format!(
                        "Unsupported predicate for ID has-filter, got: {:?}",
                        other
                    )))
                }
            };
            steps.push(LogicalStep::HasId(HasIdStep { ids }));
        }
        Key::Label => {
            let labels: SmallVec<[SmolStr; 4]> = match pred {
                Predicate::Eq(Value::String(s)) => smallvec![SmolStr::from(s)],
                Predicate::Eq(Value::Int32(n)) => smallvec![SmolStr::from(n.to_string())],
                Predicate::Eq(Value::Int64(n)) => smallvec![SmolStr::from(n.to_string())],
                Predicate::Within(vs) => {
                    let mut parsed = SmallVec::new();
                    for v in vs {
                        match v {
                            Value::String(s) => parsed.push(SmolStr::from(s)),
                            Value::Int32(n) => parsed.push(SmolStr::from(n.to_string())),
                            Value::Int64(n) => parsed.push(SmolStr::from(n.to_string())),
                            other => {
                                return Err(StoreError::UnexpectedDataType(format!(
                                    "Label has-filter expects String, i32 or i64, got {:?}",
                                    other
                                )))
                            }
                        }
                    }
                    parsed
                }
                other => {
                    return Err(StoreError::UnsupportedOperation(format!(
                        "Unsupported predicate for Label has-filter, got: {:?}",
                        other
                    )))
                }
            };
            steps.push(LogicalStep::HasLabel(HasLabelStep { labels }));
        }
        Key::Property(s) => match pred {
            Predicate::Eq(v) => {
                if let Some(p) = value_to_primitive(v.clone()) {
                    steps.push(LogicalStep::HasProperty(HasPropertyStep { key: s, value: p }));
                } else {
                    return Err(StoreError::UnexpectedDataType(format!(
                        "Property has-filter expects scalar value, got complex type: {:?}",
                        v
                    )));
                }
            }
            other => {
                return Err(StoreError::UnsupportedOperation(format!(
                    "Non-equality filters on user properties are not yet supported, got: {:?}",
                    other
                )))
            }
        },
    }
    Ok(())
}
