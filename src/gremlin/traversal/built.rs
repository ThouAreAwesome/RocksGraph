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

//! Terminal execution types: [`BuiltTraversal`] (the lazy iterator) and the
//! internal [`materialize`] function that converts engine values into user-facing
//! [`Value`]s.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use smol_str::SmolStr;

use crate::{
    engine::volcano::builder::PhysicalPlan,
    gremlin::{
        type_bridge::primitive_to_value,
        value::{Edge as UserEdge, Map, Path, Property as UserProperty, Value, Vertex as UserVertex},
    },
    schema::Schema,
    types::{
        gvalue::GValue,
        keys::{CanonicalKey, VertexKey},
        StoreError,
    },
};

/// Pre-built lookup table: label_id → SmolStr, avoiding per-result BiHashMap lookups.
pub(crate) struct LabelCache {
    vertex_labels: HashMap<u16, SmolStr>,
    edge_labels: HashMap<u16, SmolStr>,
}

impl LabelCache {
    pub(crate) fn from_schema(schema: &Schema) -> Self {
        // Iterate all known vertex label ids and resolve eagerly.
        let mut vertex_labels = HashMap::new();
        for id in 1..=schema.vertex_labels_count() as u16 {
            if let Some(name) = schema.vertex_label_str(id) {
                vertex_labels.insert(id, name.clone());
            }
        }
        let mut edge_labels = HashMap::new();
        for id in 1..=schema.edge_labels_count() as u16 {
            if let Some(name) = schema.edge_label_str(id) {
                edge_labels.insert(id, name.clone());
            }
        }
        Self { vertex_labels, edge_labels }
    }

    #[inline]
    fn vertex_label(&self, label_id: u16) -> &SmolStr {
        self.vertex_labels.get(&label_id).unwrap_or_else(|| {
            static EMPTY: SmolStr = SmolStr::new_inline("");
            &EMPTY
        })
    }

    #[inline]
    fn edge_label(&self, label_id: u16) -> &SmolStr {
        self.edge_labels.get(&label_id).unwrap_or_else(|| {
            static EMPTY: SmolStr = SmolStr::new_inline("");
            &EMPTY
        })
    }
}

/// Materialize an internal [`GValue`] into a user-facing [`Value`].
///
/// `prop_keys` controls property fetching:
/// - `None` → default: return id + label only, no property reads.
/// - `Some([])` → fetch and return all properties (existing behavior).
/// - `Some(keys)` → fetch only named properties.
///
/// `schema` should be a pre-acquired read-lock on the schema registry,
/// passed through from [`BuiltTraversal::next`] to avoid repeated lock contention.
pub(crate) fn materialize(
    gv: &GValue,
    ctx: &mut dyn crate::engine::GraphCtx,
    schema: &Schema,
    cache: &LabelCache,
    prop_keys: Option<&[SmolStr]>,
) -> Result<Value, StoreError> {
    match gv {
        GValue::Scalar(ref p) => Ok(primitive_to_value(p.clone())),
        GValue::Vertex(vk) => materialize_vertex(*vk, ctx, schema, cache, prop_keys),
        GValue::Edge(ek) => materialize_edge(*ek, ctx, schema, cache, prop_keys),
        GValue::Property(ref p) => {
            let key = schema.prop_key_str(p.key).cloned().unwrap_or_else(|| SmolStr::from(format!("key_{}", p.key)));
            Ok(Value::Property(UserProperty { key, value: Box::new(primitive_to_value(p.value.clone())) }))
        }
        GValue::List(ref list) => {
            let mut out = Vec::with_capacity(list.len());
            for item in list {
                out.push(materialize(item, ctx, schema, cache, prop_keys)?);
            }
            Ok(Value::List(out))
        }
        GValue::Map(ref map) => {
            let mut out = Map::new();
            for (k, v) in map {
                out.entries.push((
                    materialize(k, ctx, schema, cache, prop_keys)?,
                    materialize(v, ctx, schema, cache, prop_keys)?,
                ));
            }
            Ok(Value::Map(out))
        }
        GValue::Path(ref path) => {
            let mut objects = Vec::with_capacity(path.len());
            let mut labels: Vec<Vec<String>> = Vec::with_capacity(path.len());
            for (val, step_labels) in path {
                objects.push(materialize(val, ctx, schema, cache, prop_keys)?);
                labels.push(match step_labels {
                    Some(ls) => ls.iter().map(|s| s.to_string()).collect(),
                    None => vec![],
                });
            }
            Ok(Value::Path(Path { objects, labels }))
        }
    }
}

