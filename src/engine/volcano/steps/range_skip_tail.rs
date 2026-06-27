// Physical steps: range(), skip(), tail()

use crate::engine::volcano::steps::traits::ExplainNode;
use crate::types::PIPELINE_PRODUCE_SIZE;
use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::error::StoreError,
};
use smallvec::{smallvec, SmallVec};
use std::rc::Rc;

/// Emits traversers in the half-open range [lo, hi).
#[derive(Debug)]
pub struct RangeStep {
    upstream: Option<StepRef>,
    lo: i64,
    hi: i64,
    index: usize,
}

impl RangeStep {
    pub fn new(lo: i64, hi: i64) -> Self {
        Self { upstream: None, lo, hi, index: 0 }
    }
}

impl CoreStep for RangeStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }
    fn reset(&mut self) {
        self.index = 0;
        if let Some(u) = &self.upstream {
            u.reset();
        }
    }
    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };
            if self.index >= self.hi as usize {
                return Ok(None);
            }
            let emit = self.index >= self.lo as usize;
            self.index += 1;
            if emit {
                return Ok(Some(smallvec![t]));
            }
        }
    }

    fn explain(&self) -> ExplainNode {
        let params = vec![("lo", self.lo.to_string()), ("hi", self.hi.to_string())];
        ExplainNode::new("RangeStep").with_params(params)
    }
}

/// Skips the first n traversers.
#[derive(Debug)]
pub struct SkipStep {
    upstream: Option<StepRef>,
    n: i64,
    skipped: usize,
}

impl SkipStep {
    pub fn new(n: i64) -> Self {
        Self { upstream: None, n, skipped: 0 }
    }
}

impl CoreStep for SkipStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }
    fn reset(&mut self) {
        self.skipped = 0;
        if let Some(u) = &self.upstream {
            u.reset();
        }
    }
    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };
            if self.skipped >= self.n as usize {
                return Ok(Some(smallvec![t]));
            }
            self.skipped += 1;
        }
    }

    fn explain(&self) -> ExplainNode {
        let params = vec![("n", self.n.to_string())];
        ExplainNode::new("SkipStep").with_params(params)
    }
}

/// Collects all traversers, then emits the last n.
#[derive(Debug)]
pub struct TailStep {
    upstream: Option<StepRef>,
    n: i64,
    buffer: Vec<Rc<Traverser>>,
    cursor: usize,
    done: bool,
}

impl TailStep {
    pub fn new(n: i64) -> Self {
        Self { upstream: None, n, buffer: Vec::new(), cursor: 0, done: false }
    }
}

impl CoreStep for TailStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }
    fn reset(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.done = false;
        if let Some(u) = &self.upstream {
            u.reset();
        }
    }
    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        if !self.done {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            while let Some(t) = upstream.next(ctx)? {
                self.buffer.push(t);
            }
            self.done = true;
            let start = if self.buffer.len() >= self.n as usize { self.buffer.len() - self.n as usize } else { 0 };
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

    fn explain(&self) -> ExplainNode {
        let params = vec![("n", self.n.to_string())];
        ExplainNode::new("TailStep").with_params(params)
    }
}
