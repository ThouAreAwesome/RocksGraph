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

use bimap::BiHashMap;
use smol_str::SmolStr;
use std::collections::HashMap;

use crate::types::{LabelId, PropKey};

/// Configuration for an edge label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EdgeConfig {
    /// If true, multiple edges of this label are allowed between a vertex pair (Multigraph).
    /// If false, at most one edge of this label can exist between a vertex pair (Simple Graph).
    pub multi_edge: bool,
}

/// Maximum number of distinct vertex or edge labels (12-bit label_id space).
pub const MAX_LABELS: usize = 4096;

/// Process-wide label and property-key dictionary, shared across transactions.
///
/// Provides bidirectional O(1) lookup between numeric IDs and string names.
/// All three maps are append-only after initial load; IDs are never reused.
///
/// Thread-safety: wrap in `Arc<RwLock<Schema>>` when shared across queries.
#[derive(Debug, Default)]
pub struct Schema {
    /// Maps between `LabelId` and the vertex label string (e.g. `"person"`).
    pub vertex_labels: BiHashMap<LabelId, SmolStr>,

    /// Maps between `LabelId` and the edge label string (e.g. `"knows"`).
    /// Uses the same 12-bit `LabelId` space, but vertex and edge labels are
    /// independent namespaces — id 1 for vertices and id 1 for edges refer to
    /// different strings.
    pub edge_labels: BiHashMap<LabelId, SmolStr>,

    /// Maps between a compact `u16` id and the property key name.
    /// Interning is in-memory only; the on-disk format stores the raw string.
    pub prop_keys: BiHashMap<u16, PropKey>,

    /// Maps between `LabelId` and the edge label's configuration.
    pub edge_configs: HashMap<LabelId, EdgeConfig>,
}

impl Schema {
    pub fn new() -> Self {
        Self::default()
    }

    // ── Vertex labels ─────────────────────────────────────────────────────────

    /// Look up the string for a vertex `LabelId`.
    pub fn vertex_label_str(&self, id: LabelId) -> Option<&SmolStr> {
        self.vertex_labels.get_by_left(&id)
    }

    /// Look up the `LabelId` for a vertex label string.
    pub fn vertex_label_id(&self, name: &str) -> Option<LabelId> {
        self.vertex_labels.get_by_right(name).copied()
    }

    /// Register a new vertex label, returning its id.
    /// Returns the existing id if the label is already registered.
    /// Returns `None` if the id space is exhausted.
    pub fn register_vertex_label(&mut self, name: impl Into<SmolStr>) -> Option<LabelId> {
        let s = name.into();
        if let Some(&id) = self.vertex_labels.get_by_right(&s) {
            return Some(id);
        }
        let id = self.vertex_labels.len() as u16;
        if self.vertex_labels.len() >= MAX_LABELS {
            return None;
        }
        self.vertex_labels.insert(id, s);
        Some(id)
    }

    // ── Edge labels ───────────────────────────────────────────────────────────

    /// Look up the string for an edge `LabelId`.
    pub fn edge_label_str(&self, id: LabelId) -> Option<&SmolStr> {
        self.edge_labels.get_by_left(&id)
    }

    /// Look up the `LabelId` for an edge label string.
    pub fn edge_label_id(&self, name: &str) -> Option<LabelId> {
        self.edge_labels.get_by_right(name).copied()
    }

    /// Register a new edge label, returning its id.
    pub fn register_edge_label(&mut self, name: impl Into<SmolStr>) -> Option<LabelId> {
        self.register_edge_label_with_config(name, EdgeConfig::default())
    }

    /// Register a new edge label with a specific configuration, returning its id.
    pub fn register_edge_label_with_config(&mut self, name: impl Into<SmolStr>, config: EdgeConfig) -> Option<LabelId> {
        let s = name.into();
        let id = if let Some(&id) = self.edge_labels.get_by_right(&s) {
            id
        } else {
            let id = self.edge_labels.len() as u16;
            if self.edge_labels.len() >= MAX_LABELS {
                return None;
            }
            self.edge_labels.insert(id, s);
            id
        };
        self.edge_configs.insert(id, config);
        Some(id)
    }

    /// Retrieve the configuration of a registered edge label.
    pub fn edge_config(&self, id: LabelId) -> EdgeConfig {
        self.edge_configs.get(&id).copied().unwrap_or_default()
    }

    // ── Property keys ─────────────────────────────────────────────────────────

    /// Look up the string for a prop-key id.
    pub fn prop_key_str(&self, id: u16) -> Option<&PropKey> {
        self.prop_keys.get_by_left(&id)
    }

    /// Look up the id for a prop-key string.
    pub fn prop_key_id(&self, name: &str) -> Option<u16> {
        self.prop_keys.get_by_right(name).copied()
    }

    /// Register a new property key, returning its id.
    pub fn register_prop_key(&mut self, name: impl Into<PropKey>) -> Option<u16> {
        let s = name.into();
        if let Some(&id) = self.prop_keys.get_by_right(&s) {
            return Some(id);
        }
        let id = self.prop_keys.len() as u16;
        if self.prop_keys.len() >= u16::MAX as usize {
            return None;
        }
        self.prop_keys.insert(id, s);
        Some(id)
    }
}
