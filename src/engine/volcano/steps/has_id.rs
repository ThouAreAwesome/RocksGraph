// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::types::keys::CanonicalEdgeKey;
use crate::types::PIPELINE_PRODUCE_SIZE;
use std::rc::Rc;

use smallvec::SmallVec;

use crate::engine::volcano::steps::traits::ExplainNode;
use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{
        error::StoreError,
        gvalue::{GValue, Primitive, PrimitivePredicate},
    },
};

/// Pre-parsed edge-id predicate — avoids per-traverser allocation (see §6 of
/// `docs/design_edge_id_string.md`).  Strings in the raw `PrimitivePredicate`
/// are parsed into `CanonicalEdgeKey` once at construction.
#[derive(Debug, Clone)]
enum EdgeIdPredicate {
    Eq(CanonicalEdgeKey),
    Ne(CanonicalEdgeKey),
    Within(Vec<CanonicalEdgeKey>),
    Without(Vec<CanonicalEdgeKey>),
    /// All elements match — used when `Ne`/`Without` operands fail to parse
    /// (nothing equals garbage, so the negation is universally true).
    AlwaysTrue,
}

impl EdgeIdPredicate {
    fn matches(&self, cek: &CanonicalEdgeKey) -> bool {
        match self {
            Self::Eq(k) => cek == k,
            Self::Ne(k) => cek != k,
            Self::Within(ks) => ks.iter().any(|k| cek == k),
            Self::Without(ks) => !ks.iter().any(|k| cek == k),
            Self::AlwaysTrue => true,
        }
    }
}

/// Try to parse string operand(s) of a `PrimitivePredicate` into `CanonicalEdgeKey`.
/// Returns `None` if the predicate has no string operands (vertex-id only).
fn try_parse_edge_pred(pred: &PrimitivePredicate) -> Option<EdgeIdPredicate> {
    fn parse_one(s: &Primitive) -> Option<CanonicalEdgeKey> {
        if let Primitive::String(s) = s {
            s.parse().ok()
        } else {
            None
        }
    }
    match pred {
        PrimitivePredicate::Eq(v) => parse_one(v).map(EdgeIdPredicate::Eq),
        PrimitivePredicate::Ne(v) => {
            // Ne(garbage) = true for all elements — nothing equals garbage.
            Some(parse_one(v).map(EdgeIdPredicate::Ne).unwrap_or(EdgeIdPredicate::AlwaysTrue))
        }
        PrimitivePredicate::Within(vs) => {
            let keys: Vec<CanonicalEdgeKey> = vs.iter().filter_map(parse_one).collect();
            if keys.is_empty() {
                None
            } else {
                Some(EdgeIdPredicate::Within(keys))
            }
        }
        PrimitivePredicate::Without(vs) => {
            let keys: Vec<CanonicalEdgeKey> = vs.iter().filter_map(parse_one).collect();
            if keys.is_empty() {
                Some(EdgeIdPredicate::AlwaysTrue)
            } else {
                Some(EdgeIdPredicate::Without(keys))
            }
        }
        // Gt/Gte/Lt/Lte/Between don't make sense for string edge ids; fall back to None.
        _ => None,
    }
}

/// A physical step that filters traversers based on their vertex ID or edge canonical id.
#[derive(Debug)]
pub struct HasIdStep {
    upstream: Option<StepRef>,
    /// The predicate for vertex-id matching (existing path).
    pred: PrimitivePredicate,
    /// Pre-parsed edge-id predicate — constructed once, matched per traverser
    /// without allocation.  `None` when the predicate has no string operands.
    edge_pred: Option<EdgeIdPredicate>,
}

impl HasIdStep {
    pub fn new(pred: PrimitivePredicate) -> Self {
        let edge_pred = try_parse_edge_pred(&pred);
        Self { upstream: None, pred, edge_pred }
    }
}

impl CoreStep for HasIdStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let mut batch = SmallVec::with_capacity(PIPELINE_PRODUCE_SIZE);
        while batch.len() < PIPELINE_PRODUCE_SIZE {
            let Some(t) = upstream.next(ctx)? else { break };
            match &t.value {
                GValue::Vertex(vk) if self.pred.evaluate(&Primitive::Int64(*vk)) => {
                    batch.push(t);
                }
                GValue::Edge(ek) => {
                    if let Some(edge_pred) = &self.edge_pred {
                        if edge_pred.matches(&ek.canonical_edge_key()) {
                            batch.push(t);
                        }
                    }
                }
                _ => {}
            }
        }
        if batch.is_empty() {
            Ok(None)
        } else {
            Ok(Some(batch))
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn explain(&self) -> ExplainNode {
        let params = vec![("pred", format!("{:?}", self.pred))];
        ExplainNode::new("HasIdStep").with_params(params)
    }
}
