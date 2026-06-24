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

//! A basic example demonstrating the foundational RocksGraph API:
//! 1. Initializing an embedded RocksGraph instance.
//! 2. Creating a read-only query session and traversing vertices.
//! 3. Writing data inside a transaction.
//! 4. Handling Optimistic Concurrency Control (OCC) conflicts with retries.

use rocksgraph::{Graph, StoreError, TraversalBuilder, Value};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Initialize the Graph Database
    // We use a temporary directory so the example runs cleanly and cleans up on exit.
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path();
    println!("Initializing Graph Database at: {:?}", db_path);

    // Open (or create) the graph. By default, it opens in SchemaMode::Auto
    // which automatically registers labels and property keys on first use.
    let graph = Graph::open(db_path)?;

    // 2. Perform a Write Transaction
    // Write queries must borrow a TxSession from graph.begin().
    println!("\n--- Phase 1: Writing Initial Graph Data ---");
    let mut tx = graph.begin();

    // Add a vertex for Marko
    tx.g().addV("person").property("id", 1i64).property("name", "marko").property("age", 29i32).next()?;

    // Add a vertex for Vadas
    tx.g().addV("person").property("id", 2i64).property("name", "vadas").property("age", 27i32).next()?;

    // Add a "knows" edge from Marko to Vadas
    tx.g().addE("knows").from(1).to(2).property("weight", 0.5f64).next()?;

    // Commit the transaction to flush changes atomically to RocksDB.
    tx.commit()?;
    println!("Graph data successfully committed!");

    // 3. Build a Read-Only Traversal Session
    // Read-only queries use graph.read() which incurs no transaction overhead.
    println!("\n--- Phase 2: Querying the Graph (Read-Only Session) ---");
    let mut snap = graph.read();

    // Query: Get Marko's age
    let marko_age = snap.g().V([1]).values(["age"]).next()?;
    println!("Marko's age: {:?}", marko_age);

    // Query: Find all people Marko knows
    let friends = snap.g().V([1]).out(["knows"]).values(["name"]).to_list()?;
    println!("People Marko knows: {:?}", friends);

    // 4. Handle OCC transaction conflicts with a retry loop
    // RocksGraph uses Optimistic Concurrency Control. If two sessions write concurrently
    // to the same keys, the second session's commit() will return StoreError::Conflict.
    // The application is responsible for retrying the transaction.
    println!("\n--- Phase 3: Writing with OCC Conflict Retry Loop ---");
    let mut retries = 0;
    loop {
        let mut tx = graph.begin();

        // Increment Marko's age by 1
        let current_age = tx
            .g()
            .V([1])
            .values(["age"])
            .next()?
            .and_then(|v| match v {
                Value::Int32(age) => Some(age),
                _ => None,
            })
            .unwrap_or(29);

        tx.g().V([1]).property("age", current_age + 1).next()?;

        // Attempt to commit
        match tx.commit() {
            Ok(_) => {
                println!("Successfully updated Marko's age to {} after {} retries.", current_age + 1, retries);
                break;
            }
            Err(StoreError::Conflict) => {
                // Another thread/process modified Marko's age in the meantime. Retry!
                retries += 1;
                println!("OCC Conflict detected! Retrying transaction (attempt {})...", retries);
                continue;
            }
            Err(e) => {
                // Hard error occurred, stop retrying.
                return Err(Box::new(e));
            }
        }
    }

    Ok(())
}