/// Materialize a vertex, respecting the property fetch hint.
fn materialize_vertex(
    vk: VertexKey,
    ctx: &mut dyn crate::engine::GraphCtx,
    _schema: &Schema,
    cache: &LabelCache,
    prop_keys: Option<&[SmolStr]>,
) -> Result<Value, StoreError> {
    match prop_keys {
        // Default: id + label only, skip properties.
        None => match ctx.get_all_props(&CanonicalKey::Vertex(vk))? {
            None => Err(StoreError::NotFound),
            Some((label_id, _props)) => Ok(Value::Vertex(UserVertex {
                id: vk,
                label: cache.vertex_label(label_id).clone(),
                properties: HashMap::new(),
            })),
        },
        // All properties — existing behavior.
        Some([]) => match ctx.get_all_props(&CanonicalKey::Vertex(vk))? {
            None => Err(StoreError::NotFound),
            Some((label_id, props)) => {
                let label = cache.vertex_label(label_id).clone();
                let mut properties: HashMap<SmolStr, Vec<Value>> = HashMap::new();
                for (key, prim) in props {
                    properties.entry(key).or_default().push(primitive_to_value(prim));
                }
                Ok(Value::Vertex(UserVertex { id: vk, label, properties }))
            }
        },
        // Named properties only — filters on client side after fetching.
        Some(keys) => match ctx.get_all_props(&CanonicalKey::Vertex(vk))? {
            None => Err(StoreError::NotFound),
            Some((label_id, all_props)) => {
                let label = cache.vertex_label(label_id).clone();
                let mut properties: HashMap<SmolStr, Vec<Value>> = HashMap::new();
                for (prop_name, prim) in all_props {
                    if keys.iter().any(|k| k.as_str() == prop_name.as_str()) {
                        properties.entry(prop_name).or_default().push(primitive_to_value(prim));
                    }
                }
                Ok(Value::Vertex(UserVertex { id: vk, label, properties }))
            }
        },
    }
}

/// Materialize an edge, respecting the property fetch hint.
fn materialize_edge(
    ek: crate::types::keys::EdgeKey,
    ctx: &mut dyn crate::engine::GraphCtx,
    _schema: &Schema,
    cache: &LabelCache,
    prop_keys: Option<&[SmolStr]>,
) -> Result<Value, StoreError> {
    let cek = ek.canonical_edge_key();
    match prop_keys {
        // Default: id + label only — label_id is in the key, zero store reads.
        None => Ok(Value::Edge(UserEdge {
            out_v: cek.src_id,
            in_v: cek.dst_id,
            label: cache.edge_label(ek.label_id).clone(),
            rank: cek.rank,
            properties: HashMap::new(),
        })),
        // All properties — existing behavior.
        Some([]) => match ctx.get_all_props(&CanonicalKey::Edge(cek))? {
            None => Err(StoreError::NotFound),
            Some((label_id, props)) => {
                let label = cache.edge_label(label_id).clone();
                let mut properties: HashMap<SmolStr, Value> = HashMap::new();
                for (key, prim) in props {
                    properties.insert(key, primitive_to_value(prim));
                }
                Ok(Value::Edge(UserEdge { out_v: cek.src_id, in_v: cek.dst_id, label, rank: cek.rank, properties }))
            }
        },
        // Named properties only — filters on client side after fetching.
        Some(keys) => match ctx.get_all_props(&CanonicalKey::Edge(cek))? {
            None => Err(StoreError::NotFound),
            Some((label_id, all_props)) => {
                let label = cache.edge_label(label_id).clone();
                let mut properties: HashMap<SmolStr, Value> = HashMap::new();
                for (prop_name, prim) in all_props {
                    if keys.iter().any(|k| k.as_str() == prop_name.as_str()) {
                        properties.insert(prop_name, primitive_to_value(prim));
                    }
                }
                Ok(Value::Edge(UserEdge { out_v: cek.src_id, in_v: cek.dst_id, label, rank: cek.rank, properties }))
            }
        },
    }
}

// ── BuiltTraversal ────────────────────────────────────────────────────────────

/// The result of building a traversal — a pull-based lazy iterator over results.
///
/// Obtained from [`ReadTraversal::iter`] or [`WriteTraversal::iter`].
/// Implements `Iterator<Item = Result<Value, StoreError>>`.
///
/// Holds the schema lock for the duration of iteration so that
/// [`materialize`] does not re-acquire it per result.
///
/// [`ReadTraversal::iter`]: super::ReadTraversal::iter
/// [`WriteTraversal::iter`]: super::WriteTraversal::iter
pub struct BuiltTraversal<'g> {
    pub(super) graph: &'g mut dyn crate::engine::GraphCtx,
    pub(super) plan: PhysicalPlan,
    pub(super) schema: Arc<RwLock<Schema>>,
    /// Property fetch hint set by `withProperties()`.
    /// - `None` → default: id + label only, no properties.
    /// - `Some(vec![])` → all properties.
    /// - `Some(vec!["name"])` → named properties only.
    pub(super) prop_keys: Option<Vec<SmolStr>>,
    /// Pre-built label resolution cache (label_id → string).
    pub(super) cache: LabelCache,
}

impl<'g> Iterator for BuiltTraversal<'g> {
    type Item = Result<Value, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.plan.next(self.graph) {
            Err(e) => Some(Err(e)),
            Ok(None) => None,
            Ok(Some(t)) => {
                let schema = self.schema.read().unwrap();
                Some(materialize(&t.value, self.graph, &schema, &self.cache, self.prop_keys.as_deref()))
            }
        }
    }
}
