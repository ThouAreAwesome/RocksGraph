use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)] // Allows deserializing into different types based on content
pub enum GremlinArgument {
    String(String),
    Int(i32),
    Float(f64),
    Bool(bool),
    // For nested traversals in steps like union, where, etc.
    NestedBytecode(GremlinQueryAst),
    List(Vec<GremlinArgument>),
    Map(std::collections::HashMap<String, GremlinArgument>),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ParsedGremlinStep {
    pub name: String,
    #[serde(default)]
    pub arguments: Vec<GremlinArgument>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct GremlinQueryAst {
    #[serde(default)]
    pub source: Vec<ParsedGremlinStep>,
    #[serde(default)]
    pub step: Vec<ParsedGremlinStep>,
}

#[derive(Debug, Clone)]
pub enum DeserializationError {
    Json(String),
    InvalidFormat(String),
}

/// Deserializes a byte slice (expected to be JSON) into a GremlinQueryAst.
pub fn deserialize_bytecode(bytes: &[u8]) -> Result<GremlinQueryAst, DeserializationError> {
    serde_json::from_slice(bytes).map_err(|e| DeserializationError::Json(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_simple_query() {
        let json_bytecode = r#"{
            "source": [],
            "step": [
                {"name": "V", "arguments": [1]},
                {"name": "has", "arguments": ["name", "marko"]},
                {"name": "count", "arguments": []}
            ]
        }"#;

        let ast = deserialize_bytecode(json_bytecode.as_bytes()).unwrap();

        assert_eq!(ast.step.len(), 3);
        assert_eq!(ast.step[0].name, "V");
        assert_eq!(ast.step[0].arguments.len(), 1);
        if let GremlinArgument::Int(id) = ast.step[0].arguments[0] {
            assert_eq!(id, 1);
        } else {
            panic!("Expected Int argument");
        }

        assert_eq!(ast.step[1].name, "has");
        assert_eq!(ast.step[1].arguments.len(), 2);
        if let GremlinArgument::String(key) = &ast.step[1].arguments[0] {
            assert_eq!(key, "name");
        } else {
            panic!("Expected String argument");
        }
        if let GremlinArgument::String(value) = &ast.step[1].arguments[1] {
            assert_eq!(value, "marko");
        } else {
            panic!("Expected String argument");
        }
        assert_eq!(ast.step[2].name, "count");
        assert!(ast.step[2].arguments.is_empty());
    }

    #[test]
    fn test_deserialize_data_types() {
        let json_bytecode = r#"{
            "step": [
                {"name": "filter", "arguments": [1.5, true, false]}
            ]
        }"#;
        let ast = deserialize_bytecode(json_bytecode.as_bytes()).unwrap();
        let args = &ast.step[0].arguments;

        assert!(matches!(args[0], GremlinArgument::Float(f) if (f - 1.5).abs() < f64::EPSILON));
        assert!(matches!(args[1], GremlinArgument::Bool(true)));
        assert!(matches!(args[2], GremlinArgument::Bool(false)));
    }

    #[test]
    fn test_deserialize_collections() {
        let json_bytecode = r#"{
            "step": [
                {
                    "name": "aggregate",
                    "arguments": [
                        ["a", 1],
                        {"key": "value", "id": 100}
                    ]
                }
            ]
        }"#;
        let ast = deserialize_bytecode(json_bytecode.as_bytes()).unwrap();
        let args = &ast.step[0].arguments;

        if let GremlinArgument::List(l) = &args[0] {
            assert_eq!(l.len(), 2);
            assert!(matches!(l[0], GremlinArgument::String(_)));
            assert!(matches!(l[1], GremlinArgument::Int(1)));
        } else {
            panic!("Expected List argument");
        }

        if let GremlinArgument::Map(m) = &args[1] {
            assert_eq!(m.len(), 2);
            assert!(matches!(m.get("key"), Some(GremlinArgument::String(_))));
            assert!(matches!(m.get("id"), Some(GremlinArgument::Int(100))));
        } else {
            panic!("Expected Map argument");
        }
    }

    #[test]
    fn test_deserialize_nested_traversal() {
        let json_bytecode = r#"{
            "step": [
                {
                    "name": "union",
                    "arguments": [
                        {
                            "source": [],
                            "step": [{"name": "count", "arguments": []}]
                        }
                    ]
                }
            ]
        }"#;

        let ast = deserialize_bytecode(json_bytecode.as_bytes()).unwrap();
        assert_eq!(ast.step[0].name, "union");
        // The argument should match GremlinQueryAst structure and be picked by NestedBytecode variant
        assert!(matches!(ast.step[0].arguments[0], GremlinArgument::NestedBytecode(_)));
    }

    #[test]
    fn test_deserialize_empty_query() {
        let json_bytecode = r#"{}"#;
        let ast = deserialize_bytecode(json_bytecode.as_bytes()).unwrap();
        assert!(ast.step.is_empty());
    }

    #[test]
    fn test_deserialize_invalid_json() {
        let json_bytecode = r#"{ "step": [ { "name": "V", "#;
        let result = deserialize_bytecode(json_bytecode.as_bytes());
        assert!(result.is_err());
    }
}
