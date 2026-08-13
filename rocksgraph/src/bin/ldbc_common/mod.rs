//! Shared parsing + schema declaration for the diversified synthetic LDBC-shaped
//! dataset (see `scripts/generate_synthetic_ldbc.py`), used by both
//! `bulk_load_ldbc_typed` and `oltp_load_ldbc` via `#[path] mod`.
//!
//! Kept as a single shared module (rather than duplicated per binary) so the
//! two loaders are structurally guaranteed to parse identical vertex/edge
//! data — any divergence here would show up as a false-positive mismatch in
//! `cross_validate_load`.
#![allow(dead_code)]

use rocksgraph::{
    bulk::{BulkEdge, BulkVertex},
    schema::DataType,
    AnnAlgorithm, DistanceMetric, Graph, HnswConfig, Primitive, StoreError, Value, VectorEntityType, VectorIndexConfig,
};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

pub const VECTOR_DIM: usize = 16;
pub const VERTEX_LABEL: &str = "Person";
pub const EDGE_LABEL: &str = "knows";
pub const VECTOR_PROP: &str = "embedding";

/// Default SST-file size cap used to force splitting at this dataset's
/// moderate scale (~30MB raw), well below the library default of 58MiB —
/// mirrors `test_sst_file_splitting` (`rocksgraph/src/bulk/tests.rs`), scaled
/// up from its 1-byte extreme to a realistic multi-file target.
pub const DEFAULT_MAX_SST_SIZE: usize = 2 * 1024 * 1024;

fn hex_decode(s: &str) -> Vec<u8> {
    let s = s.trim();
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0)).collect()
}

fn parse_uuid(s: &str) -> u128 {
    u128::from_str_radix(&s.trim().replace('-', ""), 16).unwrap_or(0)
}

fn parse_embedding(s: &str) -> Vec<f32> {
    s.trim().split(',').filter_map(|x| x.parse::<f32>().ok()).collect()
}

/// person_0_0.csv columns:
/// id|firstName|lastName|gender|birthday|creationDate|locationIP|browserUsed|
/// age|friendCount|rating|balance|isVerified|sessionId|signupCode|avatarHash|embedding
///
/// Naive `split('|')` — assumes no field value itself contains a literal '|'.
/// Safe for `generate_synthetic_ldbc.py`'s controlled value pools; a real
/// CSV/TSV parser with quoting support would be required for arbitrary input.
pub fn parse_person_vertices(path: &Path) -> impl Iterator<Item = Result<BulkVertex, StoreError>> {
    let reader = BufReader::new(File::open(path).expect("failed to open person file"));
    let mut lines = reader.lines();
    let _ = lines.next(); // header

    lines.filter_map(|line| {
        let line = line.ok()?;
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 17 {
            return None;
        }
        let id: i64 = parts[0].parse().ok()?;

        let mut props = HashMap::new();
        props.insert("firstName".into(), Primitive::String(parts[1].into()));
        props.insert("lastName".into(), Primitive::String(parts[2].into()));
        props.insert("gender".into(), Primitive::String(parts[3].into()));
        props.insert("birthday".into(), Primitive::String(parts[4].into()));
        props.insert("creationDate".into(), Primitive::String(parts[5].into()));
        props.insert("locationIP".into(), Primitive::String(parts[6].into()));
        props.insert("browserUsed".into(), Primitive::String(parts[7].into()));
        props.insert("age".into(), Primitive::Int32(parts[8].parse().ok()?));
        props.insert("friendCount".into(), Primitive::Int64(parts[9].parse().ok()?));
        props.insert("rating".into(), Primitive::Float32(parts[10].parse().ok()?));
        props.insert("balance".into(), Primitive::Float64(parts[11].parse().ok()?));
        props.insert("isVerified".into(), Primitive::Bool(parts[12] == "true"));
        props.insert("sessionId".into(), Primitive::Uuid(parse_uuid(parts[13])));
        props.insert("signupCode".into(), Primitive::UInt16(parts[14].parse().ok()?));
        props.insert("avatarHash".into(), Primitive::Bytes(hex_decode(parts[15])));
        props.insert(VECTOR_PROP.into(), Primitive::FloatVector(parse_embedding(parts[16])));

        Some(Ok(BulkVertex { id, label: VERTEX_LABEL.into(), props }))
    })
}

/// person_knows_person_0_0.csv columns: Person.id|Person.id|creationDate|weight
pub fn parse_knows_edges(path: &Path) -> impl Iterator<Item = Result<BulkEdge, StoreError>> {
    let reader = BufReader::new(File::open(path).expect("failed to open knows file"));
    let mut lines = reader.lines();
    let _ = lines.next(); // header

    lines.filter_map(|line| {
        let line = line.ok()?;
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 4 {
            return None;
        }
        let src: i64 = parts[0].parse().ok()?;
        let dst: i64 = parts[1].parse().ok()?;

        let mut props = HashMap::new();
        props.insert("creationDate".into(), Primitive::String(parts[2].into()));
        props.insert("weight".into(), Primitive::Float32(parts[3].parse().ok()?));

        Some(Ok(BulkEdge { src, dst, label: EDGE_LABEL.into(), props, rank: None }))
    })
}

/// Mirrors the crate-internal `gremlin::type_bridge::primitive_to_value` (not
/// reachable from outside the crate) so `oltp_load_ldbc` can turn the
/// `Primitive` values produced by [`parse_person_vertices`]/[`parse_knows_edges`]
/// into the `Value`s `.property()` expects.
pub fn primitive_to_value(p: Primitive) -> Value {
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
        Primitive::Bytes(b) => Value::Bytes(b),
        Primitive::FloatVector(v) => Value::FloatVector(v),
        // `Primitive` is `#[non_exhaustive]`; this mirror needs a matching arm
        // added alongside the real `gremlin::type_bridge::primitive_to_value`.
        other => unimplemented!("primitive_to_value: unhandled Primitive variant {other:?}"),
    }
}

/// Declares the full diversified schema (every scalar `DataType` plus a
/// `FloatVector` vector index on `embedding`) shared by both loaders, so the
/// bulk and OLTP paths write against byte-identical schema.
pub fn declare_schema(graph: &Graph) -> Result<(), StoreError> {
    let mut schema = graph.open_schema();
    schema.add_vertex_label(VERTEX_LABEL);
    schema.add_edge_label(EDGE_LABEL);

    schema.add_property_key("firstName", DataType::String);
    schema.add_property_key("lastName", DataType::String);
    schema.add_property_key("gender", DataType::String);
    schema.add_property_key("birthday", DataType::String);
    schema.add_property_key("creationDate", DataType::String);
    schema.add_property_key("locationIP", DataType::String);
    schema.add_property_key("browserUsed", DataType::String);
    schema.add_property_key("age", DataType::Int32);
    schema.add_property_key("friendCount", DataType::Int64);
    schema.add_property_key("rating", DataType::Float32);
    schema.add_property_key("balance", DataType::Float64);
    schema.add_property_key("isVerified", DataType::Bool);
    schema.add_property_key("sessionId", DataType::Uuid);
    schema.add_property_key("signupCode", DataType::UInt16);
    schema.add_property_key("avatarHash", DataType::Bytes);
    schema.add_property_key("weight", DataType::Float32);
    schema.add_property_key(VECTOR_PROP, DataType::FloatVector);

    schema.add_vector_index(VectorIndexConfig::new(
        VECTOR_PROP,
        VectorEntityType::Vertex,
        VECTOR_DIM,
        DistanceMetric::Cosine,
        AnnAlgorithm::Hnsw(HnswConfig::default()),
    ));

    schema.commit()
}
