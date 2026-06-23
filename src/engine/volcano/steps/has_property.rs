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

use std::rc::Rc;

use smallvec::{smallvec, SmallVec};

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    planner::optimizer::primitive_to_rank,
    types::{
        error::StoreError,
        gvalue::Primitive,
        prop_key::{LABEL_KEY_ID, RANK_KEY_ID},
        CanonicalKey, GValue,
    },
};

/// A physical step that filters traversers based on a specific property key and its expected value.
#[derive(Debug)]
pub struct HasPropertyStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Static/Fixed configuration ──
    /// The property key ID to filter by.
    prop_key_id: u16,
    /// The expected value of the property.
    expected_value: Primitive,
}

/// Creates a new `HasPropertyStep` with the property key ID and expected value to filter by.
impl HasPropertyStep {
    /// Normalizes `expected_value` for reserved keys whose runtime representation doesn't
    /// match whatever literal type a caller wrote.
    ///
    /// This only matters for an *unmerged* `.has("rank", N)` — the common case
    /// (`.outE(...).has("rank", N)`) gets folded into a dedicated physical step by
    /// `merge_end_vertex_filter` before this step is ever built. But a `.has("rank", N)` that
    /// doesn't immediately follow an edge-emitting step falls through to here, and
    /// `Edge::get_value(RANK_KEY_ID)` always returns `Primitive::UInt16`. Without this, a
    /// perfectly valid `.has("rank", 5i32)` would compare `Primitive::Int32` against
    /// `Primitive::UInt16` and silently never match. Reuses `primitive_to_rank` — the same
    /// Int32/Int64/UInt16-to-`u16` conversion the merge rules already apply — so a value that
    /// isn't a valid rank (wrong type or out of range) is left as-is, which simply never
    /// matches the `UInt16` runtime value rather than panicking.
    pub fn new(prop_key_id: u16, expected_value: Primitive) -> Self {
        let expected_value = if prop_key_id == RANK_KEY_ID {
            primitive_to_rank(&expected_value).map(Primitive::UInt16).unwrap_or(expected_value)
        } else {
            expected_value
        };
        Self { upstream: None, prop_key_id, expected_value }
    }

    /// `ctx.get_value`/`get_property` return the element's label as a raw
    /// `Primitive::Int32(label_id)` (see `Vertex`/`Edge::get_value`) — decode it to the
    /// label's string name so `.has("label", "person")` compares like-for-like with the
    /// `Primitive::String` an expected value would naturally take.
    fn decode_if_label(&self, ctx: &dyn GraphCtx, key: &CanonicalKey, value: Primitive) -> Primitive {
        if self.prop_key_id != LABEL_KEY_ID {
            return value;
        }
        ctx.schema().read().unwrap().decode_label_value(key, value)
    }
}

impl CoreStep for HasPropertyStep {
    fn add_upper(&mut self, upstream: StepRef) {
        // Sets the upstream step for this filter.
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        // Produces traversers whose element has the specified property with the expected value.
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };
            match &t.value {
                GValue::Vertex(vk) => {
                    let key = CanonicalKey::Vertex(*vk);
                    if let Some(vl) = ctx.get_value(&key, self.prop_key_id)? {
                        let vl = self.decode_if_label(ctx, &key, vl);
                        if vl == self.expected_value {
                            return Ok(Some(smallvec![t]));
                        }
                    }
                }
                GValue::Edge(ek) => {
                    let key = CanonicalKey::Edge(ek.canonical_edge_key());
                    if let Some(et) = ctx.get_value(&key, self.prop_key_id)? {
                        let et = self.decode_if_label(ctx, &key, et);
                        if et == self.expected_value {
                            return Ok(Some(smallvec![t]));
                        }
                    }
                }
                _ => {}
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
