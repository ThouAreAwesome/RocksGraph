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
pub mod both_e;
pub mod coalesce;
pub mod count;
pub mod drop;
pub mod end_vertex_filter;
pub mod get_out_e;
pub mod has_id;
pub mod has_label;
pub mod has_property;
pub mod r#in;
pub mod in_e;
pub mod in_v;
pub mod limit;
pub mod other_v;
pub mod out;
pub mod out_e;
pub mod out_v;
pub mod property;
pub mod scalar_filter;
pub mod tests;
pub mod traits;
pub mod union;
pub mod v;
pub mod values;
pub mod vec_source;
pub mod r#where;
// ── Physical plan operators (storage-layer stubs) ─────────────────────────────

pub use traits::{BufferedStep, CoreStep, GremlinStep, StepRef};
