// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Data integrity checks for a RocksGraph database.
//!
//! Run with: `cargo run --bin bench_integrity -- --data-dir <path>`
//!
//! Currently checks:
//! - **Degree integrity**: verifies that the O(1) `vertex_degree` CF counters agree
//!   with a full adjacency scan across every vertex.
//!
//! ## Streaming comparison
//!
//! Both queries use `path()` to carry `[Vertex(id), Int64(degree)]` for each
//! vertex, enabling mismatch reports to include the vertex ID.  Two independent
//! `ReadSession` snapshots are compared vertex-by-vertex via `zip()` — no
//! `HashMap` or bulk `Vec` is built; peak extra memory is O(1) per vertex pair.
//! The vertex scan order is deterministic (sorted by vertex key) so positional
//! comparison is correct.
//!
//! The previous `PathStep` was eager (consumed all upstream traversers in one
//! blocking call before yielding anything).  It has been fixed to stream results
//! in chunks of `PIPELINE_PRODUCE_SIZE`, so the first result is returned
//! immediately and progress is visible from the first 0.5 s tick.

use rocksgraph::{Graph, StoreError, TraversalBuilder, Value, __};

use std::{
    env,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[allow(dead_code)]
const VERTEX_LABEL: &str = "Person";
#[allow(dead_code)]
const EDGE_LABEL: &str = "Knows";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let data_dir = args
        .iter()
        .position(|arg| arg == "--data-dir")
        .and_then(|pos| args.get(pos + 1))
        .expect("Usage: bench_integrity --data-dir <path>");

    let graph = Graph::open(data_dir)?;
    verify_degree_integrity(&graph)?;
    Ok(())
}

fn verify_degree_integrity(graph: &Graph) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n--- Verifying degree integrity (vertex_degree CF vs adjacency scan) ---");

    let edge_labels: Vec<smol_str::SmolStr> = graph.edge_label_names();
    if edge_labels.is_empty() {
        println!("  No edge labels found — skipping degree integrity check.");
        return Ok(());
    }

    let mut snap = graph.read();
    let v_count = match snap.g().V([]).count().next()? {
        Some(Value::Int64(n)) => n as usize,
        _ => 0,
    };
    println!("  Database: {v_count} vertices, {} edge label(s)", edge_labels.len());

    // ── Out-degree ──────────────────────────────────────────────────────────
    // path() carries (Vertex, Int64(degree)) so mismatches can report vertex ID.
    // The root cause of previous slowness was an eager PathStep; that is now fixed
    // to stream PIPELINE_PRODUCE_SIZE results at a time.
    {
        let mut snap_opt = graph.read();
        let mut snap_scan = graph.read();
        let opt = snap_opt.g().V([]).local(__().out([]).count()).path().iter()?;
        let scan = snap_scan.g().V([]).local(__().out(edge_labels.iter().map(|s| s.as_str())).count()).path().iter()?;
        compare_degree_streams("out-degree", v_count, opt, scan)?;
    }

    // ── In-degree ───────────────────────────────────────────────────────────
    {
        let mut snap_opt = graph.read();
        let mut snap_scan = graph.read();
        let opt = snap_opt.g().V([]).local(__().r#in([]).count()).path().iter()?;
        let scan =
            snap_scan.g().V([]).local(__().r#in(edge_labels.iter().map(|s| s.as_str())).count()).path().iter()?;
        compare_degree_streams("in-degree", v_count, opt, scan)?;
    }

    Ok(())
}

/// Stream-compare two degree iterators vertex-by-vertex.
///
/// Both iterators must produce `Value::Path([Vertex(id), Int64(degree)])` in the
/// same vertex-key order (guaranteed since both start with the same `V([])` scan).
/// Mismatches are accumulated and reported; scan order divergence is a hard error.
fn compare_degree_streams(
    name: &str,
    v_count: usize,
    opt_iter: impl Iterator<Item = Result<Value, StoreError>>,
    scan_iter: impl Iterator<Item = Result<Value, StoreError>>,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("  [{name}] stream-comparing {v_count} vertices (optimized ↔ scan)…");

    let t0 = Instant::now();
    let checked = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicBool::new(false));

    let progress_handle = {
        let checked = Arc::clone(&checked);
        let done = Arc::clone(&done);
        let name = name.to_string();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            if done.load(Ordering::Relaxed) {
                break;
            }
            let count = checked.load(Ordering::Relaxed);
            let elapsed = t0.elapsed().as_secs_f64();
            let pct = count * 100 / v_count;

            if count >= 100 {
                let eta = elapsed / count as f64 * (v_count - count) as f64;
                eprintln!("  [{name}] {pct}% ({count}/{v_count}) | {elapsed:.1}s elapsed | ~{eta:.1}s remaining");
            } else if count > 0 {
                eprintln!("  [{name}] {pct}% ({count}/{v_count}) | {elapsed:.1}s elapsed | ~estimating...");
            } else {
                eprintln!("  [{name}] 0% (0/{v_count}) | {elapsed:.1}s elapsed | ~estimating... (warming block cache)");
            }
        })
    };

    let mut mismatch_count = 0usize;
    let mut local_checked = 0usize;

    for (opt_item, scan_item) in opt_iter.zip(scan_iter) {
        let (opt_vid, opt_deg) = extract_path(opt_item?).ok_or("malformed optimized path")?;
        let (scan_vid, scan_deg) = extract_path(scan_item?).ok_or("malformed scan path")?;

        // Both V() scans share the same vertex key order — divergence is a bug.
        if opt_vid != scan_vid {
            done.store(true, Ordering::Relaxed);
            let _ = progress_handle.join();
            return Err(format!(
                "{name}: scan order diverged at position {local_checked} — opt_vid={opt_vid} scan_vid={scan_vid}"
            )
            .into());
        }

        if opt_deg != scan_deg {
            mismatch_count += 1;
            if mismatch_count <= 5 {
                eprintln!("  [{name}] MISMATCH vertex={opt_vid} CF={opt_deg} scan={scan_deg}");
            }
        }

        local_checked += 1;
        checked.store(local_checked, Ordering::Relaxed);
    }

    done.store(true, Ordering::Relaxed);
    let _ = progress_handle.join();

    if local_checked != v_count {
        return Err(format!("{name}: expected {v_count} vertices but compared {local_checked}").into());
    }

    let elapsed = t0.elapsed().as_secs_f64();
    if mismatch_count == 0 {
        println!("  {name}: OK ({local_checked} vertices, all degrees match) | {elapsed:.1}s");
    } else {
        return Err(format!("{name}: {mismatch_count} mismatch(es) detected (first 5 printed above)").into());
    }

    Ok(())
}

