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

use crate::{
    engine::logical_step::{LogicalStep, VStep},
    types::{gvalue::Primitive, keys::VertexKey},
};

/// Folds `V().has("id", N)` into `V(N)`, removing the redundant property scan.
///
/// "id" is a structural key stored in the index, not in property storage. A bare
/// `HasPropertyStep` would never match it, so we must convert the filter into an
/// explicit seed ID on `VStep` where the storage layer can resolve it directly.
pub(super) fn push_down_id_filter(steps: Vec<LogicalStep>) -> Vec<LogicalStep> {
    let mut out = Vec::with_capacity(steps.len());
    let mut iter = steps.into_iter().peekable();

    while let Some(step) = iter.next() {
        let is_id_filter = if let LogicalStep::V(v) = &step {
            v.ids.is_empty()
                && matches!(
                    iter.peek(),
                    Some(LogicalStep::HasProperty(hp))
                        if hp.key.as_str() == "id" && matches!(hp.value, Primitive::Int32(_))
                )
        } else {
            false
        };

        if is_id_filter {
            if let Some(LogicalStep::HasProperty(hp)) = iter.next() {
                if let Primitive::Int32(id) = hp.value {
                    out.push(LogicalStep::V(VStep { ids: vec![id as VertexKey] }));
                    continue;
                }
            }
        }

        out.push(step);
    }

    out
}

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;

    use super::*;
    use crate::engine::logical_step::{HasPropertyStep, VStep};
    use crate::types::gvalue::Primitive;

    fn v_all() -> LogicalStep {
        LogicalStep::V(VStep { ids: vec![] })
    }

    fn v_ids(ids: Vec<VertexKey>) -> LogicalStep {
        LogicalStep::V(VStep { ids })
    }

    fn has(key: &str, value: Primitive) -> LogicalStep {
        LogicalStep::HasProperty(HasPropertyStep { key: SmolStr::new(key), value })
    }

    #[test]
    fn test_id_filter_folded_into_v_step() {
        let steps = vec![v_all(), has("id", Primitive::Int32(7))];
        let opt = push_down_id_filter(steps);
        assert_eq!(opt.len(), 1);
        if let LogicalStep::V(v) = &opt[0] {
            assert_eq!(v.ids, vec![7]);
        } else {
            panic!("expected VStep");
        }
    }

    #[test]
    fn test_non_id_has_not_folded() {
        let steps = vec![v_all(), has("name", Primitive::String(SmolStr::new("marko")))];
        let opt = push_down_id_filter(steps);
        assert_eq!(opt.len(), 2);
        assert!(matches!(opt[0], LogicalStep::V(_)));
        assert!(matches!(opt[1], LogicalStep::HasProperty(_)));
    }

    #[test]
    fn test_v_with_explicit_ids_not_rewritten() {
        let steps = vec![v_ids(vec![2]), has("id", Primitive::Int32(2))];
        let opt = push_down_id_filter(steps);
        assert_eq!(opt.len(), 2, "V with existing IDs should not absorb another id filter");
    }

    #[test]
    fn test_id_filter_with_non_int_value_not_folded() {
        let steps = vec![v_all(), has("id", Primitive::String(SmolStr::new("abc")))];
        let opt = push_down_id_filter(steps);
        assert_eq!(opt.len(), 2);
    }

    #[test]
    fn test_trailing_steps_preserved() {
        let steps = vec![v_all(), has("id", Primitive::Int32(3)), has("name", Primitive::String(SmolStr::new("lop")))];
        let opt = push_down_id_filter(steps);
        assert_eq!(opt.len(), 2);
        if let LogicalStep::V(v) = &opt[0] {
            assert_eq!(v.ids, vec![3]);
        } else {
            panic!("expected VStep");
        }
        assert!(matches!(opt[1], LogicalStep::HasProperty(_)));
    }
}
