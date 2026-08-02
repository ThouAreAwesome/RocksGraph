// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bulk-load benchmark: loads an edge-list file into a new RocksGraph database
//! via `BulkLoader`, then reports throughput.
//!
//! Usage:
//! ```text
//! bench_write --data-dir <path> --file-path <path>
//!             [--max-memory <bytes>]  (default: 512 MiB)
//!             [--max-sst    <bytes>]  (default: 58 MiB)
//! ```

use rocksgraph::{BulkEdge, BulkVertex, Graph, Primitive, StoreError};

use rand::Rng;
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::{env, path::PathBuf, time::Instant};

const VERTEX_LABEL: &str = "Person";
const EDGE_LABEL: &str = "Knows";
const NAME_KEY: &str = "name";
const AGE_KEY: &str = "age";
const WEIGHT_KEY: &str = "weight";
const TIMESTAMP_KEY: &str = "timestamp";

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

    let source = EdgeListSource {
        path: file_path,
        vertex_label: VERTEX_LABEL.to_string(),
        edge_label: EDGE_LABEL.to_string(),
        comment_char: '#',
    };

    // Timing begins here: includes file parsing and property generation.
    let t0 = Instant::now();
    let (raw_vertices, raw_edges) = source.open()?;

    // Synthetic properties matching bench_read expectations.
    let mut rng_v = rand::thread_rng();
    let vertices = raw_vertices.into_iter().map(move |mut v| -> BulkVertex {
        v.props.insert(NAME_KEY.to_string(), Primitive::String(generate_random_string(10).into()));
        v.props.insert(AGE_KEY.to_string(), Primitive::Int64(rng_v.gen_range(18..100)));
        v
    });
    let mut rng_e = rand::thread_rng();
    let edges = raw_edges.into_iter().map(move |res| -> Result<BulkEdge, StoreError> {
        let mut e = res?;
        e.props.insert(WEIGHT_KEY.to_string(), Primitive::Float64(rng_e.gen_range(0.1..10.0)));
        e.props.insert(TIMESTAMP_KEY.to_string(), Primitive::Int64(rng_e.gen_range(0..1_000_000)));
        Ok(e)
    });

    let graph = Graph::open(&data_dir)?;
    let mut loader = graph.open_bulk_loader()?;
    if let Some(m) = max_memory {
        loader = loader.with_max_memory(m);
    }
    if let Some(s) = max_sst {
        loader = loader.with_max_sst_size(s);
    }

    loader.load_vertices(vertices)?;
    loader.load_edges(edges)?;
    let stats = loader.commit()?;
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
