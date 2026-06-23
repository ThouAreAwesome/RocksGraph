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

use rand::Rng;
use std::{
    sync::{Arc, Barrier},
    thread,
    time::Duration,
};

use crate::{schema::DataType, Graph, StoreError, TraversalBuilder, Value, __};

#[test]
fn test_hotspot_contention_preserves_all_updates() {
    let (done_tx, done_rx) = std::sync::mpsc::channel();

    let dir = tempfile::tempdir().unwrap();
    let graph = Graph::open(dir.path()).unwrap();

    // 1. Declare the counter schema & seed it
    {
        let mut mgmt = graph.open_management();
        mgmt.make_vertex_label("System").make();
        mgmt.make_property_key("counter", DataType::Int64).make();
        mgmt.commit().unwrap();

        let mut tx = graph.begin();
        tx.g().addV("System").property("id", 0i64).property("counter", 0i64).next().unwrap();
        tx.commit().unwrap();
    }

    let graph_clone = graph.clone();
    std::thread::spawn(move || {
        const THREADS: usize = 8;
        const ITERATIONS: usize = 50;

        let barrier = Arc::new(Barrier::new(THREADS));
        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let graph = graph_clone.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let mut local_successes = 0;
                    let mut rng = rand::thread_rng();

                    barrier.wait(); // start all threads at once

                    for i in 0..ITERATIONS {
                        let mut success = false;
                        for _attempt in 0..60 {
                            let mut tx = graph.begin();
                            let val = tx.g().V([0i64]).values(["counter"]).next().unwrap();
                            let current = match val {
                                Some(Value::Int64(c)) => c,
                                _ => panic!("expected int64 counter"),
                            };

                            tx.g().V([0i64]).property("counter", current + 1).next().unwrap();

                            match tx.commit() {
                                Ok(_) => {
                                    local_successes += 1;
                                    success = true;
                                    break;
                                }
                                Err(StoreError::Conflict) => {
                                    // Randomized backoff sleep (1-10ms) to break lockstep scheduling
                                    let delay = rng.gen_range(1..=10);
                                    thread::sleep(Duration::from_millis(delay));
                                }
                                Err(e) => {
                                    panic!("Thread {} iteration {} failed with unexpected error: {:?}", t, i, e);
                                }
                            }
                        }
                        if !success {
                            panic!("Thread {} iteration {} starved after 60 attempts", t, i);
                        }
                    }
                    local_successes
                })
            })
            .collect();

        let mut total_successes = 0;
        for h in handles {
            total_successes += h.join().unwrap();
        }
        done_tx.send(total_successes).unwrap();
    });

    // Watchdog check: fail fast if it takes longer than 30s
    let total_commits = done_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("hotspot contention test timed out (deadlock or starvation)");

    // Verification
    let mut tx = graph.begin();
    let final_val = tx.g().V([0i64]).values(["counter"]).next().unwrap();
    let final_counter = match final_val {
        Some(Value::Int64(c)) => c,
        _ => panic!("expected int64 final counter"),
    };

    assert_eq!(final_counter, total_commits as i64, "Lost updates detected!");
}

#[test]
fn test_disjoint_writes_never_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let graph = Graph::open(dir.path()).unwrap();

    // Pre-declare schema to prevent auto-schema registration from causing conflicts
    {
        let mut mgmt = graph.open_management();
        mgmt.make_vertex_label("User").make();
        mgmt.make_edge_label("Connects").make();
        mgmt.make_property_key("weight", DataType::Float64).make();
        mgmt.commit().unwrap();
    }

    const THREADS: usize = 6;
    const ITERATIONS: usize = 100;

    let barrier = Arc::new(Barrier::new(THREADS));
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let graph = graph.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();

                for i in 0..ITERATIONS {
                    let src_id = (t * 10000 + i) as i64;
                    let dst_id = (t * 10000 + 5000 + i) as i64;

                    let mut tx = graph.begin();

                    tx.g().addV("User").property("id", src_id).next().unwrap();

                    tx.g().addV("User").property("id", dst_id).next().unwrap();

                    tx.g().addE("Connects").from(src_id).to(dst_id).property("weight", 0.5f64).next().unwrap();

                    // Disjoint writes should commit successfully on the first attempt with zero
                    // conflicts. Match explicitly rather than a generic `.expect()` so a failure
                    // for any *other* reason doesn't get mislabeled as the conflict this test is
                    // specifically checking for.
                    match tx.commit() {
                        Ok(_) => {}
                        Err(StoreError::Conflict) => {
                            panic!(
                                "Disjoint write transaction conflicted unexpectedly (src={}, dst={})",
                                src_id, dst_id
                            )
                        }
                        Err(e) => panic!("Disjoint write transaction failed with unexpected error: {:?}", e),
                    }
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    // Single-threaded validation pass — read-only, so a snapshot is the right tool (no need to
    // pay for OCC read-set tracking on a pass that never writes).
    let mut read = graph.read();
    for t in 0..THREADS {
        for i in 0..ITERATIONS {
            let src_id = (t * 10000 + i) as i64;
            let dst_id = (t * 10000 + 5000 + i) as i64;

            let src_exists = read.g().V([src_id]).next().unwrap().is_some();
            let dst_exists = read.g().V([dst_id]).next().unwrap().is_some();
            assert!(src_exists, "Source vertex {} not found", src_id);
            assert!(dst_exists, "Destination vertex {} not found", dst_id);

            let weight = read
                .g()
                .V([src_id])
                .outE(["Connects"])
                .r#where(__().otherV().hasId([dst_id]))
                .values(["weight"])
                .next()
                .unwrap();
            assert_eq!(
                weight,
                Some(Value::Float64(0.5)),
                "Edge from {} to {} missing or has unexpected weight",
                src_id,
                dst_id
            );
        }
    }
}
