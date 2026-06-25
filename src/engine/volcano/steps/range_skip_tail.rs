// Physical steps: range(), skip(), tail()

use crate::types::PIPELINE_BATCH_INLINE;
use std::rc::Rc;
use smallvec::{smallvec, SmallVec};
use crate::{
    engine::{context::GraphCtx, traverser::Traverser, volcano::steps::traits::{CoreStep, StepRef}},
    types::error::StoreError,
};

/// Emits traversers in the half-open range [lo, hi).
#[derive(Debug)]
pub struct RangeStep {
    upstream: Option<StepRef>,
    lo: u64,
    hi: u64,
    index: u64,
}

impl RangeStep {
    pub fn new(lo: u64, hi: u64) -> Self { Self { upstream: None, lo, hi, index: 0 } }
}

impl CoreStep for RangeStep {
    fn add_upper(&mut self, upstream: StepRef) { self.upstream = Some(upstream); }
    fn reset(&mut self) { self.index = 0; if let Some(u) = &self.upstream { u.reset(); } }
    fn upper(&self) -> Option<StepRef> { self.upstream.clone() }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };
            if self.index >= self.hi { return Ok(None); }
            let emit = self.index >= self.lo;
            self.index += 1;
            if emit { return Ok(Some(smallvec![t])); }
        }
    }
}

/// Skips the first n traversers.
#[derive(Debug)]
pub struct SkipStep {
    upstream: Option<StepRef>,
    n: u64,
    skipped: u64,
}

impl SkipStep {
    pub fn new(n: u64) -> Self { Self { upstream: None, n, skipped: 0 } }
}

impl CoreStep for SkipStep {
    fn add_upper(&mut self, upstream: StepRef) { self.upstream = Some(upstream); }
    fn reset(&mut self) { self.skipped = 0; if let Some(u) = &self.upstream { u.reset(); } }
    fn upper(&self) -> Option<StepRef> { self.upstream.clone() }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };
            if self.skipped >= self.n { return Ok(Some(smallvec![t])); }
            self.skipped += 1;
        }
    }
}

/// Collects all traversers, then emits the last n.
#[derive(Debug)]
pub struct TailStep {
    upstream: Option<StepRef>,
    n: u64,
    buffer: Vec<Rc<Traverser>>,
    cursor: usize,
    done: bool,
}

impl TailStep {
    pub fn new(n: u64) -> Self { Self { upstream: None, n, buffer: Vec::new(), cursor: 0, done: false } }
}

impl CoreStep for TailStep {
    fn add_upper(&mut self, upstream: StepRef) { self.upstream = Some(upstream); }
    fn reset(&mut self) { self.buffer.clear(); self.cursor = 0; self.done = false; if let Some(u) = &self.upstream { u.reset(); } }
    fn upper(&self) -> Option<StepRef> { self.upstream.clone() }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        if !self.done {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            while let Some(t) = upstream.next(ctx)? { self.buffer.push(t); }
            self.done = true;
            let start = if self.buffer.len() as u64 >= self.n { self.buffer.len() - self.n as usize } else { 0 };
            self.cursor = start;
        }
        if self.cursor < self.buffer.len() {
            let t = Rc::clone(&self.buffer[self.cursor]);
            self.cursor += 1;
            Ok(Some(smallvec![t]))
        } else {
            Ok(None)
        }
    }
}
