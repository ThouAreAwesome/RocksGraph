// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sort-merge join that attaches `end_vertex_label` to each edge record.

use crate::types::{keys::VertexKey, kv_codec, StoreError};

use super::degree::SortedLabelFile;
use super::sort::ExternalSorter;

/// Streams `annot_iter` (keyed by `[lookup_vertex_id: 8][edge_key: 22]`) against
/// `label_file` in sorted order, resolving each edge's `end_vertex_label` and
/// pushing `(edge_key, EdgeValue)` pairs into `out_sorter`.
pub(crate) fn annotate_edges(
    annot_iter: impl Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>>,
    label_file: &SortedLabelFile,
    out_sorter: &mut ExternalSorter,
) -> Result<(), StoreError> {
    let mut label_iter = label_file.reader()?;
    let mut cur = label_iter.next().transpose()?;
    let mut cached: Option<(VertexKey, crate::types::keys::LabelId)> = None;

    for item in annot_iter {
        let (key, props) = item?;
        if key.len() != 30 {
            return Err(StoreError::CorruptData("annotation key must be 30 bytes"));
        }
        let lookup_id = VertexKey::from_be_bytes(key[0..8].try_into().unwrap());
        let edge_key = key[8..30].to_vec();

        let label = if cached.map(|(v, _)| v) == Some(lookup_id) {
            cached.unwrap().1
        } else {
            loop {
                match cur {
                    None => {
                        return Err(StoreError::SchemaViolation(format!(
                            "edge references vertex {lookup_id} not in vertex set"
                        )));
                    }
                    Some((vid, _)) if vid < lookup_id => {
                        cur = label_iter.next().transpose()?;
                    }
                    Some((vid, lid)) if vid == lookup_id => {
                        cached = Some((vid, lid));
                        break lid;
                    }
                    Some((vid, _)) => {
                        return Err(StoreError::SchemaViolation(format!(
                            "edge references vertex {lookup_id} not in vertex set (next in file: {vid})"
                        )));
                    }
                }
            }
        };

        out_sorter.push(edge_key, kv_codec::EdgeValue { end_vertex_label: label, property_blob: props }.encode())?;
    }
    Ok(())
}
