// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use smol_str::SmolStr;
use std::collections::{HashMap, HashSet};

use crate::{
    schema::{definition::PropKeyConfig, EdgeMode, Schema},
    types::LabelId,
};

/// Eagerly captured transaction-local snapshot of the schema dictionary.
///
/// Avoids repeated read-lock acquisitions on the shared `Arc<RwLock<Schema>>`
/// during high-throughput property get/set/drop operations within a
/// transaction's lifetime. Label name/id resolution stays on `Schema`
/// directly — it happens once per traversal build, not per-property, so it
/// isn't worth duplicating here.
#[derive(Clone, Debug)]
pub(crate) struct TxSchemaCache {
    pub edge_mode: EdgeMode,
    pub persisted_vertex_labels: HashSet<LabelId>,
    pub persisted_edge_labels: HashSet<LabelId>,
    pub persisted_prop_keys: HashSet<u16>,
    prop_key_to_id: HashMap<SmolStr, u16>,
    prop_id_to_key: HashMap<u16, SmolStr>,
    prop_key_types: HashMap<u16, PropKeyConfig>,
}

impl TxSchemaCache {
    /// Build an eager snapshot cache from a Schema instance (under read lock or owned).
    pub fn from_schema(schema: &Schema) -> Self {
        let mut prop_key_to_id = HashMap::with_capacity(schema.prop_keys.len());
        let mut prop_id_to_key = HashMap::with_capacity(schema.prop_keys.len());
        for (&id, name) in &schema.prop_keys {
            prop_key_to_id.insert(name.clone(), id);
            prop_id_to_key.insert(id, name.clone());
        }

        Self {
            edge_mode: schema.edge_mode,
            persisted_vertex_labels: schema.persisted_vertex_labels.clone(),
            persisted_edge_labels: schema.persisted_edge_labels.clone(),
            persisted_prop_keys: schema.persisted_prop_keys.clone(),
            prop_key_to_id,
            prop_id_to_key,
            prop_key_types: schema.prop_key_types.clone(),
        }
    }

    #[inline]
    pub fn prop_key_id(&self, name: &str) -> Option<u16> {
        self.prop_key_to_id.get(name).copied()
    }

    #[inline]
    pub fn prop_key_str(&self, id: u16) -> Option<&SmolStr> {
        self.prop_id_to_key.get(&id)
    }

    #[inline]
    pub fn prop_key_type(&self, id: u16) -> Option<PropKeyConfig> {
        self.prop_key_types.get(&id).copied()
    }
}
