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

//! Logical plan rewriter — transforms a [`LogicalPlan`] into a semantically
//! equivalent but more efficient form before physical planning.
//!
//! Each rewrite rule lives in its own submodule. [`optimize`] applies them in
//! order; adding a new rule means creating a new file and one extra call here.
//!
//! ## Current rules
//!
//! | Rule | File | Effect |
//! |------|------|--------|
//! | ID filter pushdown | [`push_down_id_filter`] | `V().has("id", N)` → `V(N)` |
//!
//! [`LogicalPlan`]: crate::planner::logical_step::LogicalPlan

mod push_down_id_filter;

use crate::planner::logical_step::LogicalPlan;
use push_down_id_filter::push_down_id_filter;

/// Rewrites a `LogicalPlan` into a more efficient equivalent before physical planning.
pub fn optimize(plan: LogicalPlan) -> LogicalPlan {
    LogicalPlan { steps: push_down_id_filter(plan.steps) }
}
