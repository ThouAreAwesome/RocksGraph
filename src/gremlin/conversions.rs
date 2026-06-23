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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gremlin::value::ne;

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
    fn test_key_to_prop_key() {
        assert_eq!(key_to_prop_key(Key::Id), crate::types::prop_key::ID.clone());
        assert_eq!(key_to_prop_key(Key::Label), crate::types::prop_key::LABEL.clone());
        assert_eq!(key_to_prop_key(Key::Property("name".into())), SmolStr::from("name"));
    }

    #[test]
    fn test_push_has_step_errors() {
        let mut steps = Vec::new();

        // ID error cases
        assert!(push_has_step(&mut steps, Key::Id, ne(10i32)).is_err());
        assert!(push_has_step(&mut steps, Key::Id, Predicate::Within(vec![Value::Null])).is_err());

        // Label error cases
        assert!(push_has_step(&mut steps, Key::Label, ne("person")).is_err());
        assert!(push_has_step(&mut steps, Key::Label, Predicate::Within(vec![Value::Null])).is_err());

        // Property error cases
        assert!(push_has_step(&mut steps, Key::Property("age".into()), ne(10i32)).is_err());
        assert!(push_has_step(&mut steps, Key::Property("age".into()), Predicate::Eq(Value::List(vec![]))).is_err());

        // Success cases
        assert!(push_has_step(&mut steps, Key::Id, Predicate::Eq(Value::Int64(42))).is_ok());
        assert!(push_has_step(&mut steps, Key::Id, Predicate::Eq(Value::Int32(42))).is_ok());
        assert!(push_has_step(&mut steps, Key::Id, Predicate::Within(vec![Value::Int64(42)])).is_ok());
        assert!(push_has_step(&mut steps, Key::Id, Predicate::Within(vec![Value::Int32(42)])).is_ok());

        assert!(push_has_step(&mut steps, Key::Label, Predicate::Eq(Value::String("person".to_string()))).is_ok());
        assert!(push_has_step(&mut steps, Key::Label, Predicate::Eq(Value::Int32(1))).is_ok());
        assert!(push_has_step(&mut steps, Key::Label, Predicate::Eq(Value::Int64(1))).is_ok());
        assert!(
            push_has_step(&mut steps, Key::Label, Predicate::Within(vec![Value::String("person".to_string())])).is_ok()
        );
        assert!(push_has_step(&mut steps, Key::Label, Predicate::Within(vec![Value::Int32(1)])).is_ok());
        assert!(push_has_step(&mut steps, Key::Label, Predicate::Within(vec![Value::Int64(1)])).is_ok());

        assert!(push_has_step(&mut steps, Key::Property("age".into()), Predicate::Eq(Value::Int32(42))).is_ok());
    }
}
