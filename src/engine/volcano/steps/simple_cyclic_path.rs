// Physical steps: simplePath(), cyclicPath()

use crate::types::PIPELINE_BATCH_INLINE;
use std::rc::Rc;
use smallvec::{smallvec, SmallVec};
use crate::{
    engine::{context::GraphCtx, traverser::Traverser, volcano::steps::traits::{CoreStep, StepRef}},
    types::{error::StoreError, gvalue::GValue},
};

/// Filters out traversers whose parent chain contains duplicates — keeps only simple paths.
#[derive(Debug, Default)]
pub struct SimplePathStep {
    upstream: Option<StepRef>,
}

impl CoreStep for SimplePathStep {
    fn add_upper(&mut self, upstream: StepRef) { self.upstream = Some(upstream); }
    fn reset(&mut self) { if let Some(u) = &self.upstream { u.reset(); } }
    fn upper(&self) -> Option<StepRef> { self.upstream.clone() }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };
            if has_duplicate_vertex(&t) { continue; }
            return Ok(Some(smallvec![t]));
        }
    }
}

/// Filters out traversers without duplicate vertices — keeps only cyclic paths.
#[derive(Debug, Default)]
pub struct CyclicPathStep {
    upstream: Option<StepRef>,
}

impl CoreStep for CyclicPathStep {
    fn add_upper(&mut self, upstream: StepRef) { self.upstream = Some(upstream); }
    fn reset(&mut self) { if let Some(u) = &self.upstream { u.reset(); } }
    fn upper(&self) -> Option<StepRef> { self.upstream.clone() }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };
            if !has_duplicate_vertex(&t) { continue; }
            return Ok(Some(smallvec![t]));
        }
    }
}

/// Check whether the traverser's parent chain contains a duplicate vertex id.
fn has_duplicate_vertex(t: &Rc<Traverser>) -> bool {
    let current_id = match &t.value {
        GValue::Vertex(vk) => Some(*vk),
        _ => None,
    };
    let mut seen = std::collections::HashSet::new();
    if let Some(id) = current_id { seen.insert(id); }
    let mut cur = t.parent.as_deref();
    while let Some(ancestor) = cur {
        if let GValue::Vertex(vk) = &ancestor.value {
            if !seen.insert(*vk) { return true; }
        }
        cur = ancestor.parent.as_deref();
    }
    false
}
