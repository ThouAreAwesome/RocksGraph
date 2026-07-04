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

//! Bulk-load benchmark: loads an edge-list file into a new RocksGraph database
//! via `SstBulkLoader`, then reports throughput.
//!
//! Usage:
//! ```text
//! bench_write --data-dir <path> --file-path <path>
//!             [--max-memory <bytes>]  (default: 512 MiB)
//!             [--max-sst    <bytes>]  (default: 58 MiB)
//! ```

use rocksgraph::{
    schema::{DataType, GraphOptions},
    BulkSchema, EdgeListSource, Primitive, RocksOptions, SstBulkLoader,
};

use rand::Rng;
use std::{env, path::PathBuf, time::Instant};

const VERTEX_LABEL: &str = "Person";
const EDGE_LABEL: &str = "Knows";
const NAME_KEY: &str = "name";
const AGE_KEY: &str = "age";
const WEIGHT_KEY: &str = "weight";
const TIMESTAMP_KEY: &str = "timestamp";

fn generate_random_string(len: usize) -> String {
    rand::thread_rng().sample_iter(rand::distributions::Alphanumeric).take(len).map(char::from).collect()
}

fn format_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('_');
        }
        out.push(c);
    }
    out
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

    let max_memory: Option<usize> =
        args.iter().position(|a| a == "--max-memory").and_then(|p| args.get(p + 1)).and_then(|s| s.parse().ok());

    let max_sst: Option<usize> =
        args.iter().position(|a| a == "--max-sst").and_then(|p| args.get(p + 1)).and_then(|s| s.parse().ok());

    if data_dir.exists() {
        std::fs::remove_dir_all(&data_dir)?;
    }

    // Schema matching bench_read expectations so round-trip verification works.
    let schema = BulkSchema {
        vertex_labels: vec![VERTEX_LABEL.to_string()],
        edge_labels: vec![EDGE_LABEL.to_string()],
        prop_keys: vec![
            (NAME_KEY.to_string(), DataType::String),
            (AGE_KEY.to_string(), DataType::Int64),
            (WEIGHT_KEY.to_string(), DataType::Float64),
            (TIMESTAMP_KEY.to_string(), DataType::Int64),
        ],
    };

    let source = EdgeListSource {
        path: file_path,
        vertex_label: VERTEX_LABEL.to_string(),
        edge_label: EDGE_LABEL.to_string(),
        comment_char: '#',
    };

    // Timing begins here: includes file parsing and property generation.
    let t0 = Instant::now();
    let (raw_vertices, raw_edges) = source.open().map_err(|e| format!("failed to open source: {e}"))?;

    // Synthetic properties matching the schema above.
    let mut rng_v = rand::thread_rng();
    let vertices = raw_vertices.into_iter().map(move |mut v| {
        v.props.insert(NAME_KEY.to_string(), Primitive::String(generate_random_string(10).into()));
        v.props.insert(AGE_KEY.to_string(), Primitive::Int64(rng_v.gen_range(18..100)));
        v
    });
    let mut rng_e = rand::thread_rng();
    let edges = raw_edges.into_iter().map(move |mut e| {
        e.props.insert(WEIGHT_KEY.to_string(), Primitive::Float64(rng_e.gen_range(0.1..10.0)));
        e.props.insert(TIMESTAMP_KEY.to_string(), Primitive::Int64(rng_e.gen_range(0..1_000_000)));
        e
    });

    let work_dir = data_dir.join("_bulk_work");
    let mut loader = SstBulkLoader::new(&data_dir, &work_dir);
    if let Some(m) = max_memory {
        loader = loader.with_max_memory(m);
    }
    if let Some(s) = max_sst {
        loader = loader.with_max_sst_size(s);
    }

    let stats = loader.load_initial(schema, vertices, edges, GraphOptions::default(), &RocksOptions::default())?;
    let elapsed = t0.elapsed();

    println!("=== Bulk SST Load Complete ===");
    println!("Vertices:    {}", format_count(stats.vertices_written));
    println!("Edges:       {}", format_count(stats.edges_written));
    println!("SST files:   {}", stats.sst_files);
    println!("Elapsed:     {:.2}s", elapsed.as_secs_f64());
    let throughput = if elapsed.as_secs_f64() > 0.0 {
        (stats.edges_written as f64 / elapsed.as_secs_f64()) as u64
    } else {
        stats.edges_written
    };
    println!("Throughput:  {} edges/s", format_count(throughput));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_bench_write_bulk_sst() {
        let dir = tempdir().unwrap();
        let file_dir = tempdir().unwrap();
        let file_path = file_dir.path().join("graph.txt");
        std::fs::write(&file_path, "1 2\n2 3\n3 1\n").unwrap();

        let args = vec![
            "bench_write".to_string(),
            "--data-dir".to_string(),
            dir.path().join("db").to_str().unwrap().to_string(),
            "--file-path".to_string(),
            file_path.to_str().unwrap().to_string(),
        ];
        assert!(run_with_args(args).is_ok());
    }
}
