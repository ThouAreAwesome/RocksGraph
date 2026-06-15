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

//! [`Label`] — a human-readable vertex or edge label string.
//!
//! Labels are the user-facing names for element types (e.g. `"person"`, `"knows"`).
//! Internally the engine maps each label to a compact [`LabelId`](crate::types::LabelId)
//! (a `u16`) via the schema registry; `Label` is only used at the API boundary where
//! users specify labels by name.
//!
//! `Label` wraps [`SmolStr`], so strings up to 22 bytes are stack-allocated with no
//! heap allocation.

use smol_str::SmolStr;

/// Human-readable label for a vertex or edge (e.g. `"person"`, `"knows"`).
/// Stack-allocated for strings up to 22 bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Label(pub SmolStr);

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
