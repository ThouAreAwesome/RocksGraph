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

use crate::api::Graph;
use crate::gremlin::traversal::TraversalBuilder;
use crate::schema::definition::{DataType, EdgeMode, GraphOptions, SchemaMode};
use crate::types::StoreError;
use tempfile::tempdir;

#[test]
fn test_management_explicit_declaration_and_cas() {
    let dir = tempdir().unwrap();
    let graph = Graph::open(dir.path()).unwrap();

    // Check initial empty schema version is 0
    {
        let schema = graph.schema();
        assert_eq!(schema.read().unwrap().version, 0);
    }

    // Declare vertex label, edge label, property key
    {
        let mut mgmt = graph.open_management();
        mgmt.make_vertex_label("person").make();
        mgmt.make_edge_label("knows").make();
        mgmt.make_property_key("age", DataType::Int32).make();
        mgmt.commit().unwrap();
    }

    // Check schema version bumped to 1, and declarations persisted
    {
        let schema = graph.schema();
        let s = schema.read().unwrap();
        assert_eq!(s.version, 1);
        assert!(s.vertex_label_id("person").is_some());
        assert!(s.edge_label_id("knows").is_some());
        assert!(s.prop_key_id("age").is_some());
    }

    // Re-open graph to verify persistence of schema entries
    drop(graph);
    let graph_reopened = Graph::open(dir.path()).unwrap();
    {
        let schema = graph_reopened.schema();
        let s = schema.read().unwrap();
        assert_eq!(s.version, 1);
        assert!(s.vertex_label_id("person").is_some());
        assert!(s.edge_label_id("knows").is_some());
        assert!(s.prop_key_id("age").is_some());
    }

    // Test CAS (Compare-And-Swap) version validation conflict
    let mut mgmt1 = graph_reopened.open_management();
    let mut mgmt2 = graph_reopened.open_management();

    mgmt1.make_vertex_label("software").make();
    mgmt1.commit().unwrap(); // Increments version to 2

    mgmt2.make_vertex_label("project").make();
    let err = mgmt2.commit().unwrap_err();
    assert!(matches!(err, StoreError::SchemaConflict(_)));

    // Test edge mode multiplicity ratchet: Single -> Multi allowed
    {
        let mut mgmt = graph_reopened.open_management();
        mgmt.set_edge_mode(EdgeMode::Multi);
        mgmt.commit().unwrap();
    }
    {
        let schema = graph_reopened.schema();
        assert_eq!(schema.read().unwrap().edge_mode, EdgeMode::Multi);
    }

    // Edge mode multiplicity ratchet: Multi -> Single rejected
    {
        let mut mgmt = graph_reopened.open_management();
        mgmt.set_edge_mode(EdgeMode::Single);
        let err = mgmt.commit().unwrap_err();
        assert!(matches!(err, StoreError::SchemaConflict(_)));
    }
}

#[test]
fn test_schema_mode_auto_implicit_writes_and_types() {
    let dir = tempdir().unwrap();
    let graph =
        Graph::open_with_options(dir.path(), GraphOptions { mode: SchemaMode::Auto, edge_mode: EdgeMode::Single })
            .unwrap();

    // 1. Implicit write registers label and key on-the-fly
    {
        let mut tx = graph.begin();
        tx.g().addV("person").property("id", 1i64).property("name", "Alice").next().unwrap();
        tx.commit().unwrap();
    }

    {
        let schema = graph.schema();
        let s = schema.read().unwrap();
        assert!(s.vertex_label_id("person").is_some());
        assert!(s.prop_key_id("name").is_some());
        assert_eq!(s.prop_key_types.get(&s.prop_key_id("name").unwrap()).unwrap().data_type, DataType::String);
    }

    // 2. Mismatching property type is rejected at write time
    {
        let mut tx = graph.begin();
        // "name" was registered as String, so Int32 should fail
        let err = tx.g().addV("person").property("id", 2i64).property("name", 123i32).next().unwrap_err();
        assert!(matches!(err, StoreError::SchemaViolation(_)));
    }

    // 3. Rollback recovery (aborted tx does not pollute the persisted schema)
    {
        let mut tx = graph.begin();
        // Resolve a new vertex label "animal" and prop key "species" inside the transaction
        tx.g().addV("animal").property("id", 3i64).property("species", "cat").next().unwrap();
        tx.rollback(); // Rollback!
    }

    // Re-open graph from disk to verify database schema CF is clean
    drop(graph);
    let graph_reopened = Graph::open(dir.path()).unwrap();
    {
        let schema = graph_reopened.schema();
        let s = schema.read().unwrap();
        assert!(s.vertex_label_id("animal").is_none());
        assert!(s.prop_key_id("species").is_none());
    }
}

