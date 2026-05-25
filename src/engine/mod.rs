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

//! Execution engine and shared runtime primitives.
//!
//! ## Submodules
//!
//! | Submodule    | Role |
//! |--------------|------|
//! | [`context`]  | [`GraphCtx`] trait — the graph-access interface passed to every step at runtime. Shared by both engines. |
//! | [`group_id`] | [`GroupId`] — hierarchical group identity carried on every [`Traverser`]. Used by both engines for `where`/co-group correlation. |
//! | [`traverser`]| [`Traverser`] — the unit of work flowing between steps. |
//! | [`volcano`]  | Pull-based iterator execution engine. Logical steps are compiled to a chain of physical operators by [`volcano::builder::PhysicalPlanBuilder`]. |
//!
//! [`GraphCtx`]: context::GraphCtx
//! [`GroupId`]: group_id::GroupId
//! [`Traverser`]: traverser::Traverser

pub mod context;
pub mod traverser;
pub mod volcano;

pub use traverser::Traverser;
