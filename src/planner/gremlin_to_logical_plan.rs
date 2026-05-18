// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of MultiGraph.
//
// Use of this software is governed by the Business Source License 1.1
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date (2030-01-01), in accordance with the Business Source
// License, use of this software will be governed by the Apache License 2.0.
//
// SPDX-License-Identifier: BUSL-1.1

//! Translates a deserialized Gremlin [`GremlinQueryAst`] into a [`LogicalPlan`].
//!
//! This is the bridge between the server's transport layer and the planner IR.
//! It resolves string label names to numeric [`LabelId`]s and converts
//! [`GremlinArgument`] values to typed [`Primitive`]s.
//!
//! Label resolution is currently hard-coded (see [`resolve_label_name`]); in a
//! production system this would delegate to a schema manager.
//!
//! [`GremlinQueryAst`]: crate::server::bytecode_deserializer::GremlinQueryAst
//! [`LabelId`]: crate::types::LabelId
//! [`Primitive`]: crate::types::gvalue::Primitive
//! [`GremlinArgument`]: crate::server::bytecode_deserializer::GremlinArgument

use smol_str::SmolStr;

use crate::{
    planner::logical_step::{
        AddEStep, AddVStep, BothEStep, BothStep, CountStep, HasLabelStep, HasPropertyStep, InEStep, InStep, InVStep,
        LogicalPlan, LogicalStep, OtherVStep, OutEStep, OutStep, OutVStep, PropertyStep, ScalarFilterStep, UnionStep,
        VStep, ValuesStep, WhereStep,
    },
    server::bytecode_deserializer::{GremlinArgument, GremlinQueryAst, ParsedGremlinStep},
    types::{gvalue::Primitive, keys::LabelId, VertexKey},
};

// These would ideally come from a schema manager or configuration
const PERSON_LABEL_ID: LabelId = 1;
const SOFTWARE_LABEL_ID: LabelId = 2;
const KNOWS_LABEL_ID: LabelId = 3;
const CREATED_LABEL_ID: LabelId = 4;
const FRIENDS_LABEL_ID: LabelId = 5;

#[derive(Debug)]
pub enum TranslationError {
    InvalidArguments(String),
    UnsupportedGremlinStep(String),
    LabelResolutionError(String),
    PropertyKeyResolutionError(String),
    PrimitiveConversionError(String),
    NestedPlanError(String),
}

impl TryFrom<GremlinQueryAst> for LogicalPlan {
    type Error = TranslationError;

    fn try_from(ast: GremlinQueryAst) -> Result<Self, Self::Error> {
        let mut logical_steps = Vec::new();

        for parsed_step in ast.source {
            logical_steps.extend(translate_parsed_step(parsed_step)?);
        }

        for parsed_step in ast.step {
            logical_steps.extend(translate_parsed_step(parsed_step)?);
        }

        Ok(LogicalPlan { steps: logical_steps })
    }
}

fn parse_optional_labels(args: &[GremlinArgument]) -> Result<Vec<LabelId>, TranslationError> {
    let mut label_ids = Vec::new();
    for arg in args {
        match arg {
            GremlinArgument::String(s) => label_ids.push(resolve_label_name(s)?),
            _ => return Err(TranslationError::InvalidArguments("label filter arguments must be strings".to_string())),
        }
    }
    Ok(label_ids)
}

