// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Input format adapters for [`SstBulkLoader`].
//!
//! Currently ships one format:
//!
//! - [`EdgeListSource`] — SNAP-format edge list (`src dst` per line).  Performs
//!   two passes over the file: Pass 1 collects unique vertex IDs into a
//!   `BTreeSet`; Pass 2 returns an [`EdgeListIter`] that reads edges lazily one
//!   line at a time (O(1) memory for the edge stream).
//!
//! Additional formats (`CsvEdgeSource`, `JsonLinesSource`, `GraphSONSource`,
//! `AdjacencyListSource`) are designed in
//! `docs/design_bulkload_source_formats.md` but not yet implemented.
//!
//! [`SstBulkLoader`]: super::bulk_loader::SstBulkLoader

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use crate::types::VertexKey;

use super::bulk_loader::{BulkEdge, BulkVertex};

/// Reads a SNAP-format edge list (one `src dst` pair per line, comment lines
/// start with `#`).
pub struct EdgeListSource {
    pub path: PathBuf,
    pub vertex_label: String,
    pub edge_label: String,
    pub comment_char: char,
}

impl EdgeListSource {
    /// Two-pass open: Pass 1 collects unique vertex IDs (in-memory, compact).
    /// Pass 2 streams edges lazily — no `Vec<BulkEdge>` is built in memory.
    pub fn open(self) -> std::io::Result<(Vec<BulkVertex>, EdgeListIter)> {
        // Pass 1: collect unique vertex IDs into a sorted set (~4.85M × 8B ≈ 40 MB).
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
                    ids.insert(src as VertexKey);
                    ids.insert(dst as VertexKey);
                }
            }
        }

        let vertices: Vec<BulkVertex> = ids
            .into_iter()
            .map(|id| BulkVertex { id, label: self.vertex_label.clone(), props: HashMap::new() })
            .collect();

        // Pass 2: re-open for lazy edge streaming (no Vec<BulkEdge> allocated).
        let file = File::open(&self.path)?;
        let edge_iter =
            EdgeListIter { reader: BufReader::new(file), edge_label: self.edge_label, comment_char: self.comment_char };

        Ok((vertices, edge_iter))
    }
}

/// Lazy iterator over edges in a SNAP edge-list file.
/// Reads and parses one line at a time — peak memory is O(1) per edge.
pub struct EdgeListIter {
    reader: BufReader<File>,
    edge_label: String,
    comment_char: char,
}

impl Iterator for EdgeListIter {
    type Item = BulkEdge;

    fn next(&mut self) -> Option<Self::Item> {
        let mut line = String::new();
        loop {
            line.clear();
            match self.reader.read_line(&mut line) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {}
            }
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with(self.comment_char) {
                continue;
            }
            let mut parts = trimmed.split_whitespace();
            if let (Some(s), Some(d)) = (parts.next(), parts.next()) {
                if let (Ok(src), Ok(dst)) = (s.parse::<i64>(), d.parse::<i64>()) {
                    return Some(BulkEdge {
                        src: src as VertexKey,
                        dst: dst as VertexKey,
                        label: self.edge_label.clone(),
                        props: HashMap::new(),
                        rank: None,
                    });
                }
            }
        }
    }
}
