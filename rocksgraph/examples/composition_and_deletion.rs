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

//! Demonstrates composing traversals and deleting graph elements:
//! 1. `union()` — merging the result streams of several sub-traversals.
//! 2. `coalesce()` — idempotent upserts (first non-empty branch wins), run twice to prove
//!    the second run takes the "already exists" branch instead of erroring on a duplicate.
//! 3. `drop()` — deleting properties, edges, and vertices: `properties([key, ...]).drop()`
//!    removes just that property (leaving the rest of the element untouched), while `drop()`
//!    on a vertex/edge traverser removes the whole element — including the structural rule that
//!    a vertex with incident edges must have them dropped first (`StoreError::IncidentEdges`).

use rocksgraph::{Graph, StoreError, TraversalBuilder, __};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let graph = Graph::open(temp_dir.path())?;

    let mut tx = graph.begin();
    tx.g().addV("person").property("id", 1i64).property("name", "marko").next()?;
    tx.g().addV("person").property("id", 2i64).property("name", "vadas").next()?;
    tx.g().addV("software").property("id", 3i64).property("name", "lop").next()?;
    tx.g().addE("knows").from(1).to(2).property("weight", 0.5f64).property("note", "old friends").next()?;
    tx.g().addE("created").from(1).to(3).property("weight", 0.4f64).next()?;
    tx.commit()?;

    // --- union(): merge the result streams of multiple sub-traversals ---
    println!("=== union() ===");
    let mut snap = graph.read();
    let outgoing_edge_count = snap.g().V([1]).union([__().outE(["knows"]), __().outE(["created"])]).count().next()?;
    println!("marko's outE(knows) + outE(created), merged and counted: {:?}", outgoing_edge_count);

    let names_and_id = snap.g().V([1]).union([__().values(["name"]), __().values(["id"])]).to_list()?;
    println!("marko's name and id, merged into one stream: {:?}", names_and_id);

    // --- coalesce(): idempotent upserts ---
    println!("\n=== coalesce() (idempotent upsert) ===");
    for attempt in 1..=2 {
        let mut tx = graph.begin();
        // Upsert vertex: emit the existing name if id 42 already exists, otherwise create it.
        // `coalesce()` only evaluates its branches once per *incoming* traverser, so it needs a
        // seed step ahead of it. `.V([42])` alone won't do — it filters out missing ids, so on
        // the first run (42 doesn't exist yet) it would emit zero traversers and coalesce would
        // never fire either branch. `.count()` always emits exactly one traverser (a count of 0
        // or 1) regardless of whether 42 exists yet, which is what reliably drives coalesce here.
        let vertex_result = tx
            .g()
            .V([42])
            .count()
            .coalesce([
                __().V([42]).values(["name"]),
                __().addV("person").property("id", 42i64).property("name", "charlie"),
            ])
            .next()?;
        // Upsert edge: emit it if marko already knows 42, otherwise create the edge.
        let edge_result = tx
            .g()
            .V([1])
            .coalesce([
                __().outE(["knows"]).r#where(__().otherV().hasId([42])),
                __().addE("knows").from(1).to(42).property("weight", 0.9f64),
            ])
            .next()?;
        tx.commit()?;
        println!("attempt {}: vertex branch -> {:?}, edge branch -> {:?}", attempt, vertex_result, edge_result);
    }

    // --- drop(): deleting a single property ---
    println!("\n=== drop() on a property ===");
    let mut tx = graph.begin();
    // `properties([key, ...])` carries the property element itself, not the owning vertex/edge,
    // so `drop()` here removes only "note" — "weight" on the same edge is untouched.
    tx.g().V([1]).outE(["knows"]).properties(["note"]).drop().next()?;
    tx.commit()?;
    let mut snap = graph.read();
    let note_after = snap.g().V([1]).outE(["knows"]).values(["note"]).next()?;
    let weight_after = snap.g().V([1]).outE(["knows"]).values(["weight"]).next()?;
    println!("knows-edge note after drop: {:?}", note_after);
    println!("knows-edge weight after drop (untouched): {:?}", weight_after);

    // Dropping a property key that was never set is a no-op, not an error.
    let mut tx = graph.begin();
    tx.g().V([1]).properties(["never_set"]).drop().next()?;
    tx.commit()?;

    // --- drop(): deleting edges and vertices ---
    println!("\n=== drop() on edges and vertices ===");
    let mut tx = graph.begin();

    // A vertex with incident edges cannot be dropped directly.
    let blocked = tx.g().V([3]).drop().next();
    match blocked {
        Err(StoreError::IncidentEdges) => {
            println!("dropping lop (id 3) while it has incident edges: blocked, as expected")
        }
        other => panic!("expected StoreError::IncidentEdges, got {:?}", other),
    }
    tx.rollback();

    // Drop the incident edge first, then the vertex succeeds.
    let mut tx = graph.begin();
    tx.g().V([1]).outE(["created"]).drop().next()?;
    tx.g().V([3]).drop().next()?;
    tx.commit()?;
    println!("dropped the created-edge, then vertex lop (id 3): succeeded");

    let mut snap = graph.read();
    let lop_still_there = snap.g().V([3]).next()?;
    println!("lop after drop: {:?}", lop_still_there);

    Ok(())
}