fn translate_parsed_step(parsed_step: ParsedGremlinStep) -> Result<Vec<LogicalStep>, TranslationError> {
    match parsed_step.name.as_str() {
        "V" => {
            let mut steps = vec![LogicalStep::V(VStep { ids: vec![] })];
            for arg in parsed_step.arguments {
                let value = match arg {
                    GremlinArgument::Int(i) => Primitive::Int32(i),
                    GremlinArgument::String(s) => Primitive::String(SmolStr::new(s)),
                    GremlinArgument::Float(f) => Primitive::Float64(f),
                    GremlinArgument::Bool(b) => Primitive::Bool(b),
                    _ => {
                        return Err(TranslationError::PrimitiveConversionError(
                            "unsupported primitive type".to_string(),
                        ))
                    }
                };
                steps.push(LogicalStep::HasProperty(HasPropertyStep { key: SmolStr::new("id"), value }));
            }
            Ok(steps)
        }
        "has" => {
            if parsed_step.arguments.len() != 2 {
                return Err(TranslationError::InvalidArguments("has step expects 2 arguments".to_string()));
            }
            let prop_key = match &parsed_step.arguments[0] {
                GremlinArgument::String(s) => SmolStr::new(s),
                _ => return Err(TranslationError::InvalidArguments("has step key must be a string".to_string())),
            };
            let prop_value = match &parsed_step.arguments[1] {
                GremlinArgument::Int(i) => Primitive::Int32(*i),
                GremlinArgument::String(s) => Primitive::String(SmolStr::new(s)),
                GremlinArgument::Float(f) => Primitive::Float64(*f),
                GremlinArgument::Bool(b) => Primitive::Bool(*b),
                _ => return Err(TranslationError::PrimitiveConversionError("unsupported primitive type".to_string())),
            };
            Ok(vec![LogicalStep::HasProperty(HasPropertyStep { key: prop_key, value: prop_value })])
        }
        "hasLabel" => {
            if parsed_step.arguments.is_empty() {
                return Err(TranslationError::InvalidArguments("hasLabel expects at least 1 argument".to_string()));
            }
            let mut label_ids = Vec::new();
            for arg in &parsed_step.arguments {
                let label_id = match arg {
                    GremlinArgument::String(s) => resolve_label_name(s)?,
                    _ => {
                        return Err(TranslationError::InvalidArguments(
                            "hasLabel arguments must be strings".to_string(),
                        ))
                    }
                };
                label_ids.push(label_id);
            }
            Ok(vec![LogicalStep::HasLabel(HasLabelStep { label_ids })])
        }
        "is" => {
            if parsed_step.arguments.len() != 1 {
                return Err(TranslationError::InvalidArguments("is expects 1 argument".to_string()));
            }
            let value = match &parsed_step.arguments[0] {
                GremlinArgument::Int(i) => Primitive::Int32(*i),
                GremlinArgument::String(s) => Primitive::String(SmolStr::new(s)),
                GremlinArgument::Float(f) => Primitive::Float64(*f),
                GremlinArgument::Bool(b) => Primitive::Bool(*b),
                _ => return Err(TranslationError::PrimitiveConversionError("unsupported primitive type".to_string())),
            };
            Ok(vec![LogicalStep::ScalarFilter(ScalarFilterStep { value })])
        }
        "outE" => {
            let label_ids = parse_optional_labels(&parsed_step.arguments)?;
            Ok(vec![LogicalStep::OutE(OutEStep { label_ids })])
        }
        "inE" => {
            let label_ids = parse_optional_labels(&parsed_step.arguments)?;
            Ok(vec![LogicalStep::InE(InEStep { label_ids })])
        }
        "bothE" => {
            let label_ids = parse_optional_labels(&parsed_step.arguments)?;
            Ok(vec![LogicalStep::BothE(BothEStep { label_ids })])
        }
        "out" => {
            let label_ids = parse_optional_labels(&parsed_step.arguments)?;
            Ok(vec![LogicalStep::Out(OutStep { label_ids })])
        }
        "in" => {
            let label_ids = parse_optional_labels(&parsed_step.arguments)?;
            Ok(vec![LogicalStep::In(InStep { label_ids })])
        }
        "both" => {
            let label_ids = parse_optional_labels(&parsed_step.arguments)?;
            Ok(vec![LogicalStep::Both(BothStep { label_ids })])
        }
        "outV" => Ok(vec![LogicalStep::OutV(OutVStep {})]),
        "inV" => Ok(vec![LogicalStep::InV(InVStep {})]),
        "otherV" => Ok(vec![LogicalStep::OtherV(OtherVStep {})]),
        "count" => Ok(vec![LogicalStep::Count(CountStep {})]),
        "values" => {
            let property_keys = parsed_step
                .arguments
                .into_iter()
                .map(|arg| match arg {
                    GremlinArgument::String(s) => Ok(SmolStr::new(s)),
                    _ => Err(TranslationError::InvalidArguments("values arguments must be strings".to_string())),
                })
                .collect::<Result<Vec<_>, TranslationError>>()?;
            Ok(vec![LogicalStep::Values(ValuesStep { property_keys })])
        }
        "where" => {
            let mut args = parsed_step.arguments.into_iter();
            let arg = args
                .next()
                .ok_or_else(|| TranslationError::InvalidArguments("where expects 1 argument".to_string()))?;
            let plan = match arg {
                GremlinArgument::NestedBytecode(ast) => ast.try_into()?,
                _ => return Err(TranslationError::NestedPlanError("where step expects nested bytecode".to_string())),
            };
            Ok(vec![LogicalStep::Where(WhereStep { plan })])
        }
        "union" => {
            let plans: Vec<LogicalPlan> = parsed_step
                .arguments
                .into_iter()
                .map(|arg| match arg {
                    GremlinArgument::NestedBytecode(ast) => ast.try_into(),
                    _ => Err(TranslationError::NestedPlanError("union step expects nested bytecode".to_string())),
                })
                .collect::<Result<Vec<LogicalPlan>, TranslationError>>()?;
            Ok(vec![LogicalStep::Union(UnionStep { plans })])
        }
        "property" => {
            if parsed_step.arguments.len() != 2 {
                return Err(TranslationError::InvalidArguments("property expects 2 arguments".to_string()));
            }
            let prop_key = match &parsed_step.arguments[0] {
                GremlinArgument::String(s) => SmolStr::new(s),
                _ => return Err(TranslationError::InvalidArguments("property key must be a string".to_string())),
            };
            let prop_value = match &parsed_step.arguments[1] {
                GremlinArgument::Int(i) => Primitive::Int32(*i),
                GremlinArgument::String(s) => Primitive::String(SmolStr::new(s)),
                GremlinArgument::Float(f) => Primitive::Float64(*f),
                GremlinArgument::Bool(b) => Primitive::Bool(*b),
                _ => return Err(TranslationError::PrimitiveConversionError("unsupported primitive type".to_string())),
            };
            Ok(vec![LogicalStep::Property(PropertyStep { prop_key, prop_value })])
        }
        "addV" => {
            let mut args = parsed_step.arguments.into_iter();
            let label_name = match args.next() {
                Some(GremlinArgument::String(s)) => s,
                _ => {
                    return Err(TranslationError::InvalidArguments(
                        "addV expects a string label as first argument".to_string(),
                    ))
                }
            };
            let label_id = resolve_label_name(&label_name)?;

            let vertex_id = match args.next() {
                Some(GremlinArgument::Int(i)) => i as VertexKey,
                _ => {
                    return Err(TranslationError::InvalidArguments(
                        "addV expects an integer ID as second argument".to_string(),
                    ))
                }
            };

            let properties = match args.next() {
                Some(GremlinArgument::Map(m)) => {
                    let mut props = std::collections::HashMap::new();
                    for (k, v) in m {
                        let val = match v {
                            GremlinArgument::Int(i) => Primitive::Int32(i),
                            GremlinArgument::String(s) => Primitive::String(SmolStr::new(s)),
                            GremlinArgument::Float(f) => Primitive::Float64(f),
                            GremlinArgument::Bool(b) => Primitive::Bool(b),
                            _ => {
                                return Err(TranslationError::PrimitiveConversionError(
                                    "unsupported primitive type in addV properties".to_string(),
                                ))
                            }
                        };
                        props.insert(SmolStr::new(k), val);
                    }
                    props
                }
                None => std::collections::HashMap::new(),
                _ => {
                    return Err(TranslationError::InvalidArguments("addV expects a map as third argument".to_string()))
                }
            };

            Ok(vec![LogicalStep::AddV(AddVStep { label_id, vertex_id, properties })])
        }
        "addE" => {
            let mut args = parsed_step.arguments.into_iter();
            let label_name = match args.next() {
                Some(GremlinArgument::String(s)) => s,
                _ => {
                    return Err(TranslationError::InvalidArguments(
                        "addE expects a string label as first argument".to_string(),
                    ))
                }
            };
            let label_id = resolve_label_name(&label_name)?;

            let out_v_id = match args.next() {
                Some(GremlinArgument::Int(i)) => i as VertexKey,
                _ => {
                    return Err(TranslationError::InvalidArguments(
                        "addE expects an integer outV ID as second argument".to_string(),
                    ))
                }
            };

            let in_v_id = match args.next() {
                Some(GremlinArgument::Int(i)) => i as VertexKey,
                _ => {
                    return Err(TranslationError::InvalidArguments(
                        "addE expects an integer inV ID as third argument".to_string(),
                    ))
                }
            };

            let properties = match args.next() {
                Some(GremlinArgument::Map(m)) => {
                    let mut props = std::collections::HashMap::new();
                    for (k, v) in m {
                        let val = match v {
                            GremlinArgument::Int(i) => Primitive::Int32(i),
                            GremlinArgument::String(s) => Primitive::String(SmolStr::new(s)),
                            GremlinArgument::Float(f) => Primitive::Float64(f),
                            GremlinArgument::Bool(b) => Primitive::Bool(b),
                            _ => {
                                return Err(TranslationError::PrimitiveConversionError(
                                    "unsupported primitive type in addE properties".to_string(),
                                ))
                            }
                        };
                        props.insert(SmolStr::new(k), val);
                    }
                    props
                }
                None => std::collections::HashMap::new(),
                _ => {
                    return Err(TranslationError::InvalidArguments("addE expects a map as fourth argument".to_string()))
                }
            };

            Ok(vec![LogicalStep::AddE(AddEStep { label_id, out_v_id, in_v_id, properties })])
        }
        _ => Err(TranslationError::UnsupportedGremlinStep(parsed_step.name)),
    }
}

