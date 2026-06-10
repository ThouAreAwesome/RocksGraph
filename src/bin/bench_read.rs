// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of MultiGraph.
//
// Use of this software is governed by the Business Source License 1.1
// included in the LICENSE file at the root of this repository.
//
// SPDX-License-Identifier: BUSL-1.1

use hdrhistogram::Histogram;
use multigraph::{
    graph::LogicalGraph,
    gremlin::traversal::{self, graphTraversalSource, __},
    store::{GraphStore, RocksStorage},
    types::error::StoreError,
};

use smol_str::SmolStr;
use std::{
    env,
    fs::File,
    io::{BufRead, BufReader},
    sync::{mpsc, Arc},
    time::Instant,
};

pub const EDGE_LABEL: u16 = 2;

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
        .unwrap_or(3); // Default parallelism

    let graph_store = traversal::open_rocks_store(Some(&data_dir))?;
    let file = File::open(file_dir)?;
    let reader = BufReader::new(file);
    let lines: Arc<Vec<String>> = Arc::new(reader.lines().collect::<Result<_, _>>()?);

    // --- Query 1 ---
    run_query_benchmark(
        "Q1: g.V().hasId(id).values('name', 'age').count()",
        &lines,
        &graph_store,
        parallelism,
        |lg, src, _dst| {
            let mut t = graphTraversalSource();
            t.V(&[]).hasId(&[src]).values(&[SmolStr::new("name"), SmolStr::new("age")]).count();
            let p = t.build()?;
            while p.next(lg)?.is_some() {}
            Ok(())
        },
    )?;

    // --- Query 2 ---
    run_query_benchmark(
        "Q2: g.V().hasId(id).outE(label).where(otherV().hasId(dst)).values('weight', 'timestamp').count()",
        &lines,
        &graph_store,
        parallelism,
        |lg, src, dst| {
            let mut t = graphTraversalSource();
            t.V(&[])
                .hasId(&[src])
                .outE(&[EDGE_LABEL])
                .r#where(__().otherV().hasId(&[dst]))
                .values(&[SmolStr::new("weight"), SmolStr::new("timestamp")])
                .count();
            let p = t.build()?;
            while p.next(lg)?.is_some() {}
            Ok(())
        },
    )?;

    // --- Query 3 ---
    run_query_benchmark(
        "Q3: g.V().hasId(id).both(label).values('weight', 'timestamp').count()",
        &lines,
        &graph_store,
        parallelism,
        |lg, src, _dst| {
            let mut t = graphTraversalSource();
            t.V(&[]).hasId(&[src]).both(&[EDGE_LABEL]).values(&[SmolStr::new("name"), SmolStr::new("age")]).count();
            let p = t.build()?;
            while p.next(lg)?.is_some() {}
            Ok(())
        },
    )?;

    // --- Query 4 ---
    run_query_benchmark(
        "Q4: g.V(id).out(label).out(label).count()",
        &lines,
        &graph_store,
        parallelism,
        |lg, src, _dst| {
            let mut t = graphTraversalSource();
            t.V(&[src]).out(&[EDGE_LABEL]).out(&[EDGE_LABEL]).count();
            let p = t.build()?;
            while p.next(lg)?.is_some() {}
            Ok(())
        },
    )?;

    Ok(())
}

fn run_query_benchmark<F>(
    name: &str,
    lines: &Arc<Vec<String>>,
    graph_store: &Arc<RocksStorage>,
    parallelism: usize,
    query_fn: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: Fn(&mut LogicalGraph<RocksStorage>, i64, i64) -> Result<(), StoreError> + Send + Sync + 'static,
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
        let store = Arc::clone(graph_store);
        let h_tx = hist_tx.clone();
        let query_fn = Arc::clone(&query_fn);

        let handle = std::thread::spawn(move || {
            let mut local_hist = Histogram::<u64>::new(3).unwrap();
            let start_index = i * chunk_size;
            let end_index = (start_index + chunk_size).min(line_count);

            for line in &lines_chunk[start_index..end_index] {
                let parts: Vec<i64> = line.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                if parts.len() != 2 {
                    continue;
                }
                let (src, dst) = (parts[0], parts[1]);

                let mut lg = LogicalGraph::new(store.begin());
                let op_start = Instant::now();
                if let Err(e) = query_fn(&mut lg, src, dst) {
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
