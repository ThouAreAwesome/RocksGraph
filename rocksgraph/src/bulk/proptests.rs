// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Property-based round-trip testing for `BulkLoader`.
//!
//! The ~33 hand-written integration tests in `tests.rs` all reuse the same
//! 5-vertex/6-edge fixture — even the ones nominally about scale/spill
//! behavior just force the mechanism via extreme settings on that same
//! trivial graph. This module instead generates varied, randomized
//! vertex/edge data and asserts the core contract: every vertex/edge fed
//! into `BulkLoader` is retrievable afterward with identical id/label/
//! properties.
//!
//! Generated data deliberately avoids a few known encoding-layer edges that
//! are unrelated to `BulkLoader`'s own correctness (documented inline below)
//! so failures here point at the loader, not at pre-existing boundaries in
//! the property codec.

use std::collections::{HashMap, HashSet};

use proptest::prelude::*;
use tempfile::tempdir;

use super::loader::{BulkEdge, BulkVertex};
use crate::{
    gremlin::type_bridge::primitive_to_value, types::gvalue::Primitive, Graph, SmolStr, TraversalBuilder, Value,
};

const MAX_VERTICES: usize = 64;
const MAX_EDGES: usize = 64;
const MAX_SCHEMA_KEYS: usize = 8;
const MAX_STRING_CHARS: usize = 64;
const MAX_BYTES_LEN: usize = 512;
const MAX_VECTOR_DIM: usize = 32;

const VERTEX_LABELS: [&str; 2] = ["Person", "Company"];
const EDGE_LABELS: [&str; 2] = ["knows", "worksAt"];

/// Which `Primitive` shape a schema key uses. Fixed once per test case (see
/// `arb_schema`) so every record that uses a given key draws from the same
/// generator — Auto-mode schema resolution requires this: once a key's
/// `DataType` is established by the first record that uses it, a later
/// record using a *different* `Primitive` variant for that key fails with
/// `SchemaViolation` (see `test_bulk_loader_auto_mode_enforces_property_types`
/// in `tests.rs`). That's correct, already-tested behavior, not something
/// this test should trip over.
#[derive(Clone, Debug)]
enum ValueKind {
    Null,
    Bool,
    Int32,
    Int64,
    UInt16,
    Float32,
    Float64,
    String,
    Uuid,
    Bytes,
    FloatVector,
}

fn arb_value_kind() -> impl Strategy<Value = ValueKind> {
    prop_oneof![
        Just(ValueKind::Null),
        Just(ValueKind::Bool),
        Just(ValueKind::Int32),
        Just(ValueKind::Int64),
        Just(ValueKind::UInt16),
        Just(ValueKind::Float32),
        Just(ValueKind::Float64),
        Just(ValueKind::String),
        Just(ValueKind::Uuid),
        Just(ValueKind::Bytes),
        Just(ValueKind::FloatVector),
    ]
}

// Bounded well under the real limits so failures here are about BulkLoader,
// not the property codec's own boundaries: `Bytes` > 65535 bytes hits a hard
// `assert!` panic, and `String` > 65535 bytes silently corrupts the encoded
// blob (unchecked `as u16` cast on the length prefix, no guard at all).
fn arb_string() -> impl Strategy<Value = String> {
    proptest::collection::vec(proptest::char::range('\u{0020}', '\u{ffff}'), 0..MAX_STRING_CHARS)
        .prop_map(|chars| chars.into_iter().collect())
}

fn arb_bytes() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..MAX_BYTES_LEN)
}

// Finite-only: NaN/Inf round-trip via bit-pattern equality rather than
// `PartialEq`, which would make a plain `assert_eq!` oracle unreliable for
// something orthogonal to the round-trip invariant under test here.
fn arb_finite_f32() -> impl Strategy<Value = f32> {
    any::<f32>().prop_filter("finite", |f| f.is_finite())
}

fn arb_finite_f64() -> impl Strategy<Value = f64> {
    any::<f64>().prop_filter("finite", |f| f.is_finite())
}

fn arb_float_vector() -> impl Strategy<Value = Vec<f32>> {
    proptest::collection::vec(arb_finite_f32(), 0..MAX_VECTOR_DIM)
}