fn resolve_label_name(name: &str) -> Result<LabelId, TranslationError> {
    match name {
        "person" => Ok(PERSON_LABEL_ID),
        "software" => Ok(SOFTWARE_LABEL_ID),
        "knows" => Ok(KNOWS_LABEL_ID),
        "created" => Ok(CREATED_LABEL_ID),
        "friends" => Ok(FRIENDS_LABEL_ID),
        _ => Err(TranslationError::LabelResolutionError(format!("Unknown label: {}", name))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        planner::logical_step::LogicalStep,
        server::bytecode_deserializer::{GremlinArgument, GremlinQueryAst, ParsedGremlinStep},
    };

    #[test]
    fn test_translate_v_step() {
        let ast = GremlinQueryAst {
            source: vec![],
            step: vec![ParsedGremlinStep {
                name: "V".to_string(),
                arguments: vec![GremlinArgument::Int(1), GremlinArgument::Int(2)],
            }],
        };

        let plan = LogicalPlan::try_from(ast).unwrap();
        assert_eq!(plan.steps.len(), 3);
        if let LogicalStep::V(s) = &plan.steps[0] {
            assert!(s.ids.is_empty());
        } else {
            panic!("Expected VStep");
        }
        if let LogicalStep::HasProperty(s) = &plan.steps[1] {
            assert_eq!(s.key, "id");
            assert_eq!(s.value, Primitive::Int32(1));
        } else {
            panic!("Expected HasProperty");
        }
        if let LogicalStep::HasProperty(s) = &plan.steps[2] {
            assert_eq!(s.key, "id");
            assert_eq!(s.value, Primitive::Int32(2));
        } else {
            panic!("Expected HasProperty");
        }
    }

    #[test]
    fn test_translate_has_step_primitives() {
        let ast = GremlinQueryAst {
            source: vec![],
            step: vec![
                ParsedGremlinStep {
                    name: "has".to_string(),
                    arguments: vec![
                        GremlinArgument::String("name".to_string()),
                        GremlinArgument::String("marko".to_string()),
                    ],
                },
                ParsedGremlinStep {
                    name: "has".to_string(),
                    arguments: vec![GremlinArgument::String("age".to_string()), GremlinArgument::Int(29)],
                },
                ParsedGremlinStep {
                    name: "has".to_string(),
                    arguments: vec![GremlinArgument::String("active".to_string()), GremlinArgument::Bool(true)],
                },
            ],
        };

        let plan = LogicalPlan::try_from(ast).unwrap();
        assert_eq!(plan.steps.len(), 3);

        if let LogicalStep::HasProperty(s) = &plan.steps[0] {
            assert_eq!(s.key, "name");
            assert_eq!(s.value, Primitive::String(SmolStr::new("marko")));
        }
        if let LogicalStep::HasProperty(s) = &plan.steps[1] {
            assert_eq!(s.key, "age");
            assert_eq!(s.value, Primitive::Int32(29));
        }
        if let LogicalStep::HasProperty(s) = &plan.steps[2] {
            assert_eq!(s.key, "active");
            assert_eq!(s.value, Primitive::Bool(true));
        }
    }

    #[test]
    fn test_translate_edge_steps() {
        let ast = GremlinQueryAst {
            source: vec![],
            step: vec![
                ParsedGremlinStep {
                    name: "outE".to_string(),
                    arguments: vec![GremlinArgument::String("knows".to_string())],
                },
                ParsedGremlinStep { name: "inE".to_string(), arguments: vec![] },
                ParsedGremlinStep { name: "count".to_string(), arguments: vec![] },
            ],
        };

        let plan = LogicalPlan::try_from(ast).unwrap();
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(plan.steps[0], LogicalStep::OutE(_)));
        assert!(matches!(plan.steps[1], LogicalStep::InE(_)));
        assert!(matches!(plan.steps[2], LogicalStep::Count(_)));

        if let LogicalStep::OutE(s) = &plan.steps[0] {
            assert_eq!(s.label_ids, vec![KNOWS_LABEL_ID]);
        }
    }

    #[test]
    fn test_translate_union_nested() {
        let sub_ast = GremlinQueryAst {
            source: vec![],
            step: vec![ParsedGremlinStep { name: "count".to_string(), arguments: vec![] }],
        };

        let ast = GremlinQueryAst {
            source: vec![],
            step: vec![ParsedGremlinStep {
                name: "union".to_string(),
                arguments: vec![GremlinArgument::NestedBytecode(sub_ast)],
            }],
        };

        let plan = LogicalPlan::try_from(ast).unwrap();
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::Union(s) = &plan.steps[0] {
            assert_eq!(s.plans.len(), 1);
            assert_eq!(s.plans[0].steps.len(), 1);
            assert!(matches!(s.plans[0].steps[0], LogicalStep::Count(_)));
        }
    }

    #[test]
    fn test_translate_error_unsupported() {
        let ast = GremlinQueryAst {
            source: vec![],
            step: vec![ParsedGremlinStep { name: "unsupported_step".to_string(), arguments: vec![] }],
        };
        assert!(LogicalPlan::try_from(ast).is_err());
    }
}
