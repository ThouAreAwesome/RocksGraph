// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of MultiGraph.
//
// Use of this software is governed by the Business Source License 1.1
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date (2030-01-01), in accordance with the Business Source
// License, use of this software will be governed by the Apache License 2.0.
//
// SPDX-License-Identifier: BUSL-1.1

use hdrhistogram::Histogram;
use multigraph::{
    graph::LogicalGraph,
    gremlin::{
        config::Config,
        traversal::{self, graphTraversalSource, __},
    },
    store::{GraphStore, RocksStorage},
    types::{error::StoreError, gvalue::Primitive, prop_key::ID},
};
use smol_str::SmolStr;

use rand::Rng;
use std::{
    env,
    fs::File,
    io::{BufRead, BufReader},
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};
use tokio::{
    sync::mpsc,
    time::{sleep, Duration},
};

const RETRY_DELAY_MS: u64 = 5;
const MAX_RETRIES: usize = 3;
// best concurrent number
const PARALLELISM: usize = 3;

fn generate_random_string(len: usize) -> String {
    rand::thread_rng().sample_iter(rand::distributions::Alphanumeric).take(len).map(char::from).collect()
}

/// Creates a vertex; if it already exists the error is silently ignored.
fn upsert_vertex(graph: &mut LogicalGraph<RocksStorage>, label: u16, vertex_id: i64) -> Result<bool, StoreError> {
    let mut rng = rand::thread_rng();
    let mut traversal = graphTraversalSource();
    traversal
        .addV(label)
        .property(ID, Primitive::Int64(vertex_id))
        .property(SmolStr::new("name"), Primitive::String(generate_random_string(10).into()))
        .property(SmolStr::new("age"), Primitive::Int64(rng.gen_range(18..100)));
    let physical_plan = traversal.build();

    match physical_plan.next(graph) {
        Ok(Some(_)) => Ok(true),
        Ok(None) => Ok(false), // vertex already exists (addV is idempotent)
        Err(e) => Err(e),
    }
}

