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
use rocksgraph::{Graph, StoreError, TraversalBuilder, TxSession, Value, __};

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
/// Hotspot mode deliberately maximizes contention (a handful of keys hammered by every
/// thread), so it needs a much larger retry budget and a jittered delay instead of disjoint
/// mode's flat 1ms: with a flat delay, threads that lose a commit race tend to retry in
/// lockstep and re-collide. Without both of these, ops get dropped outright rather than just
/// slowed down, which defeats the point of measuring latency/throughput under contention.
const HOTSPOT_MAX_RETRIES: usize = 50;
const HOTSPOT_RETRY_DELAY_MS_RANGE: std::ops::RangeInclusive<u64> = 1..=10;
const VERTEX_LABEL: &str = "Person";
const EDGE_LABEL: &str = "Knows";
const NAME_KEY: &str = "name";
const AGE_KEY: &str = "age";
const WEIGHT_KEY: &str = "weight";
const TIMESTAMP_KEY: &str = "timestamp";

fn generate_random_string(len: usize) -> String {
    rand::thread_rng().sample_iter(rand::distributions::Alphanumeric).take(len).map(char::from).collect()
}

/// Upsert a vertex; returns `Ok(true)` if newly created, `Ok(false)` if it already existed.
fn upsert_vertex(tx: &mut TxSession, label: &str, vertex_id: i64) -> Result<bool, StoreError> {
    let mut rng = rand::thread_rng();
    let result = tx
        .g()
        .V([vertex_id])
        .count()
        .coalesce([
            __().V([vertex_id]).id(),
            __().addV(label)
                .property("id", vertex_id)
                .property(NAME_KEY, generate_random_string(10))
                .property(AGE_KEY, rng.gen_range::<i64, _>(18..100)),
        ])
        .next()?;

    match result {
        Some(Value::Int64(_)) => Ok(false),
        Some(Value::Vertex(_)) => Ok(true),
        Some(_) => unreachable!("unexpected gremlin result type"),
        None => unreachable!("unexpected gremlin result type"),
    }
}

