// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Transactional OCC write benchmark: incrementally upserts an edge-list file into a
//! RocksGraph database using `TxnSession` + Gremlin `coalesce()` (idempotent upsert),
//! measuring OLTP-style write throughput as an alternative to `BulkLoader`.
//!
//! Usage:
//! ```text
//! bench_write_occ --data-dir <path> --file-path <path> [--parallelism N]  (default: 3)
//! ```

use hdrhistogram::Histogram;
use rocksgraph::{Graph, StoreError, TraversalBuilder, TxnSession, __};

use rand::Rng;
use std::{
    env,
    fs::File,
    io::{BufRead, BufReader},
    path::PathBuf,
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

const VERTEX_LABEL: &str = "Person";
const EDGE_LABEL: &str = "Knows";
const NAME_KEY: &str = "name";
const AGE_KEY: &str = "age";
const WEIGHT_KEY: &str = "weight";
const TIMESTAMP_KEY: &str = "timestamp";

const MAX_RETRIES: usize = 5;
const RETRY_DELAY_MS: u64 = 1;

fn generate_random_string(len: usize) -> String {
    rand::thread_rng().sample_iter(rand::distributions::Alphanumeric).take(len).map(char::from).collect()
}

/// Upserts a vertex by id. `.V([id]).count()` always yields exactly one traverser
/// (0 or 1 as an Int64), so `coalesce()` runs its check-then-create branches exactly
/// once regardless of whether the vertex already exists.
fn upsert_vertex(txn: &mut TxnSession, id: i64) -> Result<(), StoreError> {
    let mut rng = rand::thread_rng();
    let age = rng.gen_range(18..100i64);
    txn.g()
        .V([id])
        .count()
        .coalesce([
            __().V([id]).id(),
            __().addV(VERTEX_LABEL)
                .property("id", id)
                .property(NAME_KEY, generate_random_string(10))
                .property(AGE_KEY, age),
        ])
        .next()?;
    Ok(())
}

/// Upserts the edge `src -> dst`. `src` is guaranteed to already exist in this
/// transaction's overlay (its own upsert runs first), so `.V([src])` alone always
/// yields exactly one traverser.
fn upsert_edge(txn: &mut TxnSession, src: i64, dst: i64) -> Result<(), StoreError> {
    let mut rng = rand::thread_rng();
    let weight = rng.gen_range(0.1..10.0f64);
    let timestamp = rng.gen_range(0..1_000_000i64);
    txn.g()
        .V([src])
        .coalesce([
            __().outE([EDGE_LABEL]).r#where(__().otherV().hasId([dst])).values([WEIGHT_KEY]),
            __().addE(EDGE_LABEL).from(src).to(dst).property(WEIGHT_KEY, weight).property(TIMESTAMP_KEY, timestamp),
        ])
        .next()?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    run_with_args(args)
}

fn run_with_args(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = args
        .iter()
        .position(|a| a == "--data-dir")
        .and_then(|p| args.get(p + 1).map(PathBuf::from))
        .expect("--data-dir <path> is required");

    let file_path = args
        .iter()
        .position(|a| a == "--file-path")
        .and_then(|p| args.get(p + 1).map(PathBuf::from))
        .expect("--file-path <path> is required");

    let parallelism = args
        .iter()
        .position(|a| a == "--parallelism")
        .and_then(|p| args.get(p + 1))
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(3);

    if data_dir.exists() {
        std::fs::remove_dir_all(&data_dir)?;
    }

    let file = File::open(&file_path)?;
    let lines: Arc<Vec<String>> = Arc::new(BufReader::new(file).lines().collect::<Result<_, _>>()?);
    let line_count = lines.len();

    let graph = Graph::open(&data_dir)?;

    let start = Instant::now();
    let chunk_size = (line_count + parallelism - 1).div_ceil(parallelism);
    let (hist_tx, hist_rx) = mpsc::channel::<(Histogram<u64>, usize)>();

    let mut handles = vec![];
    for i in 0..parallelism {
        let lines_chunk = Arc::clone(&lines);
        let graph = graph.clone(); // cheap Arc clone
        let h_tx = hist_tx.clone();

        let handle = std::thread::spawn(move || {
            let mut local_hist = Histogram::<u64>::new(3).unwrap();
            let mut mutations = 0usize;
            let start_index = i * chunk_size;
            let end_index = (start_index + chunk_size).min(line_count);

            for line in &lines_chunk[start_index..end_index] {
                let parts: Vec<i64> = line.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                if parts.len() != 2 {
                    continue;
                }
                let (src, dst) = (parts[0], parts[1]);

                let op_start = Instant::now();
                for attempt in 0..MAX_RETRIES {
                    let mut txn = graph.begin();
                    let staged = upsert_vertex(&mut txn, src)
                        .and_then(|_| upsert_vertex(&mut txn, dst))
                        .and_then(|_| upsert_edge(&mut txn, src, dst));

                    match staged.and_then(|_| txn.commit()) {
                        Ok(_) => {
                            mutations += 3; // 2 vertex upserts + 1 edge upsert
                            break;
                        }
                        Err(StoreError::Conflict) if attempt + 1 < MAX_RETRIES => {
                            std::thread::sleep(Duration::from_millis(RETRY_DELAY_MS));
                        }
                        Err(e) => {
                            eprintln!("Commit failed ({src} -> {dst}): {e}");
                            break;
                        }
                    }
                }
                local_hist.record(op_start.elapsed().as_nanos() as u64).unwrap();
            }
            h_tx.send((local_hist, mutations)).unwrap();
        });
        handles.push(handle);
    }
    drop(hist_tx);
    for h in handles {
        h.join().unwrap();
    }

    let mut final_hist = Histogram::<u64>::new(3).unwrap();
    let mut total_mutations = 0usize;
    for (h, m) in hist_rx {
        final_hist.add(h).unwrap();
        total_mutations += m;
    }

    let elapsed = start.elapsed().as_secs_f64();
    println!("=== Transactional OCC Write Complete ===");
    println!("Edges processed: {line_count} (each upserts 2 vertices + 1 edge, {total_mutations} mutations total)");
    println!("Elapsed:         {elapsed:.2}s");
    println!("Throughput:      {:.0} edges/s", line_count as f64 / elapsed);
    println!(
        "Latency (μs) — p50: {:.1}, p90: {:.1}, p95: {:.1}, p99: {:.1}, max: {:.1}",
        final_hist.value_at_quantile(0.5) as f64 / 1000.0,
        final_hist.value_at_quantile(0.9) as f64 / 1000.0,
        final_hist.value_at_quantile(0.95) as f64 / 1000.0,
        final_hist.value_at_quantile(0.99) as f64 / 1000.0,
        final_hist.max() as f64 / 1000.0
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_bench_write_occ() {
        let dir = tempdir().unwrap();
        let file_dir = tempdir().unwrap();
        let file_path = file_dir.path().join("graph.txt");
        std::fs::write(&file_path, "1 2\n2 3\n3 1\n").unwrap();

        let args = vec![
            "bench_write_occ".to_string(),
            "--data-dir".to_string(),
            dir.path().join("db").to_str().unwrap().to_string(),
            "--file-path".to_string(),
            file_path.to_str().unwrap().to_string(),
            "--parallelism".to_string(),
            "1".to_string(),
        ];
        assert!(run_with_args(args).is_ok());
    }
}
