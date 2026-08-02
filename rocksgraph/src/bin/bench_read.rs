// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use hdrhistogram::Histogram;
use rand::seq::SliceRandom;
use rocksgraph::{ne, Graph, ReadSession, StoreError, TraversalBuilder, Value, __};

use std::{
    env,
    fs::File,
    io::{BufRead, BufReader},
    sync::{mpsc, Arc},
    time::Instant,
};

const EDGE_LABEL: &str = "Knows";
const VERTEX_LABEL: &str = "Person";
const NAME_KEY: &str = "name";
const AGE_KEY: &str = "age";
const WEIGHT_KEY: &str = "weight";
const TIMESTAMP_KEY: &str = "timestamp";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    run_with_args(args)
}

fn run_with_args(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
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

    // --queries N  — randomly sample N edge pairs as query parameters (default 10_000).
    // Pass --queries 0 to use the entire file (full benchmark, slow on large datasets).
    let query_count: usize = args
        .iter()
        .position(|arg| arg == "--queries")
        .and_then(|pos| args.get(pos + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(10_000);

    let graph = Graph::open(data_dir)?;

    // --explain: print the physical plan for every query using IDs 1→2, then exit.
    if args.iter().any(|a| a == "--explain") {
        return explain_all(&graph);
    }

    let file = File::open(file_dir)?;
    let reader = BufReader::new(file);
    let lines: Arc<Vec<String>> = Arc::new({
        let mut all: Vec<String> = reader.lines().collect::<Result<_, _>>()?;
        if query_count > 0 && query_count < all.len() {
            // Reservoir-sample for statistical representativeness.
            // Avoids bias from hub vertices that cluster at the start of sorted files.
            all.shuffle(&mut rand::thread_rng());
            all.truncate(query_count);
        }
        println!(
            "Query parameters: {} edge pairs{}",
            all.len(),
            if query_count == 0 { " (full file)" } else { " (random sample)" }
        );
        all
    });

    run_query_benchmark(
        "Q1: g.V().hasId(id).values('name', 'age').count()",
        &lines,
        &graph,
        parallelism,
        |snap, src, _dst| {
            let Value::Int64(ct) = snap.g().V([]).hasId([src]).values([NAME_KEY, AGE_KEY]).count().next()?.unwrap()
            else {
                unreachable!("unexpected gremlin result type")
            };
            assert_eq!(ct, 2);
            Ok(ct)
        },
    )?;

    run_query_benchmark(
        "Q2: g.V().hasId(id).outE(label).values('weight', 'timestamp').count()",
        &lines,
        &graph,
        parallelism,
        |snap, src, _dst| {
            let Value::Int64(ct) = snap
                .g()
                .V([])
                .hasId([src])
                .outE([EDGE_LABEL])
                .values([WEIGHT_KEY, TIMESTAMP_KEY])
                .count()
                .next()?
                .unwrap()
            else {
                unreachable!("unexpected gremlin result type")
            };
            assert!(ct >= 2);
            Ok(ct)
        },
    )?;

    run_query_benchmark(
        "Q3: g.V().hasId(id).outE(label).where(otherV().hasId(dst)).values('weight', 'timestamp').count()",
        &lines,
        &graph,
        parallelism,
        |snap, src, dst| {
            let Value::Int64(ct) = snap
                .g()
                .V([])
                .hasId([src])
                .outE([EDGE_LABEL])
                .r#where(__().otherV().hasId([dst]))
                .values([WEIGHT_KEY, TIMESTAMP_KEY])
                .count()
                .next()?
                .unwrap()
            else {
                unreachable!("unexpected gremlin result type")
            };
            assert!(ct >= 2);
            Ok(ct)
        },
    )?;

    run_query_benchmark(
        "Q4: g.V().hasId(id).outE(label).values('weight', 'timestamp').limit(5).count()",
        &lines,
        &graph,
        parallelism,
        |snap, src, _dst| {
            let Value::Int64(ct) = snap
                .g()
                .V([])
                .hasId([src])
                .outE([EDGE_LABEL])
                .values([WEIGHT_KEY, TIMESTAMP_KEY])
                .limit(5)
                .count()
                .next()?
                .unwrap()
            else {
                unreachable!("unexpected gremlin result type")
            };
            assert!(ct >= 1);
            Ok(ct)
        },
    )?;

    run_query_benchmark(
        "Q5: g.V().hasId(id).out(label).values('name', 'age').count()",
        &lines,
        &graph,
        parallelism,
        |snap, src, _dst| {
            let Value::Int64(ct) =
                snap.g().V([]).hasId([src]).out([EDGE_LABEL]).values([NAME_KEY, AGE_KEY]).count().next()?.unwrap()
            else {
                unreachable!("unexpected gremlin result type")
            };
            assert!(ct >= 0);
            Ok(ct)
        },
    )?;

    run_query_benchmark(
        "Q6: g.V(id).out(label).hasLabel(v_label).dedup().out(label).hasLabel(v_label).dedup().hasId(not(id)).count()",
        &lines,
        &graph,
        parallelism,
        |snap, src, _dst| {
            let Value::Int64(ct) = snap
                .g()
                .V([src])
                .out([EDGE_LABEL])
                .hasLabel([VERTEX_LABEL])
                .dedup()
                .out([EDGE_LABEL])
                .hasLabel([VERTEX_LABEL])
                .dedup()
                .hasId(ne(src))
                .count()
                .next()?
                .unwrap()
            else {
                unreachable!("unexpected gremlin result type")
            };
            assert!(ct >= 0);
            Ok(ct)
        },
    )?;

    run_query_benchmark(
        "Q7: g.V(id).repeat(out(label).hasLabel(v_label).dedup()).times(2).hasId(not(id)).count()",
        &lines,
        &graph,
        parallelism,
        |snap, src, _dst| {
            let Value::Int64(ct) = snap
                .g()
                .V([src])
                .repeat(__().out([EDGE_LABEL]).hasLabel([VERTEX_LABEL]).dedup())
                .times(2)
                .hasId(ne(src))
                .count()
                .next()?
                .unwrap()
            else {
                unreachable!("unexpected gremlin result type")
            };
            assert!(ct >= 0);
            Ok(ct)
        },
    )?;

    // A full scan already covers the whole dataset in one pass, so a single run
    // is a stable measurement — no need to repeat it.
    run_query_benchmark(
        "Q8: g.V().count() (Scan total vertices in DB)",
        &Arc::new(vec!["0 0".to_string(); 1]),
        &graph,
        1,
        |snap, _src, _dst| {
            let Value::Int64(ct) = snap.g().V([]).count().next()?.unwrap() else {
                unreachable!("unexpected gremlin result type")
            };
            println!("   [Scan Result] Total vertices: {}", ct);
            Ok(ct)
        },
    )?;

    run_query_benchmark(
        "Q9: g.E([]).count() (Scan total edges in DB)",
        &Arc::new(vec!["0 0".to_string(); 1]),
        &graph,
        1,
        |snap, _src, _dst| {
            let Value::Int64(ct) = snap.g().E([]).count().next()?.unwrap() else {
                unreachable!("unexpected gremlin result type")
            };
            println!("   [Scan Result] Total edges: {}", ct);
            Ok(ct)
        },
    )?;

    #[cfg(feature = "rocksdb-stats")]
    if let Some(stats) = graph.statistics() {
        println!("\n--- RocksDB Statistics ---\n{}", stats);
    }

    Ok(())
}

fn explain_all(graph: &Graph) -> Result<(), Box<dyn std::error::Error>> {
    let mut snap = graph.read();
    let src: i64 = 1;
    let dst: i64 = 2;

    type QueryFn = Box<dyn Fn(&mut rocksgraph::ReadSession) -> Result<String, rocksgraph::StoreError>>;
    let queries: &[(&str, QueryFn)] = &[
        (
            "Q1: g.V([]).hasId(src).values('name','age').count()",
            Box::new(move |s| s.g().V([]).hasId([src]).values([NAME_KEY, AGE_KEY]).count().explain()),
        ),
        (
            "Q2: g.V([]).hasId(src).outE(label).values('weight','timestamp').count()",
            Box::new(move |s| {
                s.g().V([]).hasId([src]).outE([EDGE_LABEL]).values([WEIGHT_KEY, TIMESTAMP_KEY]).count().explain()
            }),
        ),
        (
            "Q3: g.V([]).hasId(src).outE(label).where(otherV().hasId(dst)).values('weight','timestamp').count()",
            Box::new(move |s| {
                s.g()
                    .V([])
                    .hasId([src])
                    .outE([EDGE_LABEL])
                    .r#where(__().otherV().hasId([dst]))
                    .values([WEIGHT_KEY, TIMESTAMP_KEY])
                    .count()
                    .explain()
            }),
        ),
        (
            "Q4: g.V([]).hasId(src).outE(label).values('weight','timestamp').limit(5).count()",
            Box::new(move |s| {
                s.g()
                    .V([])
                    .hasId([src])
                    .outE([EDGE_LABEL])
                    .values([WEIGHT_KEY, TIMESTAMP_KEY])
                    .limit(5)
                    .count()
                    .explain()
            }),
        ),
        (
            "Q5: g.V([]).hasId(src).out(label).values('name','age').count()",
            Box::new(move |s| {
                s.g().V([]).hasId([src]).out([EDGE_LABEL]).values([NAME_KEY, AGE_KEY]).count().explain()
            }),
        ),
        (
            "Q6: g.V(src).out(label).hasLabel(v_label).dedup().out(label).hasLabel(v_label).dedup().hasId(ne(src)).count()",
            Box::new(move |s| {
                s.g()
                    .V([src])
                    .out([EDGE_LABEL])
                    .hasLabel([VERTEX_LABEL])
                    .dedup()
                    .out([EDGE_LABEL])
                    .hasLabel([VERTEX_LABEL])
                    .dedup()
                    .hasId(ne(src))
                    .count()
                    .explain()
            }),
        ),
        (
            "Q7: g.V(src).repeat(out(label).hasLabel(v_label).dedup()).times(2).hasId(ne(src)).count()",
            Box::new(move |s| {
                s.g()
                    .V([src])
                    .repeat(__().out([EDGE_LABEL]).hasLabel([VERTEX_LABEL]).dedup())
                    .times(2)
                    .hasId(ne(src))
                    .count()
                    .explain()
            }),
        ),
        ("Q8: g.V().count()", Box::new(move |s| s.g().V([]).count().explain())),
        ("Q9: g.E([]).count()", Box::new(move |s| s.g().E([]).count().explain())),
    ];

    for (name, build) in queries {
        println!("\n=== {} ===", name);
        match build(&mut snap) {
            Ok(plan) => println!("{}", plan),
            Err(e) => println!("  (plan error: {})", e),
        }
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
    F: Fn(&mut ReadSession, i64, i64) -> Result<i64, StoreError> + Send + Sync + 'static,
{
    println!("\n--- Running Benchmark for: {} ---", name);
    let start = Instant::now();
    let query_fn = Arc::new(query_fn);
    let line_count = lines.len();
    let chunk_size = (line_count + parallelism - 1).div_ceil(parallelism);

    let (hist_tx, hist_rx) = mpsc::channel::<(Histogram<u64>, Histogram<u64>)>();

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
            let mut local_count_hist = Histogram::<u64>::new(3).unwrap();
            let start_index = i * chunk_size;
            let end_index = (start_index + chunk_size).min(line_count);
            let worker_total = end_index - start_index;
            // Worker 0 prints coarse progress for the whole run; chunks are
            // near-equal in size, so its fraction approximates overall progress.
            let progress_step = (worker_total / 10).max(1);

            for (idx, line) in lines_chunk[start_index..end_index].iter().enumerate() {
                let parts: Vec<i64> = line.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                if parts.len() != 2 {
                    continue;
                }
                let (src, dst) = (parts[0], parts[1]);

                snap.clear_caches();
                let op_start = Instant::now();
                match query_fn(&mut snap, src, dst) {
                    Ok(ct) => local_count_hist.record(ct as u64).unwrap(),
                    Err(e) => eprintln!("Query failed: {}", e),
                }
                local_hist.record(op_start.elapsed().as_nanos() as u64).unwrap();

                if i == 0 && worker_total > 1_000 && (idx + 1) % progress_step == 0 {
                    let elapsed = start.elapsed().as_secs_f64();
                    let pct = (idx + 1) * 100 / worker_total;
                    eprintln!("  Progress: ~{pct}% | {elapsed:.0}s elapsed");
                }
            }
            h_tx.send((local_hist, local_count_hist)).unwrap();
        });
        worker_handles.push(handle);
    }

    drop(hist_tx);
    for h in worker_handles {
        h.join().unwrap();
    }

    let mut final_hist = Histogram::<u64>::new(3).unwrap();
    let mut final_count_hist = Histogram::<u64>::new(3).unwrap();
    for (h, c) in hist_rx {
        final_hist.add(h).unwrap();
        final_count_hist.add(c).unwrap();
    }

    let elapsed_secs = start.elapsed().as_secs_f64();
    let ops = line_count as f64 / elapsed_secs;
    println!("Ops: {:.2}/s ({} queries in {:.2}s)", ops, line_count, elapsed_secs);
    println!(
        "Latency (μs) — mean: {}, p50: {}, p90: {}, p95: {}, p99: {}, max: {}",
        final_hist.mean() / 1000.0,
        final_hist.value_at_quantile(0.5) as f64 / 1000.0,
        final_hist.value_at_quantile(0.9) as f64 / 1000.0,
        final_hist.value_at_quantile(0.95) as f64 / 1000.0,
        final_hist.value_at_quantile(0.99) as f64 / 1000.0,
        final_hist.max() as f64 / 1000.0
    );
    println!(
        "Result count — mean: {:.1}, p50: {}, p90: {}, p95: {}, p99: {}, max: {}",
        final_count_hist.mean(),
        final_count_hist.value_at_quantile(0.5),
        final_count_hist.value_at_quantile(0.9),
        final_count_hist.value_at_quantile(0.95),
        final_count_hist.value_at_quantile(0.99),
        final_count_hist.max()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocksgraph::schema::{GraphOptions, SchemaMode};
    use tempfile::tempdir;

    #[test]
    fn test_bench_read() {
        let dir = tempdir().unwrap();
        {
            let graph =
                Graph::open_with_options(dir.path(), GraphOptions { mode: SchemaMode::Strict, ..Default::default() })
                    .unwrap();
            let mut snap = graph.begin();
            {
                use rocksgraph::schema::DataType;
                let mut mgmt = graph.open_schema();
                mgmt.add_vertex_label(VERTEX_LABEL)
                    .add_edge_label(EDGE_LABEL)
                    .add_property_key("name", DataType::String)
                    .add_property_key("age", DataType::Int64)
                    .add_property_key("weight", DataType::Float64)
                    .add_property_key("timestamp", DataType::Int64);
                mgmt.commit().unwrap();
            }
            snap.g()
                .addV(VERTEX_LABEL)
                .property("id", 1i64)
                .property("name", "alice")
                .property("age", 30i64)
                .next()
                .unwrap();
            snap.g()
                .addV(VERTEX_LABEL)
                .property("id", 2i64)
                .property("name", "bob")
                .property("age", 25i64)
                .next()
                .unwrap();
            snap.g()
                .addE(EDGE_LABEL)
                .from(1)
                .to(2)
                .property("weight", 0.5f64)
                .property("timestamp", 100i64)
                .next()
                .unwrap();
            snap.g()
                .addE(EDGE_LABEL)
                .from(2)
                .to(1)
                .property("weight", 0.6f64)
                .property("timestamp", 200i64)
                .next()
                .unwrap();
            snap.commit().unwrap();
        }

        let file_dir = tempdir().unwrap();
        let file_path = file_dir.path().join("graph.txt");
        std::fs::write(&file_path, "1 2\n").unwrap();

        let args = vec![
            "bench_read".to_string(),
            "--data-dir".to_string(),
            dir.path().to_str().unwrap().to_string(),
            "--file-path".to_string(),
            file_path.to_str().unwrap().to_string(),
            "--parallelism".to_string(),
            "1".to_string(),
        ];
        assert!(run_with_args(args).is_ok());
    }
}