/// Upsert an edge from `src` → `dst`; returns `Ok(true)` if newly created.
fn upsert_edge(tx: &mut TxSession, src: i64, dst: i64, edge_type: &str) -> Result<bool, StoreError> {
    let mut rng = rand::thread_rng();
    let result = tx
        .g()
        .V([src])
        .coalesce([
            __().outE([edge_type]).r#where(__().otherV().hasId([dst])).label(),
            __().addE(edge_type)
                .from(src)
                .to(dst)
                .property(WEIGHT_KEY, rng.gen_range::<f64, _>(0.1..10.0))
                .property(TIMESTAMP_KEY, rng.gen_range::<i64, _>(0..1_000_000)),
        ])
        .next()?;

    match result {
        Some(Value::String(_)) => Ok(false),
        Some(Value::Edge(_)) => Ok(true),
        Some(_) => unreachable!("unexpected gremlin result type"),
        None => unreachable!("unexpected gremlin result type"),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    run_with_args(args)
}

fn run_with_args(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = args
        .iter()
        .position(|arg| arg == "--data-dir")
        .and_then(|pos| args.get(pos + 1).map(PathBuf::from))
        .expect("Please provide a --data-dir fold path for the benchmark");

    let file_path =
        args.iter().position(|arg| arg == "--file-path").and_then(|pos| args.get(pos + 1).map(PathBuf::from));

    let parallelism = args
        .iter()
        .position(|arg| arg == "--parallelism")
        .and_then(|pos| args.get(pos + 1))
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(3);

    let mode = args
        .iter()
        .position(|arg| arg == "--mode")
        .and_then(|pos| args.get(pos + 1).map(|s| s.to_string()))
        .unwrap_or_else(|| "disjoint".to_string());

    let hotspot_keys = args
        .iter()
        .position(|arg| arg == "--hotspot-keys")
        .and_then(|pos| args.get(pos + 1))
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(10);

    let iterations = args
        .iter()
        .position(|arg| arg == "--iterations")
        .and_then(|pos| args.get(pos + 1))
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1000);

    let is_hotspot = mode == "hotspot";

    let graph = Graph::open(&data_dir)?;

    {
        use rocksgraph::schema::DataType;
        let mut mgmt = graph.open_management();
        mgmt.make_vertex_label(VERTEX_LABEL).make();
        mgmt.make_edge_label(EDGE_LABEL).make();
        mgmt.make_property_key(NAME_KEY, DataType::String).make();
        mgmt.make_property_key(AGE_KEY, DataType::Int64).make();
        mgmt.make_property_key(WEIGHT_KEY, DataType::Float64).make();
        mgmt.make_property_key(TIMESTAMP_KEY, DataType::Int64).make();
        mgmt.commit()?;
    }

    let lines = if is_hotspot {
        vec!["0 0".to_string(); iterations]
    } else if let Some(ref path) = file_path {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        reader.lines().collect::<Result<Vec<String>, _>>()?
    } else {
        panic!("Please provide a --file-path input graph path for the disjoint benchmark");
    };
    let lines = Arc::new(lines);

    let start = Instant::now();
    let mutation_counter = Arc::new(AtomicUsize::new(0));
    let line_count = lines.len();
    let chunk_size = (line_count + parallelism - 1).div_ceil(parallelism);

    let (hist_tx, hist_rx) = std::sync::mpsc::channel::<Histogram<u64>>();

    let mut worker_handles = vec![];
    for i in 0..parallelism {
        let lines_chunk = Arc::clone(&lines);
        let graph = graph.clone(); // cheap Arc clone
        let h_tx = hist_tx.clone();
        let mutation_counter = Arc::clone(&mutation_counter);

        let handle = std::thread::spawn(move || {
            let mut rng = rand::thread_rng();
            let mut local_hist = Histogram::<u64>::new(3).unwrap();
            let start_index = i * chunk_size;
            let end_index = (start_index + chunk_size).min(line_count);

            for idx in start_index..end_index {
                let (src, dst) = if is_hotspot {
                    let s = rng.gen_range(0..hotspot_keys) as i64;
                    let d = rng.gen_range(0..hotspot_keys) as i64;
                    (s, d)
                } else {
                    let line = &lines_chunk[idx];
                    let parts: Vec<i64> = line.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                    if parts.len() != 2 {
                        continue;
                    }
                    (parts[0], parts[1])
                };

                let op_start = Instant::now();
                let mut retry_count = 0;
                let max_retries_allowed = if is_hotspot { HOTSPOT_MAX_RETRIES } else { MAX_RETRIES };

                // `success` gates histogram recording below: a dropped/failed op's elapsed time
                // (which includes the full retry budget) must not be folded into the latency
                // distribution alongside genuine single-attempt commit times.
                let success = loop {
                    let mut tx = graph.begin();
                    match (|| -> Result<bool, StoreError> {
                        upsert_vertex(&mut tx, VERTEX_LABEL, src)?;
                        upsert_vertex(&mut tx, VERTEX_LABEL, dst)?;
                        let is_new = upsert_edge(&mut tx, src, dst, EDGE_LABEL)?;
                        tx.commit()?;
                        Ok(is_new)
                    })() {
                        Ok(is_new) => {
                            if is_new {
                                mutation_counter.fetch_add(1, Ordering::Relaxed);
                            }
                            break true;
                        }
                        Err(StoreError::Conflict) => {
                            retry_count += 1;
                            if retry_count > max_retries_allowed {
                                eprintln!(
                                    "Dropped op ({} -> {}) after {} retries: still conflicting",
                                    src, dst, max_retries_allowed
                                );
                                break false;
                            }
                            let delay_ms =
                                if is_hotspot { rng.gen_range(HOTSPOT_RETRY_DELAY_MS_RANGE) } else { RETRY_DELAY_MS };
                            std::thread::sleep(Duration::from_millis(delay_ms));
                        }
                        Err(e) => {
                            eprintln!("Dropped op ({} -> {}): fatal error: {}", src, dst, e);
                            break false;
                        }
                    }
                };

                if success {
                    local_hist.record(op_start.elapsed().as_nanos() as u64).unwrap();
                }
            }
            h_tx.send(local_hist).unwrap();
        });
        worker_handles.push(handle);
    }

    drop(hist_tx);
    for h in worker_handles {
        h.join().unwrap();
    }

    let mut final_hist: Option<Histogram<u64>> = None;
    for h in hist_rx {
        if let Some(ref mut fh) = final_hist {
            fh.add(h).unwrap();
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_bench_write_disjoint() {
        let dir = tempdir().unwrap();
        let file_dir = tempdir().unwrap();
        let file_path = file_dir.path().join("graph.txt");
        std::fs::write(&file_path, "1 2\n3 4\n").unwrap();

        let args = vec![
            "bench_write".to_string(),
            "--data-dir".to_string(),
            dir.path().to_str().unwrap().to_string(),
            "--file-path".to_string(),
            file_path.to_str().unwrap().to_string(),
            "--parallelism".to_string(),
            "1".to_string(),
            "--mode".to_string(),
            "disjoint".to_string(),
            "--iterations".to_string(),
            "2".to_string(),
        ];
        assert!(run_with_args(args).is_ok());
    }

    #[test]
    fn test_bench_write_hotspot() {
        let dir = tempdir().unwrap();
        let args = vec![
            "bench_write".to_string(),
            "--data-dir".to_string(),
            dir.path().to_str().unwrap().to_string(),
            "--parallelism".to_string(),
            "1".to_string(),
            "--mode".to_string(),
            "hotspot".to_string(),
            "--iterations".to_string(),
            "2".to_string(),
        ];
        assert!(run_with_args(args).is_ok());
    }
}
