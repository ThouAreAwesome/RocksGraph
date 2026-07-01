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
use std::collections::{HashMap, HashSet};

use crate::types::{CanonicalKey, LabelId, Primitive, PropKey};

/// On-disk discriminant: 0=Auto, 1=Strict. Pinned explicitly (rather than relying on
/// declaration order) since these values are persisted in the `schema` CF; see
/// [`SchemaMode::to_u8`]/[`SchemaMode::from_u8`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum SchemaMode {
    #[default]
    Auto = 0,
    Strict = 1,
}

impl SchemaMode {
    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(SchemaMode::Auto),
            1 => Some(SchemaMode::Strict),
            _ => None,
        }
    }
}

/// On-disk discriminant: 0=Single, 1=Multi. Pinned explicitly for the same reason as
/// [`SchemaMode`]; see [`EdgeMode::to_u8`]/[`EdgeMode::from_u8`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum EdgeMode {
    #[default]
    Single = 0,
    Multi = 1,
}

impl EdgeMode {
    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(EdgeMode::Single),
            1 => Some(EdgeMode::Multi),
            _ => None,
        }
    }
}

/// On-disk discriminant, pinned explicitly for the same reason as [`SchemaMode`];
/// see [`DataType::to_u8`]/[`DataType::from_u8`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DataType {
    Null = 0,
    Bool = 1,
    Int32 = 2,
    Int64 = 3,
    Float32 = 4,
    Float64 = 5,
    String = 6,
    Uuid = 7,
    UInt16 = 8,
    Bytes = 9,
}

