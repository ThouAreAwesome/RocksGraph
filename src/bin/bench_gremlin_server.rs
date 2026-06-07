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
    engine::volcano::builder::PhysicalPlanBuilder,
    graph::LogicalGraph,
    optimizer::apply_rules,
    planner::logical_step::{
        AddEStep, AddVStep, CoalesceStep, HasIdStep, HasPropertyStep, LogicalPlan, LogicalStep, OtherVStep, OutEStep,
        PropertyStep, VStep, WhereStep,
    },
    server::{config::Config, gremlin_server},
    store::{GraphStore, RocksStorage},
    types::{error::StoreError, gvalue::Primitive},
};
use smol_str::SmolStr;

use rand::Rng;
use std::{
    collections::HashMap,
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
    let mut plan = LogicalPlan {
        steps: vec![
            LogicalStep::AddV(AddVStep { label_id: label, vertex_id: Some(vertex_id), properties: HashMap::new() }),
            LogicalStep::Property(PropertyStep {
                prop_key: SmolStr::new("name"),
                prop_value: Primitive::String(generate_random_string(10).into()),
            }),
            LogicalStep::Property(PropertyStep {
                prop_key: SmolStr::new("age"),
                prop_value: Primitive::Int64(rng.gen_range(18..100)),
            }),
        ],
    };
    let _ = apply_rules(&mut plan).unwrap();

    let mut builder: PhysicalPlanBuilder = Default::default();
    let physical_plan = builder.build(&plan);

    match physical_plan.next(graph) {
        Ok(Some(_)) => Ok(true),
        Ok(None) => Ok(false), // vertex already exists (addV is idempotent)
        Err(e) => Err(e),
    }
}

/// Creates an edge from src to dst if it does not already exist, using coalesce.
/// Returns Ok(true) if created, Ok(false) if it already existed.
fn upsert_edge(graph: &mut LogicalGraph<RocksStorage>, src: i64, dst: i64, edge_type: u16) -> Result<bool, StoreError> {
    // g.V(src).coalesce(
    //   __.outE(edge_type).where(__.otherV().hasId(dst)),   -- check if edge exists
    //   __.addE(edge_type, src, dst, props)              -- create if not
    // )
    // The coalesce short-circuits: if the first branch yields results the edge
    // already exists and addE is never executed.
    let mut rng = rand::thread_rng();
    let mut upsert_e_plan = LogicalPlan {
        steps: vec![
            LogicalStep::V(VStep { ids: vec![] }),
            LogicalStep::HasProperty(HasPropertyStep { key: SmolStr::new("id"), value: Primitive::Int64(src) }),
            LogicalStep::Coalesce(CoalesceStep {
                plans: vec![
                    LogicalPlan {
                        steps: vec![
                            LogicalStep::OutE(OutEStep { label_ids: vec![edge_type], end_vertex_ids: None }),
                            LogicalStep::Where(WhereStep {
                                plan: LogicalPlan {
                                    steps: vec![
                                        LogicalStep::OtherV(OtherVStep {}),
                                        LogicalStep::HasId(HasIdStep { ids: vec![dst] }),
                                    ],
                                },
                            }),
                        ],
                    },
                    LogicalPlan {
                        steps: vec![
                            LogicalStep::AddE(AddEStep {
                                label_id: edge_type,
                                out_v_id: Some(src),
                                in_v_id: Some(dst),
                                properties: HashMap::new(),
                            }),
                            LogicalStep::Property(PropertyStep {
                                prop_key: SmolStr::new("weight"),
                                prop_value: Primitive::Float64(rng.gen_range(0.1..10.0)),
                            }),
                            LogicalStep::Property(PropertyStep {
                                prop_key: SmolStr::new("timestamp"),
                                prop_value: Primitive::Int64(rng.gen_range(0..1000000)),
                            }),
                        ],
                    },
                ],
            }),
        ],
    };
    let _ = apply_rules(&mut upsert_e_plan).unwrap();

    let mut builder: PhysicalPlanBuilder = Default::default();
    let physical_plan = builder.build(&upsert_e_plan);

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

    let graph_store = gremlin_server::open_rocks_store(Some(&config.storage.data_dir))?;

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
        let mutation_counter = Arc::clone(&mutation_counter);
        let store = Arc::clone(&graph_store);

        let handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to create tokio runtime");

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
