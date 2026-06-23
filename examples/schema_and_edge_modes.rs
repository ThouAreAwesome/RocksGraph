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

//! An advanced example demonstrating schema modes and edge modes:
//! 1. Configuring Strict Schema Mode (`SchemaMode::Strict`).
//! 2. Explicitly declaring labels and properties using `SchemaManagement`.
//! 3. Experiencing Schema Violation errors when strict rules are violated.
//! 4. Enabling and utilizing Multi-Edge Mode (`EdgeMode::Multi`) with user-specified ranks.

use rocksgraph::{
    schema::{DataType, EdgeMode, GraphOptions, SchemaMode},
    Graph, StoreError, TraversalBuilder,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // We use a temporary directory so the example runs cleanly and cleans up on exit.
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path();
    println!("Initializing Graph Database at: {:?}", db_path);

    // --- Phase 1: Configuring and Declaring Schema in Strict Mode ---
    println!("\n=== Part 1: Strict Schema Mode ===");

    // Set custom options: strict mode prevents implicit/auto registration.
    let options = GraphOptions { mode: SchemaMode::Strict, edge_mode: EdgeMode::Single };

    let graph = Graph::open_with_options(db_path, options)?;

    // Declare the schema before any write reaches the engine
    let mut mgmt = graph.open_management();
    mgmt.make_vertex_label("person").make();
    mgmt.make_edge_label("knows").make();
    mgmt.make_property_key("name", DataType::String).make();
    mgmt.make_property_key("age", DataType::Int32).make();
    mgmt.make_property_key("weight", DataType::Float64).make();
    mgmt.commit()?;
    println!("Schema successfully declared and committed in Strict Mode.");

    // Write some valid data matching the declared schema
    let mut tx = graph.begin();
    tx.g().addV("person").property("id", 1i64).property("name", "Alice").property("age", 30i32).next()?;

    tx.g().addV("person").property("id", 2i64).property("name", "Bob").property("age", 25i32).next()?;
    tx.commit()?;
    println!("Valid data committed successfully.");

    // Try to write data using an undeclared label or property key
    let mut tx = graph.begin();
    let res = tx
        .g()
        .addV("ghost") // Undeclared label, will violate Strict schema mode
        .property("id", 3i64)
        .next();

    match res {
        Err(StoreError::SchemaViolation(msg)) => {
            println!("Caught expected schema violation: {}", msg);
        }
        _ => panic!("Expected a SchemaViolation error for undeclared vertex label 'ghost'!"),
    }
    tx.rollback();

    // --- Phase 2: Upgrading to Multi-Edge Mode ---
    println!("\n=== Part 2: Upgrading to Multi-Edge Mode ===");

    // In order to write multiple edges between the same pair of vertices, we must
    // explicitly upgrade the graph's edge mode to EdgeMode::Multi.
    // Note: Schema updates are done atomically via CAS.
    let mut mgmt = graph.open_management();
    mgmt.set_edge_mode(EdgeMode::Multi);
    mgmt.commit()?;
    println!("EdgeMode upgraded to Multi successfully.");

    // Add first edge with default rank (0)
    let mut tx = graph.begin();
    tx.g().addE("knows").from(1).to(2).property("weight", 0.8f64).next()?;
    tx.commit()?;
    println!("First 'knows' edge (rank 0, default) added successfully.");

    // If we try to write a second edge without specifying a different rank,
    // the system will detect it as a duplicate edge.
    let mut tx = graph.begin();
    let res = tx.g().addE("knows").from(1).to(2).property("weight", 0.9f64).next();

    match res {
        Err(StoreError::DuplicateEdge(key)) => {
            println!("Caught expected duplicate edge error at rank 0: {}", key);
        }
        _ => panic!("Expected a DuplicateEdge error since rank defaults to 0!"),
    }
    tx.rollback();

    // To add a second edge between the same vertices under EdgeMode::Multi,
    // we must supply an explicit and unique structural rank as a u16 property named "rank".
    let mut tx = graph.begin();
    tx.g()
        .addE("knows")
        .from(1)
        .to(2)
        .property("rank", 1u16) // Specify rank 1 to distinguish it from rank 0
        .property("weight", 0.9f64)
        .next()?;
    tx.commit()?;
    println!("Second 'knows' edge with explicit rank 1 added successfully.");

    // Query and count both edges
    let mut snap = graph.read();
    let edge_count = snap.g().V([1]).outE(["knows"]).count().next()?.unwrap();
    println!("Total incident edges from Alice to Bob: {:?}", edge_count);

    Ok(())
}
