// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

pub(super) mod admin;
pub(crate) mod cf_options;
pub(crate) mod snapshot;
mod store;
pub(crate) mod transaction;

pub use store::{RocksOptions, RocksStorage};

pub(crate) const CF_VERTICES: &str = "vertices";
pub(crate) const CF_VERTEX_DEGREE: &str = "vertex_degree";
pub(crate) const CF_EDGES_OUT: &str = "edges_out";
pub(crate) const CF_EDGES_IN: &str = "edges_in";
pub(crate) const CF_SCHEMA: &str = "schema";
