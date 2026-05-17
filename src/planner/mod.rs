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

//! Compilation front-end: Gremlin AST → engine-agnostic logical IR.
//!
//! This module is engine-agnostic — it has no dependency on `engine::volcano`
//! or `engine::data_flow`. Both execution engines consume the same
//! [`logical_step::LogicalPlan`] produced here.
//!
//! ## Pipeline position
//!
//! ```text
//! server::bytecode_deserializer  (parse JSON → GremlinQueryAst)
//!          │
//!          ▼
//! planner::gremlin_to_logical_plan  (translate → LogicalPlan)
//!          │
//!          ▼
//! optimizer::optimize               (rewrite → LogicalPlan)
//!          │
//!          ▼
//! engine::volcano::builder          (compile → PhysicalPlan)
//! ```

pub mod gremlin_to_logical_plan;
pub mod logical_step;
