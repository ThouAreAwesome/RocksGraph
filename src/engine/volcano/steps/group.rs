// Physical steps: group(), groupCount()

use crate::types::PIPELINE_BATCH_INLINE;
use std::rc::Rc;
use smallvec::{smallvec, SmallVec};
use crate::{
    engine::{context::GraphCtx, traverser::Traverser, volcano::steps::traits::{CoreStep, StepRef}},
    types::{error::StoreError, gvalue::GValue},
};

/// Collects all traversers and groups them into a Map by value.
#[derive(Debug, Default)]
pub struct GroupStep {
    upstream: Option<StepRef>,
    done: bool,
}

impl CoreStep for GroupStep {
    fn add_upper(&mut self, upstream: StepRef) { self.upstream = Some(upstream); }
    fn reset(&mut self) { self.done = false; if let Some(u) = &self.upstream { u.reset(); } }
    fn upper(&self) -> Option<StepRef> { self.upstream.clone() }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        if self.done { return Ok(None); }
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let mut groups: Vec<(GValue, Vec<GValue>)> = Vec::new();
        while let Some(t) = upstream.next(ctx)? {
            let key = t.value.clone();
            if let Some((_, list)) = groups.iter_mut().find(|(k, _)| k == &key) {
                list.push(t.value.clone());
            } else {
                groups.push((key, vec![t.value.clone()]));
            }
        }
        self.done = true;
        let map = GValue::Map(groups.into_iter().map(|(k, v)| (k, GValue::List(v))).collect());
        Ok(Some(smallvec![Traverser::new_rc(map)]))
    }
}

/// Collects all traversers and counts occurrences per value.
#[derive(Debug, Default)]
pub struct GroupCountStep {
    upstream: Option<StepRef>,
    done: bool,
}

impl CoreStep for GroupCountStep {
    fn add_upper(&mut self, upstream: StepRef) { self.upstream = Some(upstream); }
    fn reset(&mut self) { self.done = false; if let Some(u) = &self.upstream { u.reset(); } }
    fn upper(&self) -> Option<StepRef> { self.upstream.clone() }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        if self.done { return Ok(None); }
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let mut counts: Vec<(GValue, i64)> = Vec::new();
        while let Some(t) = upstream.next(ctx)? {
            if let Some((_, cnt)) = counts.iter_mut().find(|(k, _)| k == &t.value) {
                *cnt += 1;
            } else {
                counts.push((t.value.clone(), 1));
            }
        }
        self.done = true;
        use crate::types::gvalue::Primitive;
        let map = GValue::Map(counts.into_iter().map(|(k, v)| {
            (k, GValue::Scalar(Primitive::Int64(v)))
        }).collect());
        Ok(Some(smallvec![Traverser::new_rc(map)]))
    }
}
