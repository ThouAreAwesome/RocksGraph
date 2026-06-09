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

//! MultiGraph — a Gremlin-compatible graph database engine.
//!
//! ## Architecture
//!
//! ```text
//! client  ──►  server  ──►  planner  ──►  optimizer  ──►  engine/{volcano,data_flow}
//!                              │                                     │
//!                         logical IR                            graph / store
//! ```
//!
//! | Module      | Role |
//! |-------------|------|
//! [`planner`]   | Translates a Gremlin AST into engine-agnostic [`LogicalPlan`] IR. |
//! [`optimizer`] | Rewrites a `LogicalPlan` into a more efficient equivalent. |
//! [`engine`]    | Execution engines (`volcano`, `data_flow`) and their shared primitives (`GraphCtx`, `Traverser`,
//! `GroupId`). | [`graph`]     | Query-scoped in-memory overlay over a `GraphStore` transaction. |
//! [`store`]     | Pluggable storage backends (RocksDB, distributed). |
//! [`server`]    | WebSocket/Gremlin server and bytecode deserializer. |
//! [`client`]    | Lightweight Gremlin WebSocket client. |
//! [`schema`]    | Schema definitions and validation. |
//! [`types`]     | Shared value types (`GValue`, `Primitive`, keys). |
//!
//! [`LogicalPlan`]: planner::logical_step::LogicalPlan

pub mod engine;
pub mod graph;
pub mod gremlin;
pub mod optimizer;
pub mod planner;
pub mod schema;
pub mod store;
pub mod types;
