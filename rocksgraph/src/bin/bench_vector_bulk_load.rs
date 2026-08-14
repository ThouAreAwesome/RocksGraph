// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bulk-load benchmark for Vector Index overhead: loads an edge-list file into a new RocksGraph database
//! via `BulkLoader`, with a synthetic L2-normalized vector property added to each vertex.
//!
//! Usage:
//! ```text
//! bench_vector_bulk_load --data-dir <path> --file-path <path> --dim <N> [--quantization f32|f16]
//! ```

#[path = "vector_bench_common/mod.rs"]
mod vector_bench_common;

use rocksgraph::{
    AnnAlgorithm, BulkEdge, BulkVertex, DistanceMetric, Graph, HnswConfig, Primitive, Quantization, StoreError,
    VectorEntityType, VectorIndexConfig,
};
use vector_bench_common::random_normal_vector;

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::{env, path::PathBuf, time::Instant};

const VERTEX_LABEL: &str = "Person";
const EDGE_LABEL: &str = "Knows";
const VECTOR_KEY: &str = "embedding";

struct EdgeListSource {
    path: PathBuf,
    vertex_label: String,
    edge_label: String,
    comment_char: char,
}

impl EdgeListSource {
    fn open(self) -> Result<(Vec<BulkVertex>, EdgeListIter), Box<dyn std::error::Error>> {
        let mut ids = BTreeSet::new();
        let file = File::open(&self.path)?;
        for line in BufReader::new(file).lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with(self.comment_char) {
                continue;
            }
            let mut parts = trimmed.split_whitespace();
            if let (Some(s), Some(d)) = (parts.next(), parts.next()) {
                if let (Ok(src), Ok(dst)) = (s.parse::<i64>(), d.parse::<i64>()) {
                    ids.insert(src);
                    ids.insert(dst);
                } else {
                    return Err(format!("failed to parse vertex IDs on line: {trimmed}").into());
                }
            }
        }

        let vertices: Vec<BulkVertex> = ids
            .into_iter()
            .map(|id| BulkVertex { id, label: self.vertex_label.clone(), props: HashMap::new() })
            .collect();

        let file = File::open(&self.path)?;
        let edge_iter =
            EdgeListIter { reader: BufReader::new(file), edge_label: self.edge_label, comment_char: self.comment_char };

        Ok((vertices, edge_iter))
    }
}

struct EdgeListIter {
    reader: BufReader<File>,
    edge_label: String,
    comment_char: char,
}

impl Iterator for EdgeListIter {
    type Item = Result<BulkEdge, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut line = String::new();
        loop {
            line.clear();
            match self.reader.read_line(&mut line) {
                Ok(0) => return None,
                Err(e) => return Some(Err(StoreError::Io(e))),
                Ok(_) => {}
            }
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with(self.comment_char) {
                continue;
            }
            let mut parts = trimmed.split_whitespace();
            if let (Some(s), Some(d)) = (parts.next(), parts.next()) {
                match (s.parse::<i64>(), d.parse::<i64>()) {
                    (Ok(src), Ok(dst)) => {
                        return Some(Ok(BulkEdge {
                            src,
                            dst,
                            label: self.edge_label.clone(),
                            props: HashMap::new(),
                            rank: None,
                        }));
                    }
                    _ => {
                        return Some(Err(StoreError::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("failed to parse vertex IDs on line: {trimmed}"),
                        ))));
                    }
                }
            } else {
                return Some(Err(StoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("malformed edge line (expected 'src dst'): {trimmed}"),
                ))));
            }
        }
    }
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

    let dim: usize = args
        .iter()
        .position(|a| a == "--dim")
        .and_then(|p| args.get(p + 1))
        .and_then(|s| s.parse().ok())
        .expect("--dim <N> is required");

    let quantization_str = args.iter().position(|a| a == "--quantization").and_then(|p| args.get(p + 1));

    let quantization = match quantization_str.map(|s| s.as_str()) {
        Some("f16") => Quantization::F16,
        Some("f32") | None => Quantization::F32,
        Some(other) => panic!("Unknown quantization: {}", other),
    };

    if data_dir.exists() {
        std::fs::remove_dir_all(&data_dir)?;
    }

    let source = EdgeListSource {
        path: file_path,
        vertex_label: VERTEX_LABEL.to_string(),
        edge_label: EDGE_LABEL.to_string(),
        comment_char: '#',
    };

    let t0 = Instant::now();
    let (raw_vertices, raw_edges) = source.open()?;

    let vertices = raw_vertices.into_iter().map(move |mut v| -> BulkVertex {
        if dim > 0 {
            let vec = random_normal_vector(dim);
            v.props.insert(VECTOR_KEY.to_string(), Primitive::FloatVector(vec));
        }
        v
    });

    let edges = raw_edges;
    let graph = Graph::open(&data_dir)?;

    if dim > 0 {
        let mut schema = graph.open_schema();
        schema.add_vector_index(
            VectorIndexConfig::new(
                VECTOR_KEY,
                VectorEntityType::Vertex,
                dim,
                DistanceMetric::Cosine,
                AnnAlgorithm::Hnsw(HnswConfig::default()),
            )
            .with_quantization(quantization),
        );
        schema.commit()?;
    }

    let mut loader = graph.open_bulk_loader()?;
    loader.load_vertices(vertices)?;
    loader.load_edges(edges)?;
    let stats = loader.commit()?;
    let elapsed = t0.elapsed();

    println!("=== Vector Bulk Load Complete ===");
    println!("Vertices:    {}", format_count(stats.vertices_written));
    println!("Edges:       {}", format_count(stats.edges_written));
    println!("SST files:   {}", stats.sst_files);

    // `total_secs` uses the same clock scope (from before `source.open()`, through
    // graph-open/schema-declare, to loader commit) as `bench_write.rs`'s own
    // `elapsed`-based throughput — required for the two to be directly
    // comparable, per the design doc's goal. `ingest_secs` is derived by
    // subtracting `build_secs` from that same total, so "Ingest Only" + "Index
    // Build" always reconciles exactly with "Total Elapsed" as printed, instead
    // of mixing in the loader's own internal `stats.duration_secs` (a smaller,
    // differently-scoped clock that excludes file read and schema setup).
    let total_secs = elapsed.as_secs_f64();
    let build_secs = stats.index_build_duration_secs.unwrap_or(0.0);
    let ingest_secs = total_secs - build_secs;

    println!("Total Elapsed:  {total_secs:.2}s");
    println!("- Ingest Only:  {ingest_secs:.2}s");
    println!("- Index Build:  {build_secs:.2}s");

    // Denominator is `ingest_secs`, not `total_secs`: `bench_write.rs` never has
    // an index-build phase at all, so its published throughput is always
    // equivalent to what we call `ingest_secs` here — using `total_secs` would
    // silently penalize vector-enabled runs for a phase the baseline never pays.
    let throughput =
        if ingest_secs > 0.0 { (stats.edges_written as f64 / ingest_secs) as u64 } else { stats.edges_written };
    println!(
        "Throughput:  {} edges/s (ingest only — directly comparable to bench_write's published numbers)",
        format_count(throughput)
    );

    Ok(())
}
