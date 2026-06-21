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
use rocksgraph::{Graph, ReadSession, StoreError, TraversalBuilder, Value, __};

use std::{
    env,
    fs::File,
    io::{BufRead, BufReader},
    sync::{mpsc, Arc},
    time::Instant,
};

const EDGE_LABEL: u16 = 2;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let data_dir = args
        .iter()
        .position(|arg| arg == "--data-dir")
        .and_then(|pos| args.get(pos + 1))
        .expect("Please provide a --data-dir path");

    let file_dir = args
        .iter()
        .position(|arg| arg == "--file-path")
        .and_then(|pos| args.get(pos + 1))
        .expect("Please provide a --file-path to specify the original graph file");

    let parallelism = args
        .iter()
        .position(|arg| arg == "--parallelism")
        .and_then(|pos| args.get(pos + 1))
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(3);

    let graph = Graph::open(data_dir)?;
    let file = File::open(file_dir)?;
    let reader = BufReader::new(file);
    let lines: Arc<Vec<String>> = Arc::new(reader.lines().collect::<Result<_, _>>()?);

    run_query_benchmark(
        "Q1: g.V().hasId(id).values('name', 'age').count()",
        &lines,
        &graph,
        parallelism,
        |snap, src, _dst| {
            let ct = snap.g().V([]).hasId([src]).values(["name", "age"]).count().next()?.unwrap();
            assert_eq!(ct, Value::Int64(2));
            Ok(())
        },
    )?;

    run_query_benchmark(
        "Q2: g.V().hasId(id).outE(label).where(otherV().hasId(dst)).values('weight', 'timestamp').count()",
        &lines,
        &graph,
        parallelism,
        |snap, src, dst| {
            let ct = snap
                .g()
                .V([])
                .hasId([src])
                .outE([EDGE_LABEL])
                .r#where(__().otherV().hasId([dst]))
                .values(["weight", "timestamp"])
                .count()
                .next()?
                .unwrap();
            assert_eq!(ct, Value::Int64(2));
            Ok(())
        },
    )?;

    run_query_benchmark(
        "Q3: g.V().hasId(id).bothE(label).values('weight', 'timestamp').count()",
        &lines,
        &graph,
        parallelism,
        |snap, src, _dst| {
            let Value::Int64(ct) = snap
                .g()
                .V([])
                .hasId([src])
                .bothE([EDGE_LABEL])
                .values(["weight", "timestamp"])
                .count()
                .next()?
                .unwrap()
            else {
                unreachable!("unexpected gremlin result type")
            };
            assert!(ct >= 2);
            Ok(())
        },
    )?;

    run_query_benchmark(
        "Q4: g.V().hasId(id).both(label).values('name', 'age').count()",
        &lines,
        &graph,
        parallelism,
        |snap, src, _dst| {
            let Value::Int64(ct) =
                snap.g().V([]).hasId([src]).both([EDGE_LABEL]).values(["name", "age"]).count().next()?.unwrap()
            else {
                unreachable!("unexpected gremlin result type")
            };
            assert!(ct >= 2);
            Ok(())
        },
    )?;

    run_query_benchmark(
        "Q5: g.V(id).out(label).both(label).count()",
        &lines,
        &graph,
        parallelism,
        |snap, src, _dst| {
            let Value::Int64(ct) = snap.g().V([src]).out([EDGE_LABEL]).both([EDGE_LABEL]).count().next()?.unwrap()
            else {
                unreachable!("unexpected gremlin result type")
            };
            assert!(ct >= 1);
            Ok(())
        },
    )?;

    // We run the full DB scan benchmarks with 5 sequential runs to measure stable database scan latencies.
    run_query_benchmark(
        "Q6: g.V().count() (Scan total vertices in DB)",
        &Arc::new(vec!["0 0".to_string(); 5]),
        &graph,
        1,
        |snap, _src, _dst| {
            let Value::Int64(ct) = snap.g().V([]).count().next()?.unwrap() else {
                unreachable!("unexpected gremlin result type")
            };
            println!("   [Scan Result] Total vertices: {}", ct);
            Ok(())
        },
    )?;

    run_query_benchmark(
        "Q7: g.E().count() (Scan total edges in DB)",
        &Arc::new(vec!["0 0".to_string(); 5]),
        &graph,
        1,
        |snap, _src, _dst| {
            let Value::Int64(ct) = snap.g().E([]).count().next()?.unwrap() else {
                unreachable!("unexpected gremlin result type")
            };
            println!("   [Scan Result] Total edges: {}", ct);
            Ok(())
        },
    )?;

    #[cfg(feature = "rocksdb-stats")]
    if let Some(stats) = graph.statistics() {
        println!("\n--- RocksDB Statistics ---\n{}", stats);
    }

    Ok(())
}

fn run_query_benchmark<F>(
    name: &str,
    lines: &Arc<Vec<String>>,
    graph: &Graph,
    parallelism: usize,
    query_fn: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: Fn(&mut ReadSession, i64, i64) -> Result<(), StoreError> + Send + Sync + 'static,
{
    println!("\n--- Running Benchmark for: {} ---", name);
    let start = Instant::now();
    let query_fn = Arc::new(query_fn);
    let line_count = lines.len();
    let chunk_size = (line_count + parallelism - 1).div_ceil(parallelism);

    let (hist_tx, hist_rx) = mpsc::channel::<Histogram<u64>>();

    let mut worker_handles = vec![];
    for i in 0..parallelism {
        let lines_chunk = Arc::clone(lines);
        let graph = graph.clone(); // cheap Arc clone
        let h_tx = hist_tx.clone();
        let query_fn = Arc::clone(&query_fn);

        let handle = std::thread::spawn(move || {
            // One snapshot per thread — reused across all queries in this chunk.
            let mut snap = graph.read();
            let mut local_hist = Histogram::<u64>::new(3).unwrap();
            let start_index = i * chunk_size;
            let end_index = (start_index + chunk_size).min(line_count);

            for line in &lines_chunk[start_index..end_index] {
                let parts: Vec<i64> = line.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                if parts.len() != 2 {
                    continue;
                }
                let (src, dst) = (parts[0], parts[1]);

                snap.clear_caches();
                let op_start = Instant::now();
                if let Err(e) = query_fn(&mut snap, src, dst) {
                    eprintln!("Query failed: {}", e);
                }
                local_hist.record(op_start.elapsed().as_nanos() as u64).unwrap();
            }
            h_tx.send(local_hist).unwrap();
        });
        worker_handles.push(handle);
    }

    drop(hist_tx);
    for h in worker_handles {
        h.join().unwrap();
    }

    let mut final_hist = Histogram::<u64>::new(3).unwrap();
    for h in hist_rx {
        final_hist.add(h).unwrap();
    }

    let elapsed_secs = start.elapsed().as_secs_f64();
    let ops = line_count as f64 / elapsed_secs;
    println!("Ops: {:.2}/s ({} queries in {:.2}s)", ops, line_count, elapsed_secs);
    println!(
        "Latency (μs) — p50: {}, p90: {}, p95: {}, p99: {}, max: {}",
        final_hist.value_at_quantile(0.5) as f64 / 1000.0,
        final_hist.value_at_quantile(0.9) as f64 / 1000.0,
        final_hist.value_at_quantile(0.95) as f64 / 1000.0,
        final_hist.value_at_quantile(0.99) as f64 / 1000.0,
        final_hist.max() as f64 / 1000.0
    );

    Ok(())
}
