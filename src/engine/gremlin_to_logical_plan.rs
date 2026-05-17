use smol_str::SmolStr;

use crate::{
    engine::logical_step::{
        CountStep as LogicalCountStep, HasPropertyStep as LogicalHasPropertyStep, InEStep as LogicalInEStep,
        LogicalPlan, LogicalStep, OutEStep as LogicalOutEStep, UnionStep as LogicalUnionStep, VStep as LogicalVStep,
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

        // Handle source steps (e.g., g.V())
        for parsed_step in ast.source {
            logical_steps.push(translate_parsed_step(parsed_step)?);
        }

        // Handle regular steps
        for parsed_step in ast.step {
            logical_steps.push(translate_parsed_step(parsed_step)?);
        }

        Ok(LogicalPlan { steps: logical_steps })
    }
}

fn translate_parsed_step(parsed_step: ParsedGremlinStep) -> Result<LogicalStep, TranslationError> {
    match parsed_step.name.as_str() {
        "V" => {
            let ids: Vec<VertexKey> = parsed_step
                .arguments
                .into_iter()
                .map(|arg| match arg {
                    GremlinArgument::Int(id) => Ok(id as VertexKey),
                    _ => Err(TranslationError::InvalidArguments("V step expects integer IDs".to_string())),
                })
                .collect::<Result<Vec<VertexKey>, TranslationError>>()?;
            Ok(LogicalStep::V(LogicalVStep { ids }))
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
            Ok(LogicalStep::HasProperty(LogicalHasPropertyStep { key: prop_key, value: prop_value }))
        }
        "outE" => {
            let label_filter = if parsed_step.arguments.is_empty() {
                None
            } else if parsed_step.arguments.len() == 1 {
                match &parsed_step.arguments[0] {
                    GremlinArgument::String(s) => Some(resolve_label_name(s)?),
                    _ => {
                        return Err(TranslationError::InvalidArguments(
                            "outE label filter must be a string".to_string(),
                        ))
                    }
                }
            } else {
                return Err(TranslationError::InvalidArguments("outE step expects 0 or 1 argument".to_string()));
            };
            Ok(LogicalStep::OutE(LogicalOutEStep { label_filter }))
        }
        "inE" => {
            let label_filter = if parsed_step.arguments.is_empty() {
                None
            } else if parsed_step.arguments.len() == 1 {
                match &parsed_step.arguments[0] {
                    GremlinArgument::String(s) => Some(resolve_label_name(s)?),
                    _ => {
                        return Err(TranslationError::InvalidArguments("inE label filter must be a string".to_string()))
                    }
                }
            } else {
                return Err(TranslationError::InvalidArguments("inE step expects 0 or 1 argument".to_string()));
            };
            Ok(LogicalStep::InE(LogicalInEStep { label_filter }))
        }
        "count" => Ok(LogicalStep::Count(LogicalCountStep {})),
        "union" => {
            let plans: Vec<LogicalPlan> = parsed_step
                .arguments
                .into_iter()
                .map(|arg| match arg {
                    GremlinArgument::NestedBytecode(ast) => ast.try_into(),
                    _ => Err(TranslationError::NestedPlanError("union step expects nested bytecode".to_string())),
                })
                .collect::<Result<Vec<LogicalPlan>, TranslationError>>()?;
            Ok(LogicalStep::Union(LogicalUnionStep { plans }))
        }
        // Add other Gremlin steps here
        _ => Err(TranslationError::UnsupportedGremlinStep(parsed_step.name)),
    }
}

// Helper to resolve label names to LabelIds. In a real system, this would query a schema manager.
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
    use crate::engine::logical_step::LogicalStep;

    #[test]
    fn test_translate_v_step() {
        let ast = GremlinQueryAst {
            source: vec![ParsedGremlinStep {
                name: "V".to_string(),
                arguments: vec![GremlinArgument::Int(1), GremlinArgument::Int(2)],
            }],
            step: vec![],
        };

        let plan = LogicalPlan::try_from(ast).unwrap();
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::V(s) = &plan.steps[0] {
            assert_eq!(s.ids, vec![1, 2]);
        } else {
            panic!("Expected VStep");
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
            assert_eq!(s.label_filter, Some(KNOWS_LABEL_ID));
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
