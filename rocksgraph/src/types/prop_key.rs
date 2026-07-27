// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`PropKey`] — the property key type — and the built-in reserved keys.
//!
//! Property keys are plain strings (e.g. `"name"`, `"age"`).  They are represented
//! as [`SmolStr`], which stack-allocates strings up to 23 bytes and avoids heap
//! allocation for the vast majority of real-world keys.
//!
//! # Built-in keys
//!
//! Three keys are reserved and synthesized on-the-fly by [`Vertex`](crate::types::Vertex)
//! and [`Edge`](crate::types::Edge) rather than stored in `props`:
//!
//! - [`ID`] (`"id"`) — the element's numeric identifier ([`VertexKey`](crate::types::VertexKey)).
//!   Vertices only; edges are identified by their composite key instead.
//! - [`LABEL`] (`"label"`) — the element's label as its numeric [`LabelId`](crate::types::LabelId).
//!   Both vertices and edges.
//! - [`RANK`] (`"rank"`) — disambiguates parallel edges with the same label between the
//!   same two vertices (multi-edge mode). Edges only.
//!
//! Querying these keys via `get_property` / `get_value` always succeeds without a
//! `props` scan.

use smol_str::SmolStr;

/// Name of a property key.
///
/// Stack-allocated for strings up to 23 bytes; heap-allocated only for
/// unusually long key names.  No interning or numeric mapping — the raw
/// string is the identity.
pub type PropKey = SmolStr;
pub const ID: PropKey = SmolStr::new_static("id");
pub const LABEL: PropKey = SmolStr::new_static("label");
pub const RANK: PropKey = SmolStr::new_static("rank");

// Property-key and label ids never assign 0 — it's reserved crate-internally to mean "no such
// key/label" (see `schema::definition::MAX_PROP_KEYS`), so real ids start at 1.
pub const ID_KEY_ID: u16 = 1;
pub const LABEL_KEY_ID: u16 = 2;
pub const RANK_KEY_ID: u16 = 3;
