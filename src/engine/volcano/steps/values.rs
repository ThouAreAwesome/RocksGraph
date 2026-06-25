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

use crate::types::PIPELINE_BATCH_INLINE;
use std::rc::Rc;

use smallvec::{smallvec, SmallVec};

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{error::StoreError, keys::CanonicalKey, prop_key::LABEL_KEY_ID, GValue},
};
use smol_str::SmolStr;

/// A physical step that extracts property values from the elements carried by incoming traversers.
#[derive(Debug)]
pub struct ValuesStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Static/Fixed configuration ──
    /// Specific property keys to extract as (name, key_id) pairs.
    property_keys: SmallVec<[(SmolStr, u16); PIPELINE_BATCH_INLINE]>,
    /// Whether to emit properties as `GValue::Property` (true) or their raw scalar values (false).
    emit_property: bool,
    /// Whether to link the parent chain on emitted traversers (`false` skips the `Rc::clone`
    /// when the plan has no `as()`/`select()`/`path()` anywhere in it).
    track_path: bool,
}

/// Creates a new `ValuesStep` to extract specified property values.
impl ValuesStep {
    pub fn new(
        property_keys: SmallVec<[(SmolStr, u16); PIPELINE_BATCH_INLINE]>,
        emit_property: bool,
        track_path: bool,
    ) -> Self {
        Self { upstream: None, property_keys, emit_property, track_path }
    }
}

impl CoreStep for ValuesStep {
    fn add_upper(&mut self, upstream: StepRef) {
        // Sets the upstream step for this property extraction.
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        // Produces traversers carrying the extracted property values (either as `GValue::Scalar` or
        // `GValue::Property`).
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };
            let canonical_key = match &t.value {
                GValue::Vertex(vt) => CanonicalKey::Vertex(*vt),
                GValue::Edge(eg) => CanonicalKey::Edge(eg.canonical_edge_key()),
                _ => continue,
            };

            if self.property_keys.is_empty() {
                // TODO: implement fetching all properties if property_keys is empty
                continue;
            }

            let mut results = smallvec![];
            if self.emit_property {
                for (_, key_id) in &self.property_keys {
                    if let Some(mut value) = ctx.get_property(&canonical_key, *key_id)? {
                        if *key_id == LABEL_KEY_ID {
                            let schema_guard = ctx.schema();
                            value.value = schema_guard.read().unwrap().decode_label_value(&canonical_key, value.value);
                        }
                        results.push(Traverser::new_rc_conditional(GValue::Property(value), &t, self.track_path));
                    }
                }
            } else {
                for (_, key_id) in &self.property_keys {
                    if let Some(mut value) = ctx.get_value(&canonical_key, *key_id)? {
                        if *key_id == LABEL_KEY_ID {
                            let schema_guard = ctx.schema();
                            value = schema_guard.read().unwrap().decode_label_value(&canonical_key, value);
                        }
                        results.push(Traverser::new_rc_conditional(GValue::Scalar(value), &t, self.track_path));
                    }
                }
            }

            if !results.is_empty() {
                return Ok(Some(results));
            }
        }
    }

    fn reset(&mut self) {
        // Resets the state of this step and its upstream.
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        // Returns a clone of the upstream step reference.
        self.upstream.clone()
    }
}