impl DataType {
    pub fn from_primitive(val: &Primitive) -> Self {
        match val {
            Primitive::Null => DataType::Null,
            Primitive::Bool(_) => DataType::Bool,
            Primitive::Int32(_) => DataType::Int32,
            Primitive::Int64(_) => DataType::Int64,
            Primitive::UInt16(_) => DataType::UInt16,
            Primitive::Float32(_) => DataType::Float32,
            Primitive::Float64(_) => DataType::Float64,
            Primitive::String(_) => DataType::String,
            Primitive::Uuid(_) => DataType::Uuid,
            Primitive::Bytes(_) => DataType::Bytes,
        }
    }

    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(DataType::Null),
            1 => Some(DataType::Bool),
            2 => Some(DataType::Int32),
            3 => Some(DataType::Int64),
            4 => Some(DataType::Float32),
            5 => Some(DataType::Float64),
            6 => Some(DataType::String),
            7 => Some(DataType::Uuid),
            8 => Some(DataType::UInt16),
            9 => Some(DataType::Bytes),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PropKeyConfig {
    pub data_type: DataType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphOptions {
    pub mode: SchemaMode,
    pub edge_mode: EdgeMode,
}

impl Default for GraphOptions {
    fn default() -> Self {
        Self { mode: SchemaMode::Auto, edge_mode: EdgeMode::Single }
    }
}

/// Maximum number of distinct vertex or edge labels.  `LabelId` is `i32`; valid IDs
/// range from `1..=i32::MAX`.  ID `0` is reserved for "no such label" and negative
/// values are internal sentinels.  Real IDs are allocated starting at `1` (see
/// `register_vertex_label`/`register_edge_label`).
pub(crate) const MAX_LABELS: usize = i32::MAX as usize;

/// Maximum number of distinct property keys — see [`MAX_LABELS`] for why this is one short of
/// the full 15-bit range. Property-key ids additionally reserve `1..=3` for the built-in
/// `id`/`label`/`rank` keys (see `prop_key::{ID_KEY_ID, LABEL_KEY_ID, RANK_KEY_ID}`); user keys
/// registered via `register_prop_key` start at `4`.
pub(crate) const MAX_PROP_KEYS: usize = (1 << 15) - 1;

/// Process-wide label and property-key dictionary, shared across transactions.
///
/// Provides bidirectional O(1) lookup between numeric IDs and string names.
/// All three maps are append-only after initial load; IDs are never reused.
///
/// Thread-safety: wrap in `Arc<RwLock<Schema>>` when shared across queries.
///
/// Crate-internal: external callers only ever interact with the schema through
/// [`SchemaManagement`](crate::schema::SchemaManagement) (declaration) and the traversal API
/// (implicit auto-registration) — never this registry directly.
#[derive(Debug, Clone)]
pub(crate) struct Schema {
    pub mode: SchemaMode,
    pub edge_mode: EdgeMode,
    pub version: u64,

    /// Maps between `LabelId` and the vertex label string (e.g. `"person"`).
    pub vertex_labels: BiHashMap<LabelId, SmolStr>,

    /// Maps between `LabelId` and the edge label string (e.g. `"knows"`).
    /// Uses the same `LabelId` space, but vertex and edge labels are
    /// independent namespaces — id 1 for vertices and id 1 for edges refer to
    /// different strings.
    pub edge_labels: BiHashMap<LabelId, SmolStr>,

    /// Maps between a compact `u16` id and the property key name.
    /// Interning is in-memory only; the on-disk format stores the raw string.
    pub prop_keys: BiHashMap<u16, PropKey>,

    /// Maps property key ID to its configuration type.
    pub prop_key_types: HashMap<u16, PropKeyConfig>,

    /// Set of vertex label IDs successfully persisted on disk.
    pub persisted_vertex_labels: HashSet<LabelId>,

    /// Set of edge label IDs successfully persisted on disk.
    pub persisted_edge_labels: HashSet<LabelId>,

    /// Set of property key IDs successfully persisted on disk.
    pub persisted_prop_keys: HashSet<u16>,
}

impl Default for Schema {
    fn default() -> Self {
        use crate::types::prop_key::{ID, ID_KEY_ID, LABEL, LABEL_KEY_ID, RANK, RANK_KEY_ID};
        use bimap::BiHashMap;
        use std::collections::{HashMap, HashSet};

        let mut prop_keys = BiHashMap::new();
        prop_keys.insert(ID_KEY_ID, ID);
        prop_keys.insert(LABEL_KEY_ID, LABEL);
        prop_keys.insert(RANK_KEY_ID, RANK);

        let mut prop_key_types = HashMap::new();
        prop_key_types.insert(ID_KEY_ID, PropKeyConfig { data_type: DataType::Int64 });
        prop_key_types.insert(LABEL_KEY_ID, PropKeyConfig { data_type: DataType::Int32 });
        prop_key_types.insert(RANK_KEY_ID, PropKeyConfig { data_type: DataType::UInt16 });

        let mut persisted_prop_keys = HashSet::new();
        persisted_prop_keys.insert(ID_KEY_ID);
        persisted_prop_keys.insert(LABEL_KEY_ID);
        persisted_prop_keys.insert(RANK_KEY_ID);

        Schema {
            mode: SchemaMode::Auto,
            edge_mode: EdgeMode::Single,
            version: 0,
            vertex_labels: BiHashMap::new(),
            edge_labels: BiHashMap::new(),
            prop_keys,
            prop_key_types,
            persisted_vertex_labels: HashSet::new(),
            persisted_edge_labels: HashSet::new(),
            persisted_prop_keys,
        }
    }
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

    /// Returns the number of registered vertex labels.
    /// Label IDs are 1-based and sequential, so this value equals `max(label_id)`.
    pub fn vertex_labels_count(&self) -> usize {
        self.vertex_labels.len()
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
        if self.vertex_labels.len() >= MAX_LABELS {
            return None;
        }
        // Ids start at 1 — 0 is reserved crate-internally to mean "no such label".
        let id = self.vertex_labels.len() as LabelId + 1;
        self.vertex_labels.insert(id, s);
        Some(id)
    }

    // ── Edge labels ───────────────────────────────────────────────────────────

    /// Look up the string for an edge `LabelId`.
    pub fn edge_label_str(&self, id: LabelId) -> Option<&SmolStr> {
        self.edge_labels.get_by_left(&id)
    }

    /// Returns the number of registered edge labels.
    /// Label IDs are 1-based and sequential, so this value equals `max(label_id)`.
    pub fn edge_labels_count(&self) -> usize {
        self.edge_labels.len()
    }

    /// Look up the `LabelId` for an edge label string.
    pub fn edge_label_id(&self, name: &str) -> Option<LabelId> {
        self.edge_labels.get_by_right(name).copied()
    }

    /// Register a new edge label, returning its id.
    pub fn register_edge_label(&mut self, name: impl Into<SmolStr>) -> Option<LabelId> {
        let s = name.into();
        if let Some(&id) = self.edge_labels.get_by_right(&s) {
            return Some(id);
        }
        if self.edge_labels.len() >= MAX_LABELS {
            return None;
        }
        // Ids start at 1 — 0 is reserved crate-internally to mean "no such label".
        let id = self.edge_labels.len() as LabelId + 1;
        self.edge_labels.insert(id, s);
        Some(id)
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
        if self.prop_keys.len() >= MAX_PROP_KEYS {
            return None;
        }
        // Ids start at 1 — 0 is reserved crate-internally to mean "no such key". The built-in
        // id/label/rank keys already occupy 1..=3 (see `prop_key::{ID_KEY_ID, LABEL_KEY_ID,
        // RANK_KEY_ID}`), so the first user-registered key naturally lands on 4.
        let id = self.prop_keys.len() as u16 + 1;
        self.prop_keys.insert(id, s);
        Some(id)
    }

    // ── Resolve & Declare Helpers ─────────────────────────────────────────────

    /// Resolve vertex label by name (mutating, SchemaMode-gated).
    pub fn resolve_vertex_label(&mut self, name: &str) -> Result<LabelId, crate::types::StoreError> {
        if let Some(id) = self.vertex_label_id(name) {
            return Ok(id);
        }
        if self.mode == SchemaMode::Strict {
            return Err(crate::types::StoreError::SchemaViolation(format!("Undeclared vertex label: '{}'", name)));
        }
        if let Some(id) = self.register_vertex_label(name) {
            self.version += 1;
            Ok(id)
        } else {
            Err(crate::types::StoreError::SchemaExhausted("Vertex label ID space exhausted".to_string()))
        }
    }

    /// Declare vertex label by name (explicit management).
    pub fn declare_vertex_label(&mut self, name: &str) -> Result<LabelId, crate::types::StoreError> {
        if let Some(id) = self.vertex_label_id(name) {
            return Ok(id);
        }
        if let Some(id) = self.register_vertex_label(name) {
            Ok(id)
        } else {
            Err(crate::types::StoreError::SchemaExhausted("Vertex label ID space exhausted".to_string()))
        }
    }

    /// Resolve edge label by name (mutating, SchemaMode-gated).
    pub fn resolve_edge_label(&mut self, name: &str) -> Result<LabelId, crate::types::StoreError> {
        if let Some(id) = self.edge_label_id(name) {
            return Ok(id);
        }
        if self.mode == SchemaMode::Strict {
            return Err(crate::types::StoreError::SchemaViolation(format!("Undeclared edge label: '{}'", name)));
        }
        if let Some(id) = self.register_edge_label(name) {
            self.version += 1;
            Ok(id)
        } else {
            Err(crate::types::StoreError::SchemaExhausted("Edge label ID space exhausted".to_string()))
        }
    }

    /// Declare edge label by name (explicit management).
    pub fn declare_edge_label(&mut self, name: &str) -> Result<LabelId, crate::types::StoreError> {
        if let Some(id) = self.edge_label_id(name) {
            return Ok(id);
        }
        if let Some(id) = self.register_edge_label(name) {
            Ok(id)
        } else {
            Err(crate::types::StoreError::SchemaExhausted("Edge label ID space exhausted".to_string()))
        }
    }

    /// Resolve property key by name (mutating, SchemaMode-gated).
    pub fn resolve_prop_key(&mut self, name: &str, inferred_type: DataType) -> Result<u16, crate::types::StoreError> {
        if let Some(id) = self.prop_key_id(name) {
            if let Some(config) = self.prop_key_types.get(&id) {
                if config.data_type != inferred_type {
                    return Err(crate::types::StoreError::SchemaViolation(format!(
                        "Property key '{}' is already defined with type {:?}, but requested {:?}",
                        name, config.data_type, inferred_type
                    )));
                }
            } else {
                self.prop_key_types.insert(id, PropKeyConfig { data_type: inferred_type });
                self.version += 1;
            }
            return Ok(id);
        }
        if self.mode == SchemaMode::Strict {
            return Err(crate::types::StoreError::SchemaViolation(format!("Undeclared property key: '{}'", name)));
        }
        if let Some(id) = self.register_prop_key(name) {
            self.prop_key_types.insert(id, PropKeyConfig { data_type: inferred_type });
            self.version += 1;
            Ok(id)
        } else {
            Err(crate::types::StoreError::SchemaExhausted("Property key ID space exhausted".to_string()))
        }
    }

    /// Declare property key by name (explicit management).
    pub fn declare_prop_key(&mut self, name: &str, data_type: DataType) -> Result<u16, crate::types::StoreError> {
        if data_type == DataType::Null {
            return Err(crate::types::StoreError::SchemaViolation(
                "'Null' is not a valid declared property type".to_string(),
            ));
        }
        if name == "id" || name == "label" || name == "rank" {
            return Err(crate::types::StoreError::SchemaViolation(format!(
                "'{}' is a system-reserved key and cannot be used as an ordinary property",
                name
            )));
        }
        if let Some(id) = self.prop_key_id(name) {
            if let Some(config) = self.prop_key_types.get(&id) {
                if config.data_type != data_type {
                    return Err(crate::types::StoreError::SchemaConflict(format!(
                        "Property key '{}' is already defined with type {:?}, but requested {:?}",
                        name, config.data_type, data_type
                    )));
                }
            }
            return Ok(id);
        }
        if let Some(id) = self.register_prop_key(name) {
            self.prop_key_types.insert(id, PropKeyConfig { data_type });
            Ok(id)
        } else {
            Err(crate::types::StoreError::SchemaExhausted("Property key ID space exhausted".to_string()))
        }
    }

    /// One-way ratchet: Multi -> Single is not allowed.
    pub fn declare_edge_mode(&mut self, mode: EdgeMode) -> Result<(), crate::types::StoreError> {
        if self.edge_mode == EdgeMode::Multi && mode == EdgeMode::Single {
            return Err(crate::types::StoreError::SchemaConflict(
                "edge_mode: Multi -> Single is not allowed".to_string(),
            ));
        }
        self.edge_mode = mode;
        Ok(())
    }

    /// Either direction is allowed.
    pub fn declare_schema_mode(&mut self, mode: SchemaMode) -> Result<(), crate::types::StoreError> {
        self.mode = mode;
        Ok(())
    }

    /// Decode a raw `Primitive::Int32(label_id)` — as returned by `get_value`/`get_property`
    /// for the reserved `"label"` key (see `Vertex`/`Edge::get_value`) — into the label's
    /// string name. Returns `value` unchanged for anything else, since this only ever
    /// applies to the synthesized `"label"` property.
    ///
    /// Falls back to the raw numeric id (stringified) if the id has no registered name,
    /// e.g. data written before the in-memory `Schema` was populated.
    pub fn decode_label_value(&self, key: &CanonicalKey, value: Primitive) -> Primitive {
        let Primitive::Int32(label_id) = value else { return value };
        let label_id = label_id as LabelId;
        let name = match key {
            CanonicalKey::Vertex(_) => self.vertex_label_str(label_id),
            CanonicalKey::Edge(_) => self.edge_label_str(label_id),
            CanonicalKey::Empty => None,
        };
        Primitive::String(name.cloned().unwrap_or_else(|| SmolStr::from(label_id.to_string())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `to_u8`/`from_u8` must be exact inverses: their values are the on-disk wire format
    // for the `schema` CF (see `store/rocks/encoding.rs`), shared by `SchemaManagement::commit`,
    // `LogicalGraph::commit`, and `RocksStorage::load_schema`. A mismatch here would silently
    // corrupt persisted schema data rather than fail loudly.
    #[test]
    fn schema_mode_u8_roundtrip() {
        for mode in [SchemaMode::Auto, SchemaMode::Strict] {
            assert_eq!(SchemaMode::from_u8(mode.to_u8()), Some(mode));
        }
        assert_eq!(SchemaMode::from_u8(2), None);
    }

    #[test]
    fn edge_mode_u8_roundtrip() {
        for mode in [EdgeMode::Single, EdgeMode::Multi] {
            assert_eq!(EdgeMode::from_u8(mode.to_u8()), Some(mode));
        }
        assert_eq!(EdgeMode::from_u8(2), None);
    }

    #[test]
    fn data_type_u8_roundtrip() {
        let all = [
            DataType::Null,
            DataType::Bool,
            DataType::Int32,
            DataType::Int64,
            DataType::Float32,
            DataType::Float64,
            DataType::String,
            DataType::Uuid,
            DataType::UInt16,
            DataType::Bytes,
        ];
        for dt in all {
            assert_eq!(DataType::from_u8(dt.to_u8()), Some(dt));
        }
        assert_eq!(DataType::from_u8(10), None);
    }
}
