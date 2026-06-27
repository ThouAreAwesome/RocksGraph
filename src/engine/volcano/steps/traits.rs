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

use crate::types::PIPELINE_PRODUCE_INLINE;
use crate::{
    engine::{context::GraphCtx, traverser::Traverser},
    types::error::StoreError,
};
use smallvec::SmallVec;
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

// ── ExplainNode ───────────────────────────────────────────────────────────────

/// A structured node in an explain tree — name, key-value params, and labelled
/// child sub-trees.  Built by each step's `CoreStep::explain()` impl and
/// rendered by the `PhysicalPlan` pretty-printer.
#[derive(Debug, Clone)]
pub(crate) struct ExplainNode {
    pub(crate) name: &'static str,
    pub(crate) params: Vec<(&'static str, String)>,
    pub(crate) children: Vec<(String, ExplainNode)>,
}

impl ExplainNode {
    pub(crate) fn new(name: &'static str) -> Self {
        ExplainNode { name, params: vec![], children: vec![] }
    }

    pub(crate) fn with_params(mut self, params: Vec<(&'static str, String)>) -> Self {
        self.params = params;
        self
    }

    pub(crate) fn with_children(mut self, children: Vec<(String, ExplainNode)>) -> Self {
        self.children = children;
        self
    }
}

// ── Public step reference ─────────────────────────────────────────────────────

/// Type-erased handle to any step. Downstream steps hold one of these for each
/// upstream they were wired to.
pub type StepRef = Rc<dyn GremlinStep>;

// ── GremlinStep — the public iterator interface ───────────────────────────────

/// The interface callers and downstream steps use. All methods take `&self`
/// because interior mutability is encapsulated inside [`BufferedStep`].
pub trait GremlinStep: std::fmt::Debug {
    fn next(&self, ctx: &mut dyn GraphCtx) -> Result<Option<Rc<Traverser>>, StoreError>;
    fn reset(&self);
    fn add_upper(&self, upstream: StepRef);
    fn upper(&self) -> Option<StepRef>;
    /// Return a structured explain node for this step.  Used by `explain()`.
    fn explain(&self) -> ExplainNode;
}

// ── CoreStep — what each step author implements ───────────────────────────────

/// The trait step authors implement. `&mut self` is safe here because
/// [`BufferedStep`] wraps every `CoreStep` in a single `RefCell`.
pub trait CoreStep: std::fmt::Debug {
    /// Wires an upstream step. Called once per upstream during plan construction.
    fn add_upper(&mut self, upstream: StepRef);

    /// Pull the next batch of results. Returns `Ok(None)` when exhausted,
    /// `Err` on storage or runtime failure.
    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_INLINE]>>, StoreError>;

    /// Reset all mutable state and propagate to upstreams.
    fn reset(&mut self);

    /// Access the upstream step if one exists. Defaults to None for source steps.
    fn upper(&self) -> Option<StepRef> {
        None
    }

    /// Return a structured explain node — no default impl, every step must
    /// provide one explicitly.
    fn explain(&self) -> ExplainNode;
}

// ── BufferedStep — the single generic wrapper ─────────────────────────────────

/// Joint container for a step's core logic and its output buffer.
/// Kept together so [`BufferedStep`] only needs one [`RefCell`] borrow per,
/// [`GremlinStep::next`] call.
pub(crate) struct StepInner<T: CoreStep> {
    pub(crate) core: T,
    buffer: VecDeque<Rc<Traverser>>,
}

/// Wraps any [`CoreStep`] and provides the full [`GremlinStep`] interface for
/// free. The buffer drains items produced by `core.produce` one at a time so
/// callers always get exactly one traverser per `next` call.
/// This struct manages the buffering of results and ensures safe interior mutability using `RefCell`.
/// A single `RefCell` guards both `core` and `buffer` so each `next` call
/// performs exactly one borrow rather than four.
pub struct BufferedStep<T: CoreStep> {
    pub(crate) inner: RefCell<StepInner<T>>,
}

/// Implements `BufferedStep` for any `CoreStep`.
impl<T: CoreStep + 'static> BufferedStep<T> {
    /// Pre-allocates the output buffer to 4 slots — matching the inline
    /// capacity of the [`SmallVec`] that [`CoreStep::produce`] returns.
    /// Avoids the 0→4 reallocation chain on the first `produce()` call.
    pub fn new(core: T) -> Rc<Self> {
        Rc::new(Self { inner: RefCell::new(StepInner { core, buffer: VecDeque::with_capacity(PIPELINE_PRODUCE_INLINE + 1) }) })
    }
}

impl<T: CoreStep + 'static> std::fmt::Debug for BufferedStep<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.borrow().core.fmt(f)
    }
}

impl<T: CoreStep + 'static> GremlinStep for BufferedStep<T> {
    /// Retrieves the next traverser from the buffer, or produces more if the buffer is empty.
    fn next(&self, ctx: &mut dyn GraphCtx) -> Result<Option<Rc<Traverser>>, StoreError> {
        // One borrow covers the buffer check, the produce call, and the pop.
        // Safety: produce only calls upstream steps (different Rc objects),
        // so their RefCells are independent — no re-entrant borrow can occur.
        let mut inner = self.inner.borrow_mut();
        if inner.buffer.is_empty() {
            let Some(items) = inner.core.produce(ctx)? else { return Ok(None) };
            #[cfg(feature = "tracing")]
            tracing::trace!(target: "rocksgraph::pipeline", "{:?} produced {} items", self, items.len());
            inner.buffer.extend(items);
        }
        Ok(inner.buffer.pop_front())
    }

    fn reset(&self) {
        // Resets the buffer and the wrapped `CoreStep`.
        let mut inner = self.inner.borrow_mut();
        inner.buffer.clear();
        inner.core.reset();
    }

    fn add_upper(&self, upstream: StepRef) {
        // Delegates to the wrapped `CoreStep` to add an upstream.
        self.inner.borrow_mut().core.add_upper(upstream);
    }

    fn upper(&self) -> Option<StepRef> {
        // Delegates to the wrapped `CoreStep` to get the upstream.
        self.inner.borrow().core.upper()
    }

    fn explain(&self) -> ExplainNode {
        self.inner.borrow().core.explain()
    }
}
