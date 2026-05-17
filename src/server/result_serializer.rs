use serde_json::{json, Value};

use crate::types::gvalue::{GValue, Primitive};

#[derive(Debug)]
pub enum SerializationError {
    Json(serde_json::Error),
}

/// Serializes a vector of GValue results into a JSON string.
pub fn serialize_results(results: Vec<GValue>) -> Result<String, SerializationError> {
    let serialized_values: Vec<Value> = results.into_iter().map(serialize_gvalue).collect();
    serde_json::to_string(&serialized_values).map_err(SerializationError::Json)
}

fn serialize_gvalue(gvalue: GValue) -> Value {
    match gvalue {
        GValue::Vertex(vk) => json!({
            "@type": "g:Vertex",
            "@value": {
                "id": vk,
                "label": "vertex" // Placeholder, actual label would need to be fetched
            }
        }),
        GValue::Edge(ek) => json!({
            "@type": "g:Edge",
            "@value": {
                "id": format!("{}-{}-{}", ek.primary_id, ek.label_id, ek.secondary_id), // Simplified ID
                "label": "edge", // Placeholder
                "inVLabel": "vertex", // Placeholder
                "outVLabel": "vertex", // Placeholder
                "inV": ek.secondary_id,
                "outV": ek.primary_id
            }
        }),
        GValue::Scalar(primitive) => serialize_primitive(primitive),
        // Add other GValue types as needed
        _ => json!({
            "@type": "g:Unknown",
            "@value": format!("{:?}", gvalue)
        }),
    }
}

fn serialize_primitive(primitive: Primitive) -> Value {
    match primitive {
        Primitive::String(s) => json!({ "@type": "g:String", "@value": s.as_str() }),
        Primitive::Int32(i) => json!({ "@type": "g:Int32", "@value": i }),
        Primitive::Int64(i) => json!({ "@type": "g:Int64", "@value": i }),
        Primitive::Float32(f) => json!({ "@type": "g:Float", "@value": f }),
        Primitive::Float64(f) => json!({ "@type": "g:Double", "@value": f }),
        Primitive::Bool(b) => json!({ "@type": "g:Boolean", "@value": b }),
        Primitive::Uuid(u) => json!({ "@type": "g:UUID", "@value": u.to_string() }),
        Primitive::Null => json!(null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{keys::EdgeKey, GValue};

    #[test]
    fn test_serialize_vertex_result() {
        let results = vec![GValue::Vertex(123)];
        let json_string = serialize_results(results).unwrap();
        let expected_json = r#"[{"@type":"g:Vertex","@value":{"id":123,"label":"vertex"}}]"#;
        let parsed_actual: Value = serde_json::from_str(&json_string).unwrap();
        let parsed_expected: Value = serde_json::from_str(expected_json).unwrap();
        assert_eq!(parsed_actual, parsed_expected);
    }

    #[test]
    fn test_serialize_edge_result() {
        let edge_key = EdgeKey::out_e(1, 100, 2, 0); // src=1, label=100, dst=2, rank=0
        let results = vec![GValue::Edge(edge_key)];
        let json_string = serialize_results(results).unwrap();
        let expected_json = r#"[{"@type":"g:Edge","@value":{"id":"1-100-2","inV":2,"inVLabel":"vertex","label":"edge","outV":1,"outVLabel":"vertex"}}]"#;
        let parsed_actual: Value = serde_json::from_str(&json_string).unwrap();
        let parsed_expected: Value = serde_json::from_str(expected_json).unwrap();
        assert_eq!(parsed_actual, parsed_expected);
    }

    #[test]
    fn test_serialize_scalar_result() {
        let results = vec![
            GValue::Scalar(Primitive::Int32(42)),
            GValue::Scalar(Primitive::String(smol_str::SmolStr::new("hello"))),
            GValue::Scalar(Primitive::Float64(3.144)),
            GValue::Scalar(Primitive::Bool(true)),
        ];
        let json_string = serialize_results(results).unwrap();
        let expected_json = r#"[
            {"@type":"g:Int32","@value":42},
            {"@type":"g:String","@value":"hello"},
            {"@type":"g:Double","@value":3.144},
            {"@type":"g:Boolean","@value":true}
        ]"#;
        let parsed_actual: Value = serde_json::from_str(&json_string).unwrap();
        let parsed_expected: Value = serde_json::from_str(expected_json).unwrap();
        assert_eq!(parsed_actual, parsed_expected);
    }
}
