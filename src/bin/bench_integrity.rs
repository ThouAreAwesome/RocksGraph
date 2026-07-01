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

//! Data integrity checks for a RocksGraph database.
//!
//! Run with: `cargo run --bin bench_integrity -- --data-dir <path>`
//!
//! Currently checks:
//! - **Degree integrity**: verifies that the O(1) `vertex_degree` CF counters agree
//!   with a full adjacency scan across every vertex.  The optimized query uses
//!   `DegreeStep` (reads the CF directly); the ground-truth query does a real
//!   adjacency scan using all known edge labels (bypassing the degree optimizer).

use rocksgraph::{Graph, TraversalBuilder, Value, __};

use std::{collections::HashMap, env};

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

/// Verify that the O(1) degree counters in the `vertex_degree` CF agree with
/// a full adjacency scan across every vertex in the graph.
///
/// Two queries are compared for both out-degree and in-degree using `path()` to
/// carry the vertex identity alongside each degree value:
///
/// **Optimized** (labels empty → `degree_pushdown` A+B+C cascade fires;
/// `DegreeStep` maps each vertex O(1) to its CF degree):
/// ```text
/// g.V([]).local(__.out([]).count()).path()   →  [Vertex(id), Int64(degree)]
/// ```
///
/// **Ground truth** (non-empty labels block Rule A so the degree optimizer
/// cannot fire; `LocalStep` re-threads the outer Vertex as parent for each
/// `CountStep` result, giving the same path structure):
/// ```text
/// g.V([]).local(__.out([all_labels]).count()).path()   →  [Vertex(id), Int64(count)]
/// ```
///
/// Results are collected into `HashMap<vertex_id, degree>` and compared by
/// vertex identity — unaffected by V() scan order.
fn verify_degree_integrity(graph: &Graph) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n--- Verifying degree integrity (vertex_degree CF vs adjacency scan) ---");

    // Collect all edge labels so the ground-truth query cannot be rewritten to
    // DegreeStep (non-empty labels fail Rule A's `labels.is_empty()` guard).
    let edge_labels: Vec<String> = {
        let mut snap = graph.read();
        snap.g()
            .E([])
            .label()
            .dedup()
            .to_list()?
            .into_iter()
            .filter_map(|v| if let Value::String(s) = v { Some(s) } else { None })
            .collect()
    };

    if edge_labels.is_empty() {
        println!("  No edge labels found — skipping degree integrity check.");
        return Ok(());
    }

    // Single snapshot for both queries — consistent view of committed state.
    let mut snap = graph.read();

    // ── Out-degree ──────────────────────────────────────────────────────────

    let out_opt = snap.g().V([]).local(__().out([]).count()).path().to_list()?;
    let out_scan = snap.g().V([]).local(__().out(edge_labels.iter().map(|s| s.as_str())).count()).path().to_list()?;
    check_degree("out-degree", out_opt, out_scan)?;

    // ── In-degree ───────────────────────────────────────────────────────────

    let in_opt = snap.g().V([]).local(__().r#in([]).count()).path().to_list()?;
    let in_scan = snap.g().V([]).local(__().r#in(edge_labels.iter().map(|s| s.as_str())).count()).path().to_list()?;
    check_degree("in-degree", in_opt, in_scan)?;

    Ok(())
}

/// Extract `HashMap<vertex_id, degree>` from path results.
/// Each path must be `[Value::Vertex(v), Value::Int64(degree)]`.
fn paths_to_degree_map(paths: Vec<Value>) -> HashMap<i64, i64> {
    let mut map = HashMap::new();
    for p in paths {
        if let Value::Path(path) = p {
            if let (Some(Value::Vertex(v)), Some(Value::Int64(degree))) = (path.objects.first(), path.objects.get(1)) {
                map.insert(v.id, *degree);
            }
        }
    }
    map
}

/// Compare per-vertex degrees extracted from two `path()` result sets.
/// Comparison is by vertex ID (HashMap key), not by position.
fn check_degree(name: &str, opt_paths: Vec<Value>, scan_paths: Vec<Value>) -> Result<(), Box<dyn std::error::Error>> {
    let opt_map = paths_to_degree_map(opt_paths);
    let scan_map = paths_to_degree_map(scan_paths);

    if opt_map.is_empty() || scan_map.is_empty() {
        return Err(format!(
            "{name}: no vertex/degree pairs found in paths — DegreeStep or LocalStep path tracking may be broken"
        )
        .into());
    }
    if opt_map.len() != scan_map.len() {
        return Err(
            format!("{name}: vertex count mismatch — optimized={}, scan={}", opt_map.len(), scan_map.len()).into()
        );
    }

    let mut mismatches: Vec<(i64, i64, i64)> = opt_map
        .iter()
        .filter_map(|(&vid, &opt_deg)| {
            let scan_deg = *scan_map.get(&vid)?;
            if opt_deg != scan_deg {
                Some((vid, opt_deg, scan_deg))
            } else {
                None
            }
        })
        .collect();

    if mismatches.is_empty() {
        println!("  {name}: OK ({} vertices, all degrees match)", opt_map.len());
    } else {
        mismatches.sort_by_key(|&(vid, _, _)| vid);
        for (vid, opt_deg, scan_deg) in &mismatches {
            println!("  {name}: MISMATCH vertex={vid} stored(CF)={opt_deg} scanned={scan_deg}");
        }
        return Err(format!("{name}: {} mismatch(es) detected", mismatches.len()).into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_bench_integrity_clean_graph() {
        let dir = tempdir().unwrap();
        let graph = Graph::open(dir.path()).unwrap();
        let mut tx = graph.begin();
        tx.g().addV("person").property("id", 1i64).next().unwrap();
        tx.g().addV("person").property("id", 2i64).next().unwrap();
        tx.g().addV("person").property("id", 3i64).next().unwrap();
        tx.g().addE("knows").from(1).to(2).next().unwrap();
        tx.g().addE("knows").from(2).to(3).next().unwrap();
        tx.g().addE("knows").from(3).to(1).next().unwrap();
        tx.commit().unwrap();

        // All degrees should match — no mismatches expected.
        assert!(verify_degree_integrity(&graph).is_ok());
    }

    #[test]
    fn test_bench_integrity_self_loop() {
        let dir = tempdir().unwrap();
        let graph = Graph::open(dir.path()).unwrap();
        let mut tx = graph.begin();
        tx.g().addV("person").property("id", 1i64).next().unwrap();
        tx.g().addV("person").property("id", 2i64).next().unwrap();
        tx.g().addE("knows").from(1).to(1).next().unwrap(); // self-loop
        tx.g().addE("knows").from(1).to(2).next().unwrap();
        tx.commit().unwrap();

        // Self-loop degree should be counted correctly (was a known bug).
        assert!(verify_degree_integrity(&graph).is_ok());
    }
}
