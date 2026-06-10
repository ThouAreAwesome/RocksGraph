// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of MultiGraph.
//
// Use of this software is governed by the Business Source License 1.1
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date (2030-01-01), in accordance with the Business Source
// License, use of this software will be governed by the Apache License 2.0.
//
// SPDX-License-Identifier: BUSL-1.1

use crate::{
    engine::{context::GraphCtx, traverser::Traverser},
    types::error::StoreError,
};
use smallvec::SmallVec;
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

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
}

// ── CoreStep — what each step author implements ───────────────────────────────

/// The trait step authors implement. `&mut self` is safe here because
/// [`BufferedStep`] wraps every `CoreStep` in a single `RefCell`.
pub trait CoreStep: std::fmt::Debug {
    /// Wire an upstream step. Called once per upstream during plan construction.
    fn add_upper(&mut self, upstream: StepRef);

    /// Pull the next batch of results. Returns `Ok(None)` when exhausted,
    /// `Err` on storage or runtime failure.
    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError>;

    /// Reset all mutable state and propagate to upstreams.
    fn reset(&mut self);

    /// Access the upstream step if one exists. Defaults to None for source steps.
    fn upper(&self) -> Option<StepRef> {
        None
    }
}

// ── BufferedStep — the single generic wrapper ─────────────────────────────────

/// Joint container for a step's core logic and its output buffer.
/// Kept together so [`BufferedStep`] only needs one [`RefCell`] borrow per
/// [`GremlinStep::next`] call.
pub(crate) struct StepInner<T: CoreStep> {
    pub(crate) core: T,
    buffer: VecDeque<Rc<Traverser>>,
}

/// Wraps any [`CoreStep`] and provides the full [`GremlinStep`] interface for
/// free. The buffer drains items produced by `core.produce` one at a time so
/// callers always get exactly one traverser per `next` call.
///
/// A single `RefCell` guards both `core` and `buffer` so each `next` call
/// performs exactly one borrow rather than four.
pub struct BufferedStep<T: CoreStep> {
    pub(crate) inner: RefCell<StepInner<T>>,
}

impl<T: CoreStep + 'static> BufferedStep<T> {
    pub fn new(core: T) -> Rc<Self> {
        Rc::new(Self { inner: RefCell::new(StepInner { core, buffer: VecDeque::new() }) })
    }
}

impl<T: CoreStep + 'static> std::fmt::Debug for BufferedStep<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.borrow().core.fmt(f)
    }
}

impl<T: CoreStep + 'static> GremlinStep for BufferedStep<T> {
    fn next(&self, ctx: &mut dyn GraphCtx) -> Result<Option<Rc<Traverser>>, StoreError> {
        // One borrow covers the buffer check, the produce call, and the pop.
        // Safety: produce only calls upstream steps (different Rc objects),
        // so their RefCells are independent — no re-entrant borrow can occur.
        let mut inner = self.inner.borrow_mut();
        if inner.buffer.is_empty() {
            let Some(items) = inner.core.produce(ctx)? else { return Ok(None) };
            inner.buffer.extend(items);
        }
        Ok(inner.buffer.pop_front())
    }

    fn reset(&self) {
        let mut inner = self.inner.borrow_mut();
        inner.buffer.clear();
        inner.core.reset();
    }

    fn add_upper(&self, upstream: StepRef) {
        self.inner.borrow_mut().core.add_upper(upstream);
    }

    fn upper(&self) -> Option<StepRef> {
        self.inner.borrow().core.upper()
    }
}