/// Regression test: a `commit()` batch that fails partway through (here, a property-key
/// redeclaration with an incompatible type) must not leave any earlier item in the same
/// batch ("ghost") registered in the live `Schema`, even though that earlier item validated
/// cleanly on its own. The whole batch is atomic: either everything lands, or nothing does.
#[test]
fn test_management_commit_atomic_on_partial_failure() {
    let dir = tempdir().unwrap();
    let graph = Graph::open(dir.path()).unwrap();

    {
        let mut mgmt = graph.open_management();
        mgmt.make_property_key("age", DataType::Int32).make();
        mgmt.commit().unwrap();
    }
    assert_eq!(graph.schema().read().unwrap().version, 1);

    {
        let mut mgmt = graph.open_management();
        mgmt.make_vertex_label("ghost").make();
        mgmt.make_property_key("age", DataType::Int64).make(); // conflicts with existing Int32
        let err = mgmt.commit().unwrap_err();
        assert!(matches!(err, StoreError::SchemaConflict(_)));
    }

    let schema = graph.schema();
    let s = schema.read().unwrap();
    assert_eq!(s.version, 1, "version must not change on a failed commit");
    assert!(s.vertex_label_id("ghost").is_none(), "ghost must not leak into the live schema from a failed batch");
}

/// Regression test: `commit()` must be a true no-op — no `version` bump, no RocksDB write —
/// when nothing in the batch actually changes anything: either nothing was staged at all, or
/// every staged item is an idempotent redeclaration of an already-identical entry.
#[test]
fn test_management_commit_noop_does_not_bump_version() {
    let dir = tempdir().unwrap();
    let graph = Graph::open(dir.path()).unwrap();

    // A completely empty commit() stages nothing.
    {
        let mgmt = graph.open_management();
        mgmt.commit().unwrap();
    }
    assert_eq!(graph.schema().read().unwrap().version, 0);

    // Declare "age" for the first time -> version bumps to 1.
    {
        let mut mgmt = graph.open_management();
        mgmt.make_property_key("age", DataType::Int32).make();
        mgmt.commit().unwrap();
    }
    assert_eq!(graph.schema().read().unwrap().version, 1);

    // Re-declaring "age" with the identical type is idempotent -> no version bump.
    {
        let mut mgmt = graph.open_management();
        mgmt.make_property_key("age", DataType::Int32).make();
        mgmt.commit().unwrap();
    }
    assert_eq!(graph.schema().read().unwrap().version, 1, "idempotent redeclare must not bump version");
}

