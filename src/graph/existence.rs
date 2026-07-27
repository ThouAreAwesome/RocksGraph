// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

// ── Existence ────────────────────────────────────────────────────────────────
//
/// Mutation kind for a dirty graph element within a `LogicalGraph`.
///
/// Only dirty elements appear in the `dirty` map; absence means `Clean`.
///
/// **Note**: How to handle delete -> add on the same element within a single query?
/// This is currently treated as `New`, but it might be beneficial to distinguish it
///     from a pure create for better conflict detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Existence {
    /// Props were mutated on an existing element.
    Modified,
    /// Only the vertex edge counts changed.
    CounterOnly,
    /// Props and vertex edge counts both changed.
    ModifiedWithCounter,
    /// Created in this query; not yet persisted.
    New,
    /// Deleted in this query.
    Tombstone,
}

impl Existence {
    /// Merges two dirty states for the same element within a single transaction.
    ///
    /// This defines the state machine for consecutive operations. For example:
    /// - Any operation followed by a deletion (`Tombstone`) results in a `Tombstone`.
    /// - Modifying properties (`Modified`) and changing edge counts (`CounterOnly`) combines into
    ///   `ModifiedWithCounter`.
    #[inline]
    pub(crate) fn merge(self, other: Existence) -> Existence {
        use Existence::*;
        match (self, other) {
            (Tombstone, _) | (_, Tombstone) => Tombstone,
            (New, _) | (_, New) => New,
            (ModifiedWithCounter, _) | (_, ModifiedWithCounter) => ModifiedWithCounter,
            (Modified, CounterOnly) | (CounterOnly, Modified) => ModifiedWithCounter,
            (Modified, Modified) => Modified,
            (CounterOnly, CounterOnly) => CounterOnly,
        }
    }
}
