// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`Label`] — a human-readable vertex or edge label string.
//!
//! Labels are the user-facing names for element types (e.g. `"person"`, `"knows"`).
//! Internally the engine maps each label to a compact [`LabelId`](crate::types::LabelId)
//! (an `i32`) via the schema registry; `Label` is only used at the API boundary where
//! users specify labels by name.
//!
//! `Label` wraps [`SmolStr`], so strings up to 23 bytes are stack-allocated with no
//! heap allocation.

use smol_str::SmolStr;

/// Human-readable label for a vertex or edge (e.g. `"person"`, `"knows"`).
/// Stack-allocated for strings up to 23 bytes.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Label(pub SmolStr);

#[allow(dead_code)]
impl Label {
    pub fn new(s: impl Into<SmolStr>) -> Self {
        Self(s.into())
    }
}

impl From<&str> for Label {
    fn from(s: &str) -> Self {
        Self(SmolStr::new(s))
    }
}
