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

use smol_str::SmolStr;

use crate::{
    gremlin::value::{Predicate, Value},
    planner::logical_step::{HasPropertyStep, LogicalStep},
    types::{
        gvalue::{Primitive, PrimitivePredicate},
        StoreError,
    },
};

/// Convert a user-facing [`Predicate`] (which holds [`Value`]) to the internal [`PrimitivePredicate`] (which holds [`Primitive`]).
pub(crate) fn predicate_to_primitive_predicate(pred: Predicate) -> Result<PrimitivePredicate, StoreError> {
    let to_prim = |v: Value| -> Result<Primitive, StoreError> {
        value_to_primitive(v.clone())
            .ok_or_else(|| StoreError::UnexpectedDataType(format!("Expected scalar value for predicate, got: {:?}", v)))
    };
    match pred {
        Predicate::Eq(v) => Ok(PrimitivePredicate::Eq(to_prim(v)?)),
        Predicate::Ne(v) => Ok(PrimitivePredicate::Ne(to_prim(v)?)),
        Predicate::Gt(v) => Ok(PrimitivePredicate::Gt(to_prim(v)?)),
        Predicate::Gte(v) => Ok(PrimitivePredicate::Gte(to_prim(v)?)),
        Predicate::Lt(v) => Ok(PrimitivePredicate::Lt(to_prim(v)?)),
        Predicate::Lte(v) => Ok(PrimitivePredicate::Lte(to_prim(v)?)),
        Predicate::Between(lo, hi) => Ok(PrimitivePredicate::Between(to_prim(lo)?, to_prim(hi)?)),
        Predicate::Within(vs) => {
            let mut prims = Vec::with_capacity(vs.len());
            for v in vs {
                prims.push(to_prim(v)?);
            }
            Ok(PrimitivePredicate::Within(prims))
        }
        Predicate::Without(vs) => {
            let mut prims = Vec::with_capacity(vs.len());
            for v in vs {
                prims.push(to_prim(v)?);
            }
            Ok(PrimitivePredicate::Without(prims))
        }
    }
}

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

fn validate_label_value(val: &Value) -> Result<(), StoreError> {
    match val {
        Value::String(_) => Ok(()),
        other => Err(StoreError::UnexpectedDataType(format!("Label has-filter expects String, got {:?}", other))),
    }
}

/// Labels are string-only and unordered — `eq`/`ne`/`within`/`without` are meaningful,
/// `gt`/`gte`/`lt`/`lte`/`between` (lexicographic ordering on a label name) are not.
pub(crate) fn validate_label_predicate(pred: &Predicate) -> Result<(), StoreError> {
    match pred {
        Predicate::Eq(v) | Predicate::Ne(v) => {
            validate_label_value(v)?;
        }
        Predicate::Within(vs) | Predicate::Without(vs) => {
            for v in vs {
                validate_label_value(v)?;
            }
        }
        other => {
            return Err(StoreError::UnsupportedOperation(format!(
                "Unsupported predicate for Label has-filter, got: {:?}",
                other
            )));
        }
    }
    Ok(())
}

fn validate_property_value(val: &Value) -> Result<(), StoreError> {
    if value_to_primitive(val.clone()).is_none() {
        return Err(StoreError::UnexpectedDataType(format!(
            "Property has-filter expects scalar value, got complex type: {:?}",
            val
        )));
    }
    Ok(())
}

fn validate_property_predicate(pred: &Predicate) -> Result<(), StoreError> {
    match pred {
        Predicate::Eq(v)
        | Predicate::Ne(v)
        | Predicate::Gt(v)
        | Predicate::Gte(v)
        | Predicate::Lt(v)
        | Predicate::Lte(v) => {
            validate_property_value(v)?;
        }
        Predicate::Between(lo, hi) => {
            validate_property_value(lo)?;
            validate_property_value(hi)?;
        }
        Predicate::Within(vs) | Predicate::Without(vs) => {
            for v in vs {
                validate_property_value(v)?;
            }
        }
    }
    Ok(())
}

/// Push a [`LogicalStep::HasProperty`] for a `.has(key, pred)` call.
///
/// `"id"`/`"label"`/`"rank"` are rejected later, at physical-build time (see
/// `reject_reserved_key` in `engine/volcano/builder/build_step.rs`) — not here, since a
/// `.has("rank", ...)` immediately following an edge-traversal step is still expected to
/// fold into that step's structural rank field via `merge_end_vertex_filter`, and folding
/// happens between this call and physical build.
pub(crate) fn push_has_step(steps: &mut Vec<LogicalStep>, key: SmolStr, pred: Predicate) -> Result<(), StoreError> {
    validate_property_predicate(&pred)?;
    let prim_pred = predicate_to_primitive_predicate(pred)?;
    steps.push(LogicalStep::HasProperty(HasPropertyStep { key, pred: prim_pred }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_value_to_primitive() {
        assert_eq!(value_to_primitive(Value::Null), Some(Primitive::Null));
        assert_eq!(value_to_primitive(Value::Bool(true)), Some(Primitive::Bool(true)));
        assert_eq!(value_to_primitive(Value::Int32(42)), Some(Primitive::Int32(42)));
        assert_eq!(value_to_primitive(Value::Int64(42)), Some(Primitive::Int64(42)));
        assert_eq!(value_to_primitive(Value::UInt16(5)), Some(Primitive::UInt16(5)));
        assert_eq!(value_to_primitive(Value::Float32(1.23)), Some(Primitive::Float32(1.23)));
        assert_eq!(value_to_primitive(Value::Float64(1.23)), Some(Primitive::Float64(1.23)));
        assert_eq!(value_to_primitive(Value::String("hello".to_string())), Some(Primitive::String("hello".into())));
        assert_eq!(value_to_primitive(Value::Uuid(123)), Some(Primitive::Uuid(123)));

        // Non-scalar
        assert_eq!(value_to_primitive(Value::List(vec![])), None);
    }

    #[test]
    fn test_primitive_to_value() {
        assert_eq!(primitive_to_value(Primitive::Null), Value::Null);
        assert_eq!(primitive_to_value(Primitive::Bool(true)), Value::Bool(true));
        assert_eq!(primitive_to_value(Primitive::Int32(42)), Value::Int32(42));
        assert_eq!(primitive_to_value(Primitive::Int64(42)), Value::Int64(42));
        assert_eq!(primitive_to_value(Primitive::UInt16(5)), Value::UInt16(5));
        assert_eq!(primitive_to_value(Primitive::Float32(1.23)), Value::Float32(1.23));
        assert_eq!(primitive_to_value(Primitive::Float64(1.23)), Value::Float64(1.23));
        assert_eq!(primitive_to_value(Primitive::String("hello".into())), Value::String("hello".to_string()));
        assert_eq!(primitive_to_value(Primitive::Uuid(123)), Value::Uuid(123));
    }

    #[test]
    fn test_push_has_step_errors() {
        let mut steps = Vec::new();

        // Non-scalar property values are rejected.
        assert!(push_has_step(&mut steps, "age".into(), Predicate::Eq(Value::List(vec![]))).is_err());

        // Scalar property values succeed. Reserved-key rejection ("id"/"label"/"rank")
        // happens later, at physical-build time — see `reject_reserved_key` in
        // `engine/volcano/builder/build_step.rs` — not here.
        assert!(push_has_step(&mut steps, "age".into(), Predicate::Eq(Value::Int32(42))).is_ok());
        assert!(push_has_step(&mut steps, "name".into(), Predicate::Eq(Value::String("alice".to_string()))).is_ok());
    }
}
