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

use std::sync::Arc;

use crate::{
    schema::definition::{DataType, EdgeMode, Schema, SchemaMode},
    store::RocksStorage,
    types::StoreError,
};

/// High-level management interface for defining schema labels and properties.
///
/// Obtain one via [`Graph::open_management`](crate::api::Graph::open_management). This is the
/// explicit-declaration counterpart to [`SchemaMode::Auto`] (the default): in `Auto` mode,
/// vertex labels, edge labels, and property keys are registered implicitly the first time a
/// traversal uses them. In [`SchemaMode::Strict`], nothing is registered implicitly — every
/// name used by a traversal must already have been declared and committed here, or the write
/// is rejected with [`StoreError::SchemaViolation`].
///
/// # Example: `Strict` mode requires declaring the schema first
///
/// ```
/// use rocksgraph::{
///     schema::{DataType, GraphOptions, SchemaMode},
///     Graph, StoreError,
/// };
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let dir = tempfile::tempdir()?;
/// let options = GraphOptions { mode: SchemaMode::Strict, ..Default::default() };
/// let graph = Graph::open_with_options(dir.path(), options)?;
///
/// // Declare the schema up front -- required before any write in `Strict` mode.
/// let mut mgmt = graph.open_management();
/// mgmt.make_vertex_label("person").make();
/// mgmt.make_property_key("name", DataType::String).make();
/// mgmt.commit()?;
///
/// // A declared label commits normally.
/// let mut tx = graph.begin();
/// tx.g().addV("person").property("id", 1i64).property("name", "Alice").next()?;
/// tx.commit()?;
///
/// // An undeclared label is rejected outright, rather than silently auto-registered.
/// let mut tx = graph.begin();
/// let err = tx.g().addV("ghost").property("id", 2i64).next().unwrap_err();
/// assert!(matches!(err, StoreError::SchemaViolation(_)));
/// # Ok(())
/// # }
/// ```
///
/// # Schema model
///
/// Three properties of this schema are easy to assume otherwise, so they're called out
/// explicitly here:
///
/// 1. **A property key is global, with exactly one type, shared by every vertex label and every
///    edge label.** `make_property_key("weight", DataType::Float64)` declares "weight" once for
///    the *entire graph* — there is no per-vertex-label or per-edge-label property scoping. A
///    `"person"` vertex, a `"software"` vertex, and a `"knows"` edge that all set a `"weight"`
///    property are all writing to the *same* property key definition, and all of them must use
///    `Float64`; declaring (or auto-inferring) `"weight"` as a second, incompatible type from any
///    of them is a [`StoreError::SchemaConflict`]/[`StoreError::SchemaViolation`]. Every declared
///    property key is implicitly legal on every label, vertex or edge alike — there's no way to
///    restrict a key to specific labels.
/// 2. **Edge multiplicity (`EdgeMode`) is one graph-wide setting, not per-edge-label.**
///    [`set_edge_mode`](SchemaManagement::set_edge_mode) flips `Single`/`Multi` for *every* edge
///    label at once — there's no way for one edge label (e.g. `"knows"`) to stay `Single` while
///    another (e.g. `"created"`) is `Multi` in the same graph. `Multi` mode requires an explicit
///    `"rank"` property to disambiguate otherwise-identical parallel edges.
/// 3. **No vertex-label ↔ edge-label connection constraints.** Any edge label can connect any two
///    vertices regardless of their labels — there's no way to declare "`knows` only connects
///    `person` to `person`".
///
/// [`SchemaMode`] (`Auto`/`Strict`) is also a single graph-wide setting rather than per-label.
pub struct SchemaManagement {
    store: Arc<RocksStorage>,
    schema: Arc<std::sync::RwLock<Schema>>,
    base_version: u64,
    pending_vertex_labels: Vec<String>,
    pending_edge_labels: Vec<String>,
    pending_prop_keys: Vec<(String, DataType)>,
    pending_edge_mode: Option<EdgeMode>,
    pending_schema_mode: Option<SchemaMode>,
}

impl SchemaManagement {
    /// Crate-internal: obtain a `SchemaManagement` session via [`Graph::open_management`](crate::api::Graph::open_management).
    pub(crate) fn new(store: Arc<RocksStorage>, schema: Arc<std::sync::RwLock<Schema>>) -> Self {
        let base_version = schema.read().unwrap().version;
        Self {
            store,
            schema,
            base_version,
            pending_vertex_labels: Vec::new(),
            pending_edge_labels: Vec::new(),
            pending_prop_keys: Vec::new(),
            pending_edge_mode: None,
            pending_schema_mode: None,
        }
    }