fn arb_primitive_for(kind: ValueKind) -> BoxedStrategy<Primitive> {
    match kind {
        ValueKind::Null => Just(Primitive::Null).boxed(),
        ValueKind::Bool => any::<bool>().prop_map(Primitive::Bool).boxed(),
        ValueKind::Int32 => any::<i32>().prop_map(Primitive::Int32).boxed(),
        ValueKind::Int64 => any::<i64>().prop_map(Primitive::Int64).boxed(),
        ValueKind::UInt16 => any::<u16>().prop_map(Primitive::UInt16).boxed(),
        ValueKind::Float32 => arb_finite_f32().prop_map(Primitive::Float32).boxed(),
        ValueKind::Float64 => arb_finite_f64().prop_map(Primitive::Float64).boxed(),
        ValueKind::String => arb_string().prop_map(|s| Primitive::String(SmolStr::from(s))).boxed(),
        ValueKind::Uuid => any::<u128>().prop_map(Primitive::Uuid).boxed(),
        ValueKind::Bytes => arb_bytes().prop_map(Primitive::Bytes).boxed(),
        ValueKind::FloatVector => arb_float_vector().prop_map(Primitive::FloatVector).boxed(),
    }
}

#[derive(Clone, Debug)]
struct SchemaEntry {
    key: String,
    kind: ValueKind,
}

/// A small, fixed `(key -> value-kind)` schema, generated once per test
/// case. Key names are index-derived (`prop_0`, `prop_1`, ...) so they can
/// never collide with the reserved `"id"`/`"label"`/`"rank"` property names
/// — `BulkLoader`'s own key-registration path has no guard against that
/// collision (unlike the `SchemaSession::declare_prop_key` path), so
/// generating one of those names here would test an unrelated latent issue
/// instead of the round-trip invariant.
fn arb_schema() -> impl Strategy<Value = Vec<SchemaEntry>> {
    (1usize..=MAX_SCHEMA_KEYS).prop_flat_map(|n| {
        proptest::collection::vec(arb_value_kind(), n).prop_map(|kinds| {
            kinds.into_iter().enumerate().map(|(i, kind)| SchemaEntry { key: format!("prop_{i}"), kind }).collect()
        })
    })
}

/// One record's property map: each schema key is independently included or
/// omitted, and included keys draw a value from that key's fixed generator.
fn arb_props(schema: &[SchemaEntry]) -> impl Strategy<Value = HashMap<String, Primitive>> {
    let per_key: Vec<_> = schema
        .iter()
        .map(|entry| {
            let key = entry.key.clone();
            proptest::option::of(arb_primitive_for(entry.kind.clone())).prop_map(move |v| (key.clone(), v))
        })
        .collect();
    per_key.prop_map(|pairs| pairs.into_iter().filter_map(|(k, v)| v.map(|val| (k, val))).collect())
}

fn arb_vertex_label() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just(VERTEX_LABELS[0]), Just(VERTEX_LABELS[1])]
}

fn arb_edge_label() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just(EDGE_LABELS[0]), Just(EDGE_LABELS[1])]
}

/// Vertex ids are `base + 0, base + 1, ...` — trivially unique within the
/// batch (required: unlike edges, `BulkLoader` has no dedup/clean-error path
/// for a repeated vertex id; a collision reaching `SstFileWriter::put`
/// violates RocksDB's "keys must be strictly increasing" contract and
/// surfaces as an opaque `StoreError::RocksDb`, not a domain error) while a
/// random `base` (via wrapping arithmetic, so it never panics near the
/// `i64` boundary) still exercises the full `VertexKey = i64` range,
/// including negative and near-`i64::MIN`/`MAX` ids.
fn arb_vertices(schema: Vec<SchemaEntry>) -> impl Strategy<Value = Vec<BulkVertex>> {
    (any::<i64>(), 1usize..=MAX_VERTICES).prop_flat_map(move |(base, n)| {
        let per_vertex: Vec<_> = (0..n as i64)
            .map(|i| {
                let id = base.wrapping_add(i);
                (arb_vertex_label(), arb_props(&schema)).prop_map(move |(label, props)| BulkVertex {
                    id,
                    label: label.into(),
                    props,
                })
            })
            .collect();
        per_vertex
    })
}