/// Regression test: a single write that introduces exactly one new vertex label must bump
/// `version` by exactly 1 — once, at the point the label is registered — not once more when
/// the registration is later flushed to RocksDB at transaction commit.
#[test]
fn test_auto_mode_version_bumps_once_per_new_label() {
    let dir = tempdir().unwrap();
    let graph = Graph::open(dir.path()).unwrap();
    assert_eq!(graph.schema().read().unwrap().version, 0);
    {
        let mut tx = graph.begin();
        // "id" is a reserved, pre-registered key, so this introduces exactly one new
        // thing: the vertex label "person".
        tx.g().addV("person").property("id", 1i64).next().unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(graph.schema().read().unwrap().version, 1);
}

#[test]
fn test_schema_mode_strict_rejections() {
    let dir = tempdir().unwrap();
    let graph =
        Graph::open_with_options(dir.path(), GraphOptions { mode: SchemaMode::Strict, edge_mode: EdgeMode::Single })
            .unwrap();

    // 1. Write with undeclared vertex label is rejected at compile time
    {
        let mut tx = graph.begin();
        let err = tx.g().addV("person").property("id", 1i64).next().unwrap_err();
        assert!(matches!(err, StoreError::SchemaViolation(_)));
    }

    // Let's declare "person", "knows", and "name"
    {
        let mut mgmt = graph.open_management();
        mgmt.make_vertex_label("person").make();
        mgmt.make_edge_label("knows").make();
        mgmt.make_property_key("name", DataType::String).make();
        mgmt.commit().unwrap();
    }

    // 2. Now writing declared vertex label and property works
    {
        let mut tx = graph.begin();
        tx.g().addV("person").property("id", 1i64).property("name", "Alice").next().unwrap();
        tx.commit().unwrap();
    }

    // 3. Write with undeclared property key is rejected
    {
        let mut tx = graph.begin();
        let err = tx.g().addV("person").property("id", 1i64).property("age", 30i32).next().unwrap_err();
        assert!(matches!(err, StoreError::SchemaViolation(_)));
    }

    // 4. Read query referencing unregistered label/key is rejected at compile time
    {
        let mut tx = graph.begin();
        // "animal" label not registered
        let err = tx.g().V([]).hasLabel(["animal"]).next().unwrap_err();
        assert!(matches!(err, StoreError::SchemaViolation(_)));

        // "age" property key not registered
        let err = tx.g().V([]).has("age", 30i32).next().unwrap_err();
        assert!(matches!(err, StoreError::SchemaViolation(_)));

        // "purchased" edge label not registered -- the traversal-step (not just
        // hasLabel/has) read paths are gated the same way.
        let err = tx.g().V([]).out(["purchased"]).next().unwrap_err();
        assert!(matches!(err, StoreError::SchemaViolation(_)));
    }
}

/// `GraphOptions` only seeds a brand-new database. Re-opening an existing one with
/// different options must not change its persisted `mode`/`edge_mode` (design doc §0).
#[test]
fn test_open_with_options_ignored_on_existing_db() {
    let dir = tempdir().unwrap();
    {
        let graph = Graph::open_with_options(
            dir.path(),
            GraphOptions { mode: SchemaMode::Strict, edge_mode: EdgeMode::Single },
        )
        .unwrap();
        assert_eq!(graph.schema().read().unwrap().mode, SchemaMode::Strict);
    }

    // Re-open with the opposite options -- the persisted Strict/Single must win.
    let reopened =
        Graph::open_with_options(dir.path(), GraphOptions { mode: SchemaMode::Auto, edge_mode: EdgeMode::Multi })
            .unwrap();
    let s = reopened.schema();
    let s = s.read().unwrap();
    assert_eq!(s.mode, SchemaMode::Strict, "persisted schema_mode must win over new GraphOptions");
    assert_eq!(s.edge_mode, EdgeMode::Single, "persisted edge_mode must win over new GraphOptions");
}

/// Design doc §4 consistency table: "`resolve_*` also increments `version`... so a
/// `SchemaManagement` staged concurrently with an Auto-mode write that registers a
/// brand-new name will correctly see its `base_version` go stale and get
/// `StoreError::Conflict` at `commit()`, even though the racing write was a regular
/// traversal, not another management session."
#[test]
fn test_auto_mode_write_invalidates_concurrent_schema_management_session() {
    let dir = tempdir().unwrap();
    let graph = Graph::open(dir.path()).unwrap();

    // Open a management session first, capturing base_version = 0.
    let mut mgmt = graph.open_management();
    mgmt.make_vertex_label("project").make();

    // A regular Auto-mode write races ahead and registers a brand-new label, bumping
    // `version` to 1.
    {
        let mut tx = graph.begin();
        tx.g().addV("person").property("id", 1i64).next().unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(graph.schema().read().unwrap().version, 1);

    // The stale management session must now see a CAS conflict, not silently apply.
    let err = mgmt.commit().unwrap_err();
    assert!(matches!(err, StoreError::SchemaConflict(_)));
}

/// Auto mode: a read-side filter naming a label or property key that has never been
/// registered must produce zero results, not an error and not "match everything" (design
/// doc §6/§7 -- the empty-`label_ids`/dangling-`prop_key_id` traps).
#[test]
fn test_auto_mode_unresolved_read_filters_yield_zero_results() {
    let dir = tempdir().unwrap();
    let graph = Graph::open(dir.path()).unwrap();
    {
        let mut tx = graph.begin();
        tx.g().addV("person").property("id", 1i64).next().unwrap();
        tx.commit().unwrap();
    }

    let mut tx = graph.begin();
    // Never-registered edge label on a traversal step.
    assert!(tx.g().V([1]).out(["never_registered"]).to_list().unwrap().is_empty());
    // Never-registered vertex label on hasLabel.
    assert!(tx.g().V([1]).hasLabel(["never_registered"]).to_list().unwrap().is_empty());
    // Never-registered property key on has().
    assert!(tx.g().V([1]).has("never_registered", 1i32).to_list().unwrap().is_empty());
}

/// `LogicalGraph::set_property`'s type check (design doc §5a Challenge B) applies to edge
/// properties exactly as it does to vertex properties.
#[test]
fn test_edge_property_type_mismatch_rejected() {
    let dir = tempdir().unwrap();
    let graph = Graph::open(dir.path()).unwrap();
    let mut tx = graph.begin();
    tx.g().addV("person").property("id", 1i64).next().unwrap();
    tx.g().addV("person").property("id", 2i64).next().unwrap();
    tx.g().addE("knows").from(1).to(2).property("since", 2020i32).next().unwrap();

    // "since" was registered as Int32 on the edge above; a String now must be rejected.
    let err = tx.g().addE("knows").from(1).to(2).property("since", "a long time").next().unwrap_err();
    assert!(matches!(err, StoreError::SchemaViolation(_)));
}

/// Regression test for a concurrency pathology in Auto mode: `resolve_vertex_label`/
/// `resolve_edge_label`/`resolve_prop_key` used to take the `Schema` write lock
/// unconditionally from `build_step`, even on the overwhelmingly common path where the
/// name was already registered. With several threads concurrently doing `addV`/`addE`/
/// `.property(...)` against a small, shared set of labels/keys, that constant stream of
/// write-lock acquisitions starved Rust's write-preferring `RwLock` badly enough to look
/// like (and in practice run for many minutes as) a hang — reproduced with as few as 3
/// concurrent writer threads. `build_step` now tries a read-lock lookup first and only
/// escalates to the write lock on a genuine miss.
///
/// Runs the concurrent workload on a background thread and bounds the wait with
/// `recv_timeout`, so a regression fails this test in a few seconds instead of hanging the
/// whole suite.
#[test]
fn test_concurrent_auto_mode_writes_do_not_starve_schema_lock() {
    use crate::{api::TxSession, gremlin::traversal::__, gremlin::value::Key};

    // Mirrors `bench_write`'s upsert pattern: a `.coalesce([check, addV/addE])` so build_step
    // resolves names for *both* branches every call, plus an `outE().where(otherV().hasId(..))`
    // edge check, which together touch the schema lock several times per transaction (a mix
    // of read-side lookups and write-side `resolve_*` calls).
    fn upsert_vertex(tx: &mut TxSession, vertex_id: i64) {
        tx.g()
            .V([vertex_id])
            .count()
            .coalesce([
                __().V([vertex_id]).values([Key::Id]),
                __().addV("person").property("id", vertex_id).property("name", "x").property("age", 30i32),
            ])
            .next()
            .unwrap();
    }
    fn upsert_edge(tx: &mut TxSession, src: i64, dst: i64) {
        tx.g()
            .V([src])
            .coalesce([
                __().outE(["knows"]).r#where(__().otherV().hasId([dst])).values([Key::Label]),
                __().addE("knows").from(src).to(dst).property("weight", 1.0f64).property("since", 0i64),
            ])
            .next()
            .unwrap();
    }

    let dir = tempdir().unwrap();
    let graph = Graph::open(dir.path()).unwrap();

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        const THREADS: i64 = 8;
        const PAIRS_PER_THREAD: i64 = 60;

        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let graph = graph.clone();
                std::thread::spawn(move || {
                    for i in 0..PAIRS_PER_THREAD {
                        let src = t * 1000 + i;
                        let dst = src + 1;
                        // Every thread races to (re-)register the same handful of
                        // labels/keys, then retries on the expected OCC conflict (the
                        // same shared-metadata-key race covered by
                        // `test_management_explicit_declaration_and_cas`) exactly like
                        // `bench_write`'s retry loop does.
                        for attempt in 0..5 {
                            let mut tx = graph.begin();
                            upsert_vertex(&mut tx, src);
                            upsert_vertex(&mut tx, dst);
                            upsert_edge(&mut tx, src, dst);
                            if tx.commit().is_ok() || attempt == 4 {
                                break;
                            }
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let _ = done_tx.send(());
    });

    done_rx
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("concurrent Auto-mode writes did not complete within 20s -- schema lock contention regressed");
}

/// `PropertyKeyMaker::cardinality()` is staged and applied like the other builder fields.
#[test]
fn test_property_key_maker_cardinality_builder() {
    use crate::schema::definition::Cardinality;

    let dir = tempdir().unwrap();
    let graph = Graph::open(dir.path()).unwrap();
    {
        let mut mgmt = graph.open_management();
        mgmt.make_property_key("tags", DataType::String).cardinality(Cardinality::Single).make();
        mgmt.commit().unwrap();
    }
    let schema = graph.schema();
    let s = schema.read().unwrap();
    let id = s.prop_key_id("tags").unwrap();
    assert_eq!(s.prop_key_types.get(&id).unwrap().cardinality, Cardinality::Single);
}
