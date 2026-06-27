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

use crate::types::PIPELINE_PRODUCE_SIZE;
use std::rc::Rc;

use smallvec::{smallvec, SmallVec};

use crate::engine::volcano::steps::traits::ExplainNode;
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

/// Helper: extract an `f64` from a `GValue` if it is numeric, else `None`.
fn to_numeric(val: &GValue) -> Option<f64> {
    match val {
        GValue::Scalar(p) => p.to_f64(),
        _ => None,
    }
}

// ── SumStep ──────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct SumStep {
    upstream: Option<StepRef>,
    done: bool,
}

impl CoreStep for SumStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        if self.done {
            return Ok(None);
        }
        let Some(upstream) = self.upstream.as_ref() else {
            return Ok(None);
        };

        let mut has_float = false;
        let mut sum_int: i64 = 0;
        let mut sum_float: f64 = 0.0;
        let mut count: i64 = 0;

        while let Some(t) = upstream.next(ctx)? {
            if let GValue::Scalar(p) = &t.value {
                if let Some(f) = p.to_f64() {
                    if p.is_integer() && !has_float {
                        sum_int = sum_int.wrapping_add(p.to_i64().unwrap_or(0));
                    } else {
                        if !has_float {
                            has_float = true;
                            sum_float = sum_int as f64;
                        }
                        sum_float += f;
                    }
                    count += 1;
                }
            }
        }

        self.done = true;
        let result = if count == 0 {
            Primitive::Null
        } else if has_float {
            Primitive::Float64(sum_float)
        } else {
            Primitive::Int64(sum_int)
        };
        Ok(Some(smallvec![Traverser::new_rc(GValue::Scalar(result))]))
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

    fn explain(&self) -> ExplainNode {
        ExplainNode::new("SumStep")
    }
}

// ── MeanStep ─────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct MeanStep {
    upstream: Option<StepRef>,
    done: bool,
}

impl CoreStep for MeanStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        if self.done {
            return Ok(None);
        }
        let Some(upstream) = self.upstream.as_ref() else {
            return Ok(None);
        };

        let mut sum: f64 = 0.0;
        let mut count: i64 = 0;

        while let Some(t) = upstream.next(ctx)? {
            if let Some(f) = to_numeric(&t.value) {
                sum += f;
                count += 1;
            }
        }

        self.done = true;
        let result = if count == 0 { Primitive::Null } else { Primitive::Float64(sum / count as f64) };
        Ok(Some(smallvec![Traverser::new_rc(GValue::Scalar(result))]))
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

    fn explain(&self) -> ExplainNode {
        ExplainNode::new("MeanStep")
    }
}

// ── MaxStep ──────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct MaxStep {
    upstream: Option<StepRef>,
    done: bool,
}

impl CoreStep for MaxStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        if self.done {
            return Ok(None);
        }
        let Some(upstream) = self.upstream.as_ref() else {
            return Ok(None);
        };

        let mut has_float = false;
        let mut best_int: Option<i64> = None;
        let mut best_float: Option<f64> = None;

        while let Some(t) = upstream.next(ctx)? {
            if let GValue::Scalar(p) = &t.value {
                if p.is_integer() && !has_float {
                    let v = p.to_i64().unwrap();
                    best_int = Some(match best_int {
                        None => v,
                        Some(b) => b.max(v),
                    });
                } else if let Some(f) = p.to_f64() {
                    if !has_float {
                        has_float = true;
                        best_float = best_int.map(|i| i as f64);
                    }
                    best_float = Some(match best_float {
                        None => f,
                        Some(b) => {
                            if f > b {
                                f
                            } else {
                                b
                            }
                        }
                    });
                }
            }
        }

        self.done = true;
        let result = if has_float {
            match best_float {
                Some(f) => Primitive::Float64(f),
                None => Primitive::Null,
            }
        } else {
            match best_int {
                Some(i) => Primitive::Int64(i),
                None => Primitive::Null,
            }
        };
        Ok(Some(smallvec![Traverser::new_rc(GValue::Scalar(result))]))
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

    fn explain(&self) -> ExplainNode {
        ExplainNode::new("MaxStep")
    }
}

// ── MinStep ──────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct MinStep {
    upstream: Option<StepRef>,
    done: bool,
}

impl CoreStep for MinStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        if self.done {
            return Ok(None);
        }
        let Some(upstream) = self.upstream.as_ref() else {
            return Ok(None);
        };

        let mut has_float = false;
        let mut best_int: Option<i64> = None;
        let mut best_float: Option<f64> = None;

        while let Some(t) = upstream.next(ctx)? {
            if let GValue::Scalar(p) = &t.value {
                if p.is_integer() && !has_float {
                    let v = p.to_i64().unwrap();
                    best_int = Some(match best_int {
                        None => v,
                        Some(b) => b.min(v),
                    });
                } else if let Some(f) = p.to_f64() {
                    if !has_float {
                        has_float = true;
                        best_float = best_int.map(|i| i as f64);
                    }
                    best_float = Some(match best_float {
                        None => f,
                        Some(b) => {
                            if f < b {
                                f
                            } else {
                                b
                            }
                        }
                    });
                }
            }
        }

        self.done = true;
        let result = if has_float {
            match best_float {
                Some(f) => Primitive::Float64(f),
                None => Primitive::Null,
            }
        } else {
            match best_int {
                Some(i) => Primitive::Int64(i),
                None => Primitive::Null,
            }
        };
        Ok(Some(smallvec![Traverser::new_rc(GValue::Scalar(result))]))
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

    fn explain(&self) -> ExplainNode {
        ExplainNode::new("MinStep")
    }
}
