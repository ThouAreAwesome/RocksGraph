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

//! Pull-based physical operators for the volcano execution engine.
//!
//! Each submodule is one physical step. Steps are wired into a chain by
//! [`PhysicalPlanBuilder`]: every step holds an optional upstream [`ConsumerIter`]
//! that it pulls from, and exposes a new `ConsumerIter` downstream callers pull
//! from in turn.
//!
//! The shared wiring protocol lives in [`traits`]:
//! - [`GremlinStep`] — the `Rc<RefCell<…>>` wrapper + `subscribe` factory.
//! - [`ConsumerIter`] — the opaque `Rc<dyn Step>` handle passed between steps.
//! - [`Step`] — the core pull trait: `next`, `reset`, `add_upper`.
//!
//! [`PhysicalPlanBuilder`]: crate::engine::volcano::builder::PhysicalPlanBuilder

// ── Physical step submodules ───────────────────────────────────────────────────
pub mod add_e;
pub mod add_v;
pub mod both;
pub mod coalesce;
pub mod count;
pub mod dedup;
pub mod drop;
pub mod e;
pub mod end_vertex_filter;
pub mod fold;
pub mod get_e;
pub mod has_id;
pub mod has_label;
pub mod has_property;
pub mod in_out;
pub mod in_v_out_v;
pub mod limit;
pub mod other_v;
pub mod path;
pub mod property;
pub mod repeat;
pub mod scalar_filter;
pub mod tests;
pub mod traits;
pub mod union;
pub mod v;
pub mod values;
pub mod vec_source;
pub mod r#where;

// ── Physical plan operators (storage-layer stubs) ─────────────────────────────

pub use traits::{CoreStep, StepRef};
