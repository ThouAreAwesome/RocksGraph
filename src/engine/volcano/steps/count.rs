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

use smallvec::{smallvec, SmallVec};

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{
        error::StoreError,
        gvalue::{GValue, Primitive},
    },
};

#[derive(Default, Debug)]
pub struct CountStep {
    upstream: Option<StepRef>,
    done: bool,
}

impl CoreStep for CountStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        if self.done {
            return Ok(None);
        }
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let mut count: u64 = 0;
        while upstream.next(ctx)?.is_some() {
            count += 1;
        }
        self.done = true;
        Ok(Some(smallvec![Traverser::new_rc(GValue::Scalar(Primitive::Int64(count as i64)))]))
    }

    fn reset(&mut self) {
        self.done = false;
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }
}
