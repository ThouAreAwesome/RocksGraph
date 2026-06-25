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

use std::rc::Rc;

use smallvec::SmallVec;
use smol_str::SmolStr;

use crate::types::GValue;

/// The unit of work that flows between steps in a traversal pipeline.
///
/// * `value`: The current data element being traversed (e.g., a vertex, an edge, or a scalar).
/// * `labels`: Holds the `as(...)` labels active at the current step. The traversal engine uses these to build
///   label-to-position maps for `select()` and `path()` evaluation. `None` when no labels are attached. Uses `SmallVec`
///   to stack-allocate up to 2 labels without heap allocation, optimizing for common cases.
/// * `parent` is a back-pointer to the traverser at the previous step, forming a persistent tree (child → parent).
///   Following the chain and collecting `(value, labels)` pairs reconstructs the full traversal history. Allocated only
///   when path tracking is active.
#[derive(Debug, Clone)]
pub struct Traverser {
    /// The current value carried by this traverser.
    pub value: GValue,
    /// Back-pointer to the spawning traverser — `Some` only when path tracking is active.
    pub parent: Option<Rc<Traverser>>,
    /// Labels assigned to the current step via `as(…)`.  `None` = no labels.
    #[allow(dead_code)]
    pub labels: Option<SmallVec<[SmolStr; 2]>>,
}

impl Traverser {
    /// Creates a new `Traverser` with a given value and no parent or labels.
    #[inline]
    pub fn new(value: GValue) -> Self {
        Self { value, labels: None, parent: None }
    }

    /// Creates a new `Traverser` wrapped in an `Rc` with a given value.
    #[inline]
    pub fn new_rc(value: GValue) -> Rc<Self> {
        Rc::new(Self::new(value))
    }

    /// Creates a new traverser. When `track_path` is true, inherits the parent
    /// chain; when false, creates a standalone traverser with no parent back-link
    #[inline]
    pub fn new_rc_conditional(value: GValue, parent: &Rc<Traverser>, track_path: bool) -> Rc<Self> {
        if track_path {
            Rc::new(Self { value, labels: None, parent: Some(Rc::clone(parent)) })
        } else {
            Rc::new(Self { value, labels: None, parent: None })
        }
    }

    /// Collect the full traversal history as `(value, labels)` pairs,
    /// oldest entry first (including the current traverser).
    pub fn collect_path(&self) -> Vec<(GValue, Option<SmallVec<[SmolStr; 2]>>)> {
        let mut entries: Vec<(GValue, Option<SmallVec<[SmolStr; 2]>>)> =
            vec![(self.value.clone(), self.labels.clone())];
        let mut cur = self.parent.as_deref();
        while let Some(ancestor) = cur {
            entries.push((ancestor.value.clone(), ancestor.labels.clone()));
            cur = ancestor.parent.as_deref();
        }
        entries.reverse();
        entries
    }
}