/// Edges sample `src`/`dst` only from the batch's own vertex ids (required:
/// `commit()` fails with `SchemaViolation` for an edge referencing a vertex
/// outside `load_vertices()`), then dedupe to unique `(src, label, dst)`
/// triples (required: default `EdgeMode::Single` cleanly errors
/// `DuplicateEdge` on a repeat, which would make the batch fail for a reason
/// unrelated to the round-trip invariant under test — `rank` is ignored
/// entirely in Single mode, always stored as `0`).
fn arb_edges(schema: Vec<SchemaEntry>, ids: Vec<i64>) -> impl Strategy<Value = Vec<BulkEdge>> {
    (0usize..=MAX_EDGES).prop_flat_map(move |n| {
        let ids = ids.clone();
        let schema = schema.clone();
        let per_edge: Vec<_> = (0..n)
            .map(|_| {
                (
                    proptest::sample::select(ids.clone()),
                    proptest::sample::select(ids.clone()),
                    arb_edge_label(),
                    arb_props(&schema),
                )
                    .prop_map(|(src, dst, label, props)| BulkEdge {
                        src,
                        dst,
                        label: label.into(),
                        props,
                        rank: None,
                    })
            })
            .collect();
        per_edge.prop_map(|edges| {
            let mut seen = HashSet::new();
            edges.into_iter().filter(|e| seen.insert((e.src, e.label.clone(), e.dst))).collect::<Vec<_>>()
        })
    })
}

fn arb_case() -> impl Strategy<Value = (Vec<BulkVertex>, Vec<BulkEdge>)> {
    arb_schema().prop_flat_map(|schema| {
        let schema_for_edges = schema.clone();
        arb_vertices(schema).prop_flat_map(move |vertices| {
            let ids: Vec<i64> = vertices.iter().map(|v| v.id).collect();
            let vertices_for_final = vertices.clone();
            arb_edges(schema_for_edges.clone(), ids).prop_map(move |edges| (vertices_for_final.clone(), edges))
        })
    })
}

fn expected_value_props(props: &HashMap<String, Primitive>) -> HashMap<SmolStr, Value> {
    props.iter().map(|(k, p)| (SmolStr::from(k.as_str()), primitive_to_value(p.clone()))).collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, .. ProptestConfig::default() })]

    /// Every vertex/edge fed into `BulkLoader` is retrievable afterward
    /// with identical id/label/properties.
    #[test]
    fn prop_bulkloader_roundtrip((vertices, edges) in arb_case()) {
        let dir = tempdir().unwrap();
        let graph = Graph::open(dir.path().join("db")).unwrap();

        let mut loader = graph.open_bulk_loader().unwrap();
        loader.load_vertices(vertices.clone()).unwrap();
        loader.load_edges(edges.clone()).unwrap();
        let stats = loader.commit().unwrap();

        prop_assert_eq!(stats.vertices_written as usize, vertices.len());
        prop_assert_eq!(stats.edges_written as usize, edges.len());

        let mut snap = graph.read();

        for v in &vertices {
            let expected = expected_value_props(&v.props);
            match snap.g().withProperties([]).V([v.id]).next().unwrap() {
                Some(Value::Vertex(vx)) => {
                    prop_assert_eq!(vx.label.as_str(), v.label.as_str());
                    prop_assert_eq!(vx.properties, expected);
                }
                other => prop_assert!(false, "expected Value::Vertex for id {}, got {:?}", v.id, other),
            }
        }

        for e in &edges {
            let expected = expected_value_props(&e.props);
            let results = snap.g().withProperties([]).V([e.src]).outE([e.label.as_str()]).to_list().unwrap();
            let found = results.into_iter().find_map(|v| match v {
                Value::Edge(ed) if ed.in_v == e.dst => Some(ed),
                _ => None,
            });
            match found {
                Some(ed) => {
                    prop_assert_eq!(ed.out_v, e.src);
                    prop_assert_eq!(ed.rank, 0u16);
                    prop_assert_eq!(ed.properties, expected);
                }
                None => prop_assert!(false, "edge {}->{} (label {}) not found", e.src, e.dst, e.label),
            }
        }

        graph.close().unwrap();
    }
}
