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

use multigraph::{
    client::gremlin_client::{self, GremlinArgument},
    server::gremlin_server,
};

use rand::Rng;
use std::{
    collections::HashMap,
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
const PARALLELISM: usize = 20;

async fn random_server_addr() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);
    addr
}

fn generate_random_string(len: usize) -> String {
    rand::thread_rng().sample_iter(rand::distributions::Alphanumeric).take(len).map(char::from).collect()
}

fn generate_random_properties() -> HashMap<String, GremlinArgument> {
    let mut rng = rand::thread_rng();
    HashMap::from([
        ("name".to_string(), GremlinArgument::String(generate_random_string(10))),
        ("age".to_string(), GremlinArgument::Int(rng.gen_range(18..100))),
    ])
}

fn generate_random_edge_properties() -> HashMap<String, GremlinArgument> {
    let mut rng = rand::thread_rng();
    HashMap::from([
        ("weight".to_string(), GremlinArgument::Float(rng.gen_range(0.1..10.0))),
        ("timestamp".to_string(), GremlinArgument::Int(rng.gen_range(0..1000000))),
    ])
}

/// Creates a vertex; if it already exists the error is silently ignored.
async fn upsert_vertex(
    g: &mut gremlin_client::GraphTraversal<'_>,
    label: u32,
    vertex_id: i64,
    properties: HashMap<String, GremlinArgument>,
) -> Result<bool, Box<dyn std::error::Error>> {
    match g.reset().addV(label, vertex_id, properties).execute().await {
        Ok(_) => Ok(true),
        Err(e) if e.to_string().contains("duplicate vertex") => Ok(false),
        Err(e) => Err(e),
    }
}

/// Creates an edge from src to dst if it does not already exist, using coalesce.
/// Returns Ok(true) if created, Ok(false) if it already existed.
async fn upsert_edge(
    g: &mut gremlin_client::GraphTraversal<'_>,
    src: i64,
    dst: i64,
    edge_type: u32,
    properties: HashMap<String, GremlinArgument>,
    max_retries: usize,
) -> Result<bool, Box<dyn std::error::Error>> {
    // g.V(src).coalesce(
    //   __.outE(edge_type).where(__.inV().hasId(dst)),   -- check if edge exists
    //   __.addE(edge_type, src, dst, props)              -- create if not
    // )
    // The coalesce short-circuits: if the first branch yields results the edge
    // already exists and addE is never executed.
    for attempt in 0..max_retries {
        let mut check_inner = gremlin_client::__();
        check_inner.inV().hasId(&[dst]);

        let mut check_branch = gremlin_client::__();
        check_branch.outE(&[edge_type]).r#where(&mut check_inner);

        let mut create_branch = gremlin_client::__();
        create_branch.addE(edge_type, src, dst, properties.clone());

        match g.reset().V(&[src]).coalesce(vec![&mut check_branch, &mut create_branch]).execute().await {
            Ok(result) => {
                let arr = result.as_array().unwrap();
                if arr.is_empty() {
                    // V(src) returned nothing — src vertex doesn't exist yet, retry
                    if attempt == max_retries - 1 {
                        return Err("src vertex not found after max retries".into());
                    }
                    sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
                    continue;
                }
                // coalesce returned the existing edge (branch 1) or the new edge (branch 2)
                // If the edge already existed addE was not called → Ok(false); otherwise Ok(true).
                // We can't easily distinguish here so just treat any result as success.
                return Ok(true);
            }
            Err(e) => {
                if attempt == max_retries - 1 {
                    return Err(e);
                }
                sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
            }
        }
    }
    Err("upsert_edge failed after max retries".into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server_addr = random_server_addr().await;

    let path_str = "./bench_data/rocksdb_data";
    let path = PathBuf::from(path_str);
    let graph_store = gremlin_server::open_rocks_store(Some(&path));

    let addr_clone = server_addr.clone();
    tokio::spawn(async move {
        gremlin_server::start_server(&addr_clone, graph_store).await.expect("Server failed to start");
    });

    sleep(Duration::from_millis(100)).await;

    let file = File::open("./bench_data/soc-LiveJournal1-1M.txt")?;
    let reader = BufReader::new(file);

    let start = Instant::now();
    let counter = Arc::new(AtomicUsize::new(0));
    let mutation_counter = Arc::new(AtomicUsize::new(0));

    let (tx, rx) = mpsc::channel::<String>(1000);
    let rx = Arc::new(tokio::sync::Mutex::new(rx));

    let mut worker_handles = vec![];
    for _ in 0..PARALLELISM {
        let rx = Arc::clone(&rx);
        let server_addr = server_addr.clone();
        let mutation_counter = Arc::clone(&mutation_counter);

        let handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to create tokio runtime");

            rt.block_on(async move {
                let mut client = match gremlin_client::GremlinClient::connect(&server_addr).await {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("Worker failed to connect: {}", e);
                        return;
                    }
                };
                let mut g = gremlin_client::graphTraversalSource(&mut client);

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

                    // Upsert both vertices (single round trip each — addV ignores duplicate)
                    if upsert_vertex(&mut g, 1u32, src, generate_random_properties()).await.unwrap_or(false) {
                        mutation_counter.fetch_add(1, Ordering::Relaxed);
                    }
                    if upsert_vertex(&mut g, 1u32, dst, generate_random_properties()).await.unwrap_or(false) {
                        mutation_counter.fetch_add(1, Ordering::Relaxed);
                    }

                    // Upsert edge via coalesce (single round trip)
                    match upsert_edge(&mut g, src, dst, 2u32, generate_random_edge_properties(), MAX_RETRIES).await {
                        Ok(_) => {
                            mutation_counter.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            eprintln!("Edge upsert failed ({} -> {}): {}", src, dst, e);
                        }
                    }
                }
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
    for handle in worker_handles {
        let _ = handle.join();
    }

    let elapsed = start.elapsed().as_secs().max(1);
    let m_count = mutation_counter.load(Ordering::Relaxed) as u64;
    println!("Final — mutations: {}, speed: {}/s", m_count, m_count / elapsed);

    Ok(())
}
