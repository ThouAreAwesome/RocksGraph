use std::sync::Arc;

use smol_str::SmolStr;

use crate::{
    graph::LogicalGraph,
    store::{GraphStore, RocksStorage}, // Added for gvalue_to_json_value
    types::{
        gvalue::Primitive,
        keys::{CanonicalEdgeKey, CanonicalKey, LabelId},
    },
};

// Define LabelIds for common labels used across tests
pub const PERSON_LABEL_ID: LabelId = 1;
pub const SOFTWARE_LABEL_ID: LabelId = 2;
pub const KNOWS_LABEL_ID: LabelId = 3;
pub const CREATED_LABEL_ID: LabelId = 4;
pub const FRIENDS_LABEL_ID: LabelId = 5;

pub fn open_rocks_store() -> (Arc<RocksStorage>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(RocksStorage::open(dir.path()).unwrap());
    (store, dir)
}

/// Creates a standard TinkerPop Modern Graph with predefined vertices and edges.
/// This graph is used as a common baseline for various test cases.
///
/// The graph includes: Marko, Vadas, Lop, Josh, Ripple, Peter and their relationships.
pub fn create_tinkerpop_modern_graph_for_server_test(store: Arc<RocksStorage>) {
    let mut graph = LogicalGraph::<RocksStorage>::new(store.begin());

    // Add Vertices
    let (v_marko_key, _) = graph.add_vertex(1, PERSON_LABEL_ID).unwrap();
    graph
        .set_property(CanonicalKey::Vertex(v_marko_key), SmolStr::new("name"), Primitive::String(SmolStr::new("marko")))
        .unwrap();
    graph.set_property(CanonicalKey::Vertex(v_marko_key), SmolStr::new("age"), Primitive::Int32(29)).unwrap();

    let (v_vadas_key, _) = graph.add_vertex(2, PERSON_LABEL_ID).unwrap();
    graph
        .set_property(CanonicalKey::Vertex(v_vadas_key), SmolStr::new("name"), Primitive::String(SmolStr::new("vadas")))
        .unwrap();
    graph.set_property(CanonicalKey::Vertex(v_vadas_key), SmolStr::new("age"), Primitive::Int32(27)).unwrap();

    let (v_lop_key, _) = graph.add_vertex(3, SOFTWARE_LABEL_ID).unwrap();
    graph
        .set_property(CanonicalKey::Vertex(v_lop_key), SmolStr::new("name"), Primitive::String(SmolStr::new("lop")))
        .unwrap();
    graph
        .set_property(CanonicalKey::Vertex(v_lop_key), SmolStr::new("lang"), Primitive::String(SmolStr::new("java")))
        .unwrap();

    let (v_josh_key, _) = graph.add_vertex(4, PERSON_LABEL_ID).unwrap();
    graph
        .set_property(CanonicalKey::Vertex(v_josh_key), SmolStr::new("name"), Primitive::String(SmolStr::new("josh")))
        .unwrap();
    graph.set_property(CanonicalKey::Vertex(v_josh_key), SmolStr::new("age"), Primitive::Int32(32)).unwrap();

    let (v_ripple_key, _) = graph.add_vertex(5, SOFTWARE_LABEL_ID).unwrap();
    graph
        .set_property(
            CanonicalKey::Vertex(v_ripple_key),
            SmolStr::new("name"),
            Primitive::String(SmolStr::new("ripple")),
        )
        .unwrap();
    graph
        .set_property(CanonicalKey::Vertex(v_ripple_key), SmolStr::new("lang"), Primitive::String(SmolStr::new("java")))
        .unwrap();

    let (v_peter_key, _) = graph.add_vertex(6, PERSON_LABEL_ID).unwrap();
    graph
        .set_property(CanonicalKey::Vertex(v_peter_key), SmolStr::new("name"), Primitive::String(SmolStr::new("peter")))
        .unwrap();
    graph.set_property(CanonicalKey::Vertex(v_peter_key), SmolStr::new("age"), Primitive::Int32(35)).unwrap();

    // Add Edges
    let (e1_key, _) = graph
        .add_edge(CanonicalEdgeKey { src_id: v_marko_key, label_id: KNOWS_LABEL_ID, rank: 0, dst_id: v_vadas_key })
        .unwrap();
    graph
        .set_property(CanonicalKey::Edge(e1_key.canonical_edge_key()), SmolStr::new("weight"), Primitive::Float64(0.5))
        .unwrap();

    let (e2_key, _) = graph
        .add_edge(CanonicalEdgeKey { src_id: v_marko_key, label_id: KNOWS_LABEL_ID, rank: 0, dst_id: v_josh_key })
        .unwrap();
    graph
        .set_property(CanonicalKey::Edge(e2_key.canonical_edge_key()), SmolStr::new("weight"), Primitive::Float64(1.0))
        .unwrap();

    let (e3_key, _) = graph
        .add_edge(CanonicalEdgeKey { src_id: v_marko_key, label_id: CREATED_LABEL_ID, rank: 0, dst_id: v_lop_key })
        .unwrap();
    graph
        .set_property(CanonicalKey::Edge(e3_key.canonical_edge_key()), SmolStr::new("weight"), Primitive::Float64(0.4))
        .unwrap();

    let (e4_key, _) = graph
        .add_edge(CanonicalEdgeKey { src_id: v_josh_key, label_id: CREATED_LABEL_ID, rank: 0, dst_id: v_ripple_key })
        .unwrap();
    graph
        .set_property(CanonicalKey::Edge(e4_key.canonical_edge_key()), SmolStr::new("weight"), Primitive::Float64(1.0))
        .unwrap();

    let (e5_key, _) = graph
        .add_edge(CanonicalEdgeKey { src_id: v_josh_key, label_id: CREATED_LABEL_ID, rank: 0, dst_id: v_lop_key })
        .unwrap();
    graph
        .set_property(CanonicalKey::Edge(e5_key.canonical_edge_key()), SmolStr::new("weight"), Primitive::Float64(0.4))
        .unwrap();

    let (e6_key, _) = graph
        .add_edge(CanonicalEdgeKey { src_id: v_peter_key, label_id: CREATED_LABEL_ID, rank: 0, dst_id: v_lop_key })
        .unwrap();
    graph
        .set_property(CanonicalKey::Edge(e6_key.canonical_edge_key()), SmolStr::new("weight"), Primitive::Float64(0.2))
        .unwrap();

    graph.commit().unwrap();
}

