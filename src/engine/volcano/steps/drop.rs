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

use std::rc::Rc;

use std::collections::VecDeque;

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{error::StoreError, GValue},
};

#[derive(Default)]
pub struct DropStep {
    upstream: Option<StepRef>,
}

impl CoreStep for DropStep {
    /// Wire an upstream step. Called once per upstream during plan construction.
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx, _buffer: &mut VecDeque<Rc<Traverser>>) -> Result<bool, StoreError> {
        let Some(up) = self.upstream.as_deref() else { return Ok(false) };
        while let Some(el) = up.next(ctx)? {
            match &el.value {
                GValue::Property(pp) => ctx.drop_property(pp)?,
                GValue::Vertex(vt) => ctx.drop_vertex(*vt)?,
                GValue::Edge(eg) => ctx.drop_edge(eg)?,
                _ => {
                    return Err(StoreError::UnexpectedDataType("unexpected data type for drop step".into()));
                }
            }
        }
        Ok(false)
    }

    /// Reset all mutable state and propagate to upstreams.
    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }
}