    /// Create a maker for a vertex label.
    pub fn make_vertex_label(&mut self, name: impl Into<String>) -> VertexLabelMaker<'_> {
        VertexLabelMaker { mgmt: self, name: name.into() }
    }

    /// Create a maker for an edge label.
    pub fn make_edge_label(&mut self, name: impl Into<String>) -> EdgeLabelMaker<'_> {
        EdgeLabelMaker { mgmt: self, name: name.into() }
    }

    /// Create a maker for a property key.
    pub fn make_property_key(&mut self, name: impl Into<String>, data_type: DataType) -> PropertyKeyMaker<'_> {
        PropertyKeyMaker { mgmt: self, name: name.into(), data_type }
    }

    /// Stage a graph-wide multiplicity change.
    pub fn set_edge_mode(&mut self, mode: EdgeMode) -> &mut Self {
        self.pending_edge_mode = Some(mode);
        self
    }

    /// Stage a graph-wide schema-mode change.
    pub fn set_schema_mode(&mut self, mode: SchemaMode) -> &mut Self {
        self.pending_schema_mode = Some(mode);
        self
    }

    /// Commit the staged declarations to the schema registry and RocksDB.
    ///
    /// Applied atomically: every staged declaration is validated against a private
    /// clone of the schema first. If any one of them fails (a `SchemaConflict` from an
    /// incompatible redeclaration, or `SchemaExhausted`), the whole batch is discarded
    /// without the live, shared `Schema` ever being mutated — items staged earlier in the
    /// same batch do not leak through despite having validated cleanly. The live `Schema`
    /// is only swapped in (and the RocksDB write only issued) once every staged item has
    /// been validated successfully.
    pub fn commit(self) -> Result<(), StoreError> {
        use crate::store::rocks::encoding::{
            encode_schema_key, encode_schema_label_value, encode_schema_meta, encode_schema_prop_value, CF_SCHEMA,
            SCHEMA_KIND_EDGE_LABEL, SCHEMA_KIND_PROP_KEY, SCHEMA_KIND_VERTEX_LABEL, SCHEMA_META_KEY,
        };
        use rocksdb::WriteBatchWithTransaction;

        let mut schema = self.schema.write().map_err(|_| StoreError::LockError)?;

        // CAS check
        if schema.version != self.base_version {
            return Err(StoreError::SchemaConflict(format!(
                "Concurrent schema modification: base version {}, current version {}",
                self.base_version, schema.version
            )));
        }

        // Validate and apply every staged change against a private clone first, so a
        // failure partway through the batch never mutates the live `schema`.
        let mut staged = schema.clone();
        let mut changed = false;

        if let Some(edge_mode) = self.pending_edge_mode {
            changed |= staged.edge_mode != edge_mode;
            staged.declare_edge_mode(edge_mode)?;
        }

        if let Some(schema_mode) = self.pending_schema_mode {
            changed |= staged.mode != schema_mode;
            staged.declare_schema_mode(schema_mode)?;
        }

        let cf = self.store.db.cf_handle(CF_SCHEMA).ok_or(StoreError::MissingColumnFamily(CF_SCHEMA))?;

        let mut batch = WriteBatchWithTransaction::<true>::default();

        // 1. Process vertex labels
        for name in &self.pending_vertex_labels {
            let is_new = staged.vertex_label_id(name).is_none();
            let id = staged.declare_vertex_label(name)?;
            changed |= is_new;
            let key = encode_schema_key(SCHEMA_KIND_VERTEX_LABEL, name);
            let val = encode_schema_label_value(id);
            batch.put_cf(&cf, key, val);
            staged.persisted_vertex_labels.insert(id);
        }

        // 2. Process edge labels
        for name in &self.pending_edge_labels {
            let is_new = staged.edge_label_id(name).is_none();
            let id = staged.declare_edge_label(name)?;
            changed |= is_new;
            let key = encode_schema_key(SCHEMA_KIND_EDGE_LABEL, name);
            let val = encode_schema_label_value(id);
            batch.put_cf(&cf, key, val);
            staged.persisted_edge_labels.insert(id);
        }

        // 3. Process property keys
        for (name, data_type) in &self.pending_prop_keys {
            let is_new = staged.prop_key_id(name).is_none();
            let id = staged.declare_prop_key(name, *data_type)?;
            changed |= is_new;
            let key = encode_schema_key(SCHEMA_KIND_PROP_KEY, name);
            let val = encode_schema_prop_value(id, data_type.to_u8());
            batch.put_cf(&cf, key, val);
            staged.persisted_prop_keys.insert(id);
        }

        // A batch that only re-declared already-existing names with identical configs (or
        // staged nothing at all) is a no-op: idempotent re-runs of a schema-setup script
        // must not bump `version` or touch RocksDB.
        if !changed {
            return Ok(());
        }

        // 4. Increment and write meta
        staged.version += 1;
        let meta_bytes = encode_schema_meta(staged.version, staged.edge_mode.to_u8(), staged.mode.to_u8());
        batch.put_cf(&cf, SCHEMA_META_KEY, meta_bytes);

        self.store.db.write(batch).map_err(StoreError::RocksDb)?;

        // Only now, after the batch is durably written, does the validated clone replace
        // the live schema.
        *schema = staged;

        Ok(())
    }
}

pub struct VertexLabelMaker<'a> {
    mgmt: &'a mut SchemaManagement,
    name: String,
}

impl<'a> VertexLabelMaker<'a> {
    pub fn make(self) {
        self.mgmt.pending_vertex_labels.push(self.name);
    }
}

pub struct EdgeLabelMaker<'a> {
    mgmt: &'a mut SchemaManagement,
    name: String,
}

impl<'a> EdgeLabelMaker<'a> {
    pub fn make(self) {
        self.mgmt.pending_edge_labels.push(self.name);
    }
}

pub struct PropertyKeyMaker<'a> {
    mgmt: &'a mut SchemaManagement,
    name: String,
    data_type: DataType,
}

impl<'a> PropertyKeyMaker<'a> {
    pub fn make(self) {
        self.mgmt.pending_prop_keys.push((self.name, self.data_type));
    }
}