/// Helper to parse the server's JSON response and extract the data value.
pub fn parse_server_response(response_json: &str) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let parsed: serde_json::Value = serde_json::from_str(response_json)?;

    if parsed["status"]["code"] != 200 {
        return Err(format!("Server returned error: {}", parsed["status"]["message"]).into());
    }

    let data = parsed["result"]["data"]["@value"].clone();
    Ok(data)
}

/// Helper to convert a GValue to a serde_json::Value for comparison.
pub fn gvalue_to_json_value(gvalue: &crate::types::GValue) -> serde_json::Value {
    use serde_json::json; // Import json! macro locally
    match gvalue {
        crate::types::GValue::Vertex(vk) => json!({
            "@type": "g:Vertex",
            "@value": {
                "id": vk,
                "label": "vertex" // Placeholder, actual label would need to be fetched
            }
        }),
        crate::types::GValue::Edge(ek) => json!({
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
        crate::types::GValue::Scalar(primitive) => match primitive {
            Primitive::String(s) => json!({ "@type": "g:String", "@value": s.as_str() }),
            Primitive::Int32(i) => json!({ "@type": "g:Int32", "@value": i }),
            Primitive::Int64(i) => json!({ "@type": "g:Int64", "@value": i }),
            Primitive::Float32(f) => json!({ "@type": "g:Float", "@value": f }),
            Primitive::Float64(f) => json!({ "@type": "g:Double", "@value": f }),
            Primitive::Bool(b) => json!({ "@type": "g:Boolean", "@value": b }),
            Primitive::Uuid(u) => json!({ "@type": "g:UUID", "@value": u.to_string() }),
            Primitive::Null => json!(null),
        },
        _ => json!({
            "@type": "g:Unknown",
            "@value": format!("{:?}", gvalue)
        }),
    }
}