/// Creates an edge from src to dst if it does not already exist, using coalesce.
/// Returns Ok(true) if created, Ok(false) if it already existed.
fn upsert_edge(graph: &mut LogicalGraph<RocksStorage>, src: i64, dst: i64, edge_type: u16) -> Result<bool, StoreError> {
    let mut rng = rand::thread_rng();

    // Using the fluent API to construct the query
    let mut traversal = graphTraversalSource();
    traversal.V(&[src]).coalesce(vec![
        __().outE(&[edge_type]).r#where(__().otherV().hasId(&[dst])),
        __().addE(edge_type)
            .from(src)
            .to(dst)
            .property(SmolStr::new("weight"), Primitive::Float64(rng.gen_range(0.1..10.0)))
            .property(SmolStr::new("timestamp"), Primitive::Int64(rng.gen_range(0..1000000))),
    ]);

    let physical_plan = traversal.build();

    match physical_plan.next(graph) {
        Ok(Some(_)) => Ok(true),
        Ok(None) => Ok(false),
        Err(e) => Err(e),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let config_path = if let Some(pos) = args.iter().position(|arg| arg == "--config") {
        args.get(pos + 1).map(PathBuf::from)
    } else {
        None
    }
    .expect("Please provide a --config file path for the benchmark");

    let config = Config::from_file(&config_path)?;

    let graph_store = traversal::open_rocks_store(Some(&config.storage.data_dir))?;

    let file = File::open("./bench_data/soc-LiveJournal1-1M.txt")?;
    let reader = BufReader::new(file);

    let start = Instant::now();
    let counter = Arc::new(AtomicUsize::new(0));
    let mutation_counter = Arc::new(AtomicUsize::new(0));

    let (tx, rx) = mpsc::channel::<String>(1000);
    let (hist_tx, mut hist_rx) = mpsc::channel::<Histogram<u64>>(PARALLELISM);
    let rx = Arc::new(tokio::sync::Mutex::new(rx));

    let mut worker_handles = vec![];
    for _ in 0..PARALLELISM {
        let rx = Arc::clone(&rx);
        let mutation_counter = Arc::clone(&mutation_counter);
        let store = Arc::clone(&graph_store);
        let hist_tx = hist_tx.clone();

        let handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to create tokio runtime");

            let mut local_hist = Histogram::<u64>::new(3).expect("Failed to create histogram");
            rt.block_on(async move {
                loop {
                    let line = {
                        let mut lock = rx.lock().await;
                        lock.recv().await
                    };

                    let line = match line {
                        Some(l) => l,
                        None => break,
                    };

                    let parts: Vec<i64> =
                        line.split_whitespace().map(|s| s.parse::<i64>()).filter_map(Result::ok).collect();

                    if parts.len() != 2 {
                        continue;
                    }

                    let (src, dst) = (parts[0], parts[1]);

                    let op_start = Instant::now();
                    let mut lg = LogicalGraph::new(store.begin());
                    let mut success = false;

                    for attempt in 0..MAX_RETRIES {
                        // 1. Stage the mutations in the overlay
                        let v1_new = upsert_vertex(&mut lg, 1u16, src).unwrap_or(false);
                        let v2_new = upsert_vertex(&mut lg, 1u16, dst).unwrap_or(false);
                        let e_new = upsert_edge(&mut lg, src, dst, 2u16).unwrap_or(false);

                        // 2. Attempt to commit the transaction
                        match lg.commit() {
                            Ok(_) => {
                                if v1_new {
                                    mutation_counter.fetch_add(1, Ordering::Relaxed);
                                }
                                if v2_new {
                                    mutation_counter.fetch_add(1, Ordering::Relaxed);
                                }
                                if e_new {
                                    mutation_counter.fetch_add(1, Ordering::Relaxed);
                                }
                                success = true;
                                let _ = local_hist.record(op_start.elapsed().as_micros() as u64);
                                break;
                            }
                            Err(StoreError::Conflict) => {
                                if attempt < MAX_RETRIES - 1 {
                                    sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
                                    // LogicalGraph::commit() automatically calls reset() on conflict,
                                    // so 'lg' is fresh and ready for the next attempt.
                                }
                            }
                            Err(e) => {
                                eprintln!("Transaction failed with non-retryable error ({} -> {}): {}", src, dst, e);
                                break;
                            }
                        }
                    }

                    if !success {
                        eprintln!("Failed to upsert edge ({} -> {}) after {} retries", src, dst, MAX_RETRIES);
                    }
                }
                let _ = hist_tx.send(local_hist).await;
            })
        });
        worker_handles.push(handle);
    }

    for line in reader.lines() {
        let line = line?;
        tx.send(line).await?;

        let current_count = counter.fetch_add(1, Ordering::Relaxed) + 1;
        if current_count.is_multiple_of(10000) {
            let elapsed = start.elapsed().as_secs().max(1);
            let m_count = mutation_counter.load(Ordering::Relaxed) as u64;
            println!("Read {} lines | mutation speed: {}/s", current_count, m_count / elapsed);
        }
    }

    drop(tx);
    drop(hist_tx);

    for handle in worker_handles {
        let _ = handle.join();
    }

    let mut final_hist: Option<Histogram<u64>> = None;
    while let Some(h) = hist_rx.recv().await {
        if let Some(ref mut main_h) = final_hist {
            main_h.add(h).unwrap();
        } else {
            final_hist = Some(h);
        }
    }

    let elapsed = start.elapsed().as_secs().max(1);
    let m_count = mutation_counter.load(Ordering::Relaxed) as u64;
    println!("Final — mutations: {}, speed: {}/s", m_count, m_count / elapsed);

    if let Some(h) = final_hist {
        println!(
            "Latency (μs) — p50: {}_us, p90: {}_us, p95: {}_us, p99: {}_us, max: {}_us",
            h.value_at_quantile(0.5),
            h.value_at_quantile(0.9),
            h.value_at_quantile(0.95),
            h.value_at_quantile(0.99),
            h.max()
        );
    }

    Ok(())
}
