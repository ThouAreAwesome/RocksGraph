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

use hdrhistogram::Histogram;
use rocksgraph::{
    types::prop_key::{ID, LABEL},
    GValue, Graph, Primitive, StoreError, TraversalBuilder, TxSession, __,
};

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
    time::{Duration, Instant},
};

const RETRY_DELAY_MS: u64 = 1;
const MAX_RETRIES: usize = 3;
const EDGE_LABEL: u16 = 2;

fn generate_random_string(len: usize) -> String {
    rand::thread_rng().sample_iter(rand::distributions::Alphanumeric).take(len).map(char::from).collect()
}

/// Upsert a vertex; returns `Ok(true)` if newly created, `Ok(false)` if it already existed.
fn upsert_vertex(tx: &mut TxSession, label: u16, vertex_id: i64) -> Result<bool, StoreError> {
    let mut rng = rand::thread_rng();
    let result = tx
        .g()
        .V([vertex_id])
        .count()
        .coalesce([
            __().V([vertex_id]).values([ID]),
            __().addV(label)
                .property(ID, vertex_id)
                .property("name", generate_random_string(10))
                .property("age", rng.gen_range::<i64, _>(18..100)),
        ])
        .next()?;

    match result {
        Some(GValue::Scalar(Primitive::Int64(_))) => Ok(false),
        Some(GValue::Vertex(_)) => Ok(true),
        Some(_) => unreachable!("unexpected gremlin result type"),
        None => unreachable!("unexpected gremlin result type"),
    }
}

/// Upsert an edge from `src` → `dst`; returns `Ok(true)` if newly created.
fn upsert_edge(tx: &mut TxSession, src: i64, dst: i64, edge_type: u16) -> Result<bool, StoreError> {
    let mut rng = rand::thread_rng();
    let result = tx
        .g()
        .V([src])
        .coalesce([
            __().outE([edge_type]).r#where(__().otherV().hasId([dst])).values([LABEL]),
            __().addE(edge_type)
                .from(src)
                .to(dst)
                .property("weight", rng.gen_range::<f64, _>(0.1..10.0))
                .property("timestamp", rng.gen_range::<i64, _>(0..1_000_000)),
        ])
        .next()?;

    match result {
        Some(GValue::Scalar(Primitive::Int32(_))) => Ok(false),
        Some(GValue::Edge(_)) => Ok(true),
        Some(_) => unreachable!("unexpected gremlin result type"),
        None => unreachable!("unexpected gremlin result type"),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let data_dir = args
        .iter()
        .position(|arg| arg == "--data-dir")
        .and_then(|pos| args.get(pos + 1).map(PathBuf::from))
        .expect("Please provide a --data-dir fold path for the benchmark");

    let file_path = args
        .iter()
        .position(|arg| arg == "--file-path")
        .and_then(|pos| args.get(pos + 1).map(PathBuf::from))
        .expect("Please provide a --file-path input graph path for the benchmark");

    let parallelism = args
        .iter()
        .position(|arg| arg == "--parallelism")
        .and_then(|pos| args.get(pos + 1))
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(3);

    let graph = Graph::open(&data_dir)?;

    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let lines: Arc<Vec<String>> = Arc::new(reader.lines().collect::<Result<_, _>>()?);

    let start = Instant::now();
    let mutation_counter = Arc::new(AtomicUsize::new(0));
    let line_count = lines.len();
    let chunk_size = (line_count + parallelism - 1).div_ceil(parallelism);

    use std::sync::mpsc;
    let (hist_tx, hist_rx) = mpsc::channel::<Histogram<u64>>();

    let mut worker_handles = vec![];
    for i in 0..parallelism {
        let lines_chunk = Arc::clone(&lines);
        let mutation_counter = Arc::clone(&mutation_counter);
        let graph = graph.clone(); // cheap Arc clone
        let hist_tx = hist_tx.clone();

        let handle = std::thread::spawn(move || {
            let _rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to create tokio runtime");

            let mut local_hist = Histogram::<u64>::new(3).expect("Failed to create histogram");
            let start_index = i * chunk_size;
            let end_index = (start_index + chunk_size).min(line_count);

            for line in &lines_chunk[start_index..end_index] {
                let parts: Vec<i64> =
                    line.split_whitespace().map(|s| s.parse::<i64>()).filter_map(Result::ok).collect();
                if parts.len() != 2 {
                    continue;
                }
                let (src, dst) = (parts[0], parts[1]);

                let op_start = Instant::now();
                let mut success = false;
                let mut new_edge = false;

                for attempt in 0..MAX_RETRIES {
                    // Each attempt uses a fresh transaction (correct for OCC retries).
                    let mut tx = graph.begin();

                    let staged = upsert_vertex(&mut tx, 1u16, src)
                        .and_then(|_| upsert_vertex(&mut tx, 1u16, dst))
                        .and_then(|_| upsert_edge(&mut tx, src, dst, EDGE_LABEL));

                    match staged {
                        Err(e) => {
                            println!("Failed to stage ({} -> {}): {}", src, dst, e);
                            break;
                        }
                        Ok(is_new) => match tx.commit() {
                            Ok(_) => {
                                success = true;
                                new_edge = is_new;
                                break;
                            }
                            Err(StoreError::Conflict) if attempt < MAX_RETRIES - 1 => {
                                // tx consumed by commit(); new one created next iteration.
                                std::thread::sleep(Duration::from_millis(RETRY_DELAY_MS));
                            }
                            Err(e) => {
                                println!("Commit failed ({} -> {}): {}", src, dst, e);
                                break;
                            }
                        },
                    }
                }

                if success {
                    let _ = local_hist.record(op_start.elapsed().as_nanos() as u64);
                    if new_edge {
                        mutation_counter.fetch_add(1, Ordering::Relaxed);
                    }
                } else {
                    println!("Failed to upsert edge ({} -> {}) after {} retries", src, dst, MAX_RETRIES);
                }
            }
            hist_tx.send(local_hist).unwrap();
        });
        worker_handles.push(handle);
    }

    drop(hist_tx);
    for handle in worker_handles {
        let _ = handle.join();
    }

    let mut final_hist: Option<Histogram<u64>> = None;
    for h in hist_rx {
        if let Some(ref mut main_h) = final_hist {
            main_h.add(h).unwrap();
        } else {
            final_hist = Some(h);
        }
    }

    let elapsed = start.elapsed().as_secs().max(1);
    let m_count = mutation_counter.load(Ordering::Relaxed) as u64;
    println!("Final — mutations: {}, ave speed: {} mutations/s", m_count, m_count / elapsed);

    if let Some(h) = final_hist {
        println!(
            "Latency (μs) — p50: {}, p90: {}, p95: {}, p99: {}, max: {}",
            h.value_at_quantile(0.5) as f64 / 1000.0,
            h.value_at_quantile(0.9) as f64 / 1000.0,
            h.value_at_quantile(0.95) as f64 / 1000.0,
            h.value_at_quantile(0.99) as f64 / 1000.0,
            h.max() as f64 / 1000.0
        );
    }

    #[cfg(feature = "rocksdb-stats")]
    if let Some(stats) = graph.statistics() {
        println!("\n--- RocksDB Statistics ---\n{}", stats);
    }

    Ok(())
}
