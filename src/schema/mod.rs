// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

pub(crate) mod definition;
pub(crate) mod management;

#[cfg(test)]
#[cfg(test)]
mod tests;

// Public surface: only what callers need to configure a `Graph` (`GraphOptions` and friends) and
// to declare schema via `SchemaManagement`. `Schema` itself (the live registry) and
// `PropKeyConfig` (one of its internal fields) are crate-internal — see `Graph::schema()`.
pub use definition::{DataType, EdgeMode, GraphOptions, SchemaMode};
pub use management::SchemaManagement;

pub(crate) use definition::Schema;