/// Extract `(vertex_id, degree)` from `Value::Path([Vertex(id), Int64(degree)])`.
fn extract_path(val: Value) -> Option<(i64, i64)> {
    if let Value::Path(p) = val {
        if let (Some(Value::Vertex(v)), Some(Value::Int64(deg))) = (p.objects.first(), p.objects.get(1)) {
            return Some((v.id, *deg));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocksgraph::schema::{GraphOptions, SchemaMode};
    use tempfile::tempdir;

    #[test]
    fn test_bench_integrity_clean_graph() {
        let dir = tempdir().unwrap();
        let graph =
            Graph::open_with_options(dir.path(), GraphOptions { mode: SchemaMode::Strict, ..Default::default() })
                .unwrap();
        {
            let mut mgmt = graph.open_schema();
            mgmt.add_vertex_label(VERTEX_LABEL).add_edge_label(EDGE_LABEL);
            mgmt.commit().unwrap();
        }
        let mut tx = graph.begin();
        tx.g().addV(VERTEX_LABEL).property("id", 1i64).next().unwrap();
        tx.g().addV(VERTEX_LABEL).property("id", 2i64).next().unwrap();
        tx.g().addV(VERTEX_LABEL).property("id", 3i64).next().unwrap();
        tx.g().addE(EDGE_LABEL).from(1).to(2).next().unwrap();
        tx.g().addE(EDGE_LABEL).from(2).to(3).next().unwrap();
        tx.g().addE(EDGE_LABEL).from(3).to(1).next().unwrap();
        tx.commit().unwrap();
        assert!(verify_degree_integrity(&graph).is_ok());
    }

    #[test]
    fn test_bench_integrity_self_loop() {
        let dir = tempdir().unwrap();
        let graph =
            Graph::open_with_options(dir.path(), GraphOptions { mode: SchemaMode::Strict, ..Default::default() })
                .unwrap();
        {
            let mut mgmt = graph.open_schema();
            mgmt.add_vertex_label(VERTEX_LABEL).add_edge_label(EDGE_LABEL);
            mgmt.commit().unwrap();
        }
        let mut tx = graph.begin();
        tx.g().addV(VERTEX_LABEL).property("id", 1i64).next().unwrap();
        tx.g().addV(VERTEX_LABEL).property("id", 2i64).next().unwrap();
        tx.g().addE(EDGE_LABEL).from(1).to(1).next().unwrap(); // self-loop
        tx.g().addE(EDGE_LABEL).from(1).to(2).next().unwrap();
        tx.commit().unwrap();
        assert!(verify_degree_integrity(&graph).is_ok());
    }
}
