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

use crate::{
    planner::logical_step::{LogicalPlan, LogicalStep},
    types::{prop_key::ID, StoreError},
};

/// Folds `V([]).hasId(N)` or `V([]).has("id", N)` into `V(N)`.
///
/// Two sources produce an id filter immediately after an empty `V`:
/// - `HasIdStep` — from `.hasId(N)` or `.has(Key::Id, N)`.
/// - `HasPropertyStep { key: "id" }` — from `.has("id", N)` where the string
///   `"id"` converts to `Key::Property("id")` rather than `Key::Id`.
///
/// Both cases are handled here so that `V([]).has("id", 42)` gets the same
/// index-seek optimisation as `V([]).hasId(42)`.
pub fn merge_v_id_filter(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut plan_changed = false;
    let mut i = 0;
    let mut j = 1;

    while j < plan.steps.len() {
        let v_ids: Option<smallvec::SmallVec<[i64; 4]>> = match (&plan.steps[i], &plan.steps[j]) {
            (LogicalStep::V(v), LogicalStep::HasProperty(hp)) if hp.key.as_str() == ID && v.ids.is_empty() => {
                super::extract_ids_from_predicate(&hp.pred)?
            }
            (LogicalStep::V(v), LogicalStep::HasId(hi)) if v.ids.is_empty() => {
                super::extract_ids_from_predicate(&hi.pred)?
            }
            _ => None,
        };
        if let Some(ids) = v_ids {
            let LogicalStep::V(v) = &mut plan.steps[i] else {
                unreachable!("should never reach here since we have checked the pattern already")
            };
            v.ids.clear();
            v.ids.extend_from_slice(&ids);
            plan_changed = true;
            j += 1;
        } else {
            i += 1;
            if i != j {
                plan.steps.swap(i, j);
            }
            j += 1;
        }
    }
    plan.steps.truncate(i + 1);
    Ok(plan_changed)
}

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;

    use super::*;
    use crate::{
        planner::logical_step::{HasIdStep, HasPropertyStep, VStep},
        types::{gvalue::Primitive, VertexKey},
    };
    use smallvec::smallvec;

    use crate::types::gvalue::PrimitivePredicate;

    fn v_all() -> LogicalStep {
        LogicalStep::V(VStep { ids: smallvec![] })
    }

    fn v_ids(ids: Vec<VertexKey>) -> LogicalStep {
        LogicalStep::V(VStep { ids: ids.into_iter().collect() })
    }

    fn has_id(ids: Vec<VertexKey>) -> LogicalStep {
        let pred = PrimitivePredicate::Within(ids.into_iter().map(Primitive::Int64).collect());
        LogicalStep::HasId(HasIdStep { pred })
    }

    fn has_id_prop(value: Primitive) -> LogicalStep {
        LogicalStep::HasProperty(HasPropertyStep { key: SmolStr::new("id"), pred: PrimitivePredicate::Eq(value) })
    }

    fn has_prop(key: &str, value: Primitive) -> LogicalStep {
        LogicalStep::HasProperty(HasPropertyStep { key: SmolStr::new(key), pred: PrimitivePredicate::Eq(value) })
    }

    // HasId path

    #[test]
    fn test_hasid_folded_into_v_step() {
        let steps = vec![v_all(), has_id(vec![7])];
        let mut plan = LogicalPlan { steps };
        let opt = merge_v_id_filter(&mut plan).unwrap();
        assert!(opt);
        assert_eq!(plan.steps.len(), 1);
        let LogicalStep::V(v) = &plan.steps[0] else { panic!("expected VStep") };
        assert_eq!(&v.ids[..], &[7]);
    }

    #[test]
    fn test_two_consecutive_hasid_only_first_folds() {
        // V([]).hasId(1).hasId(2): is_empty guard prevents the second fold.
        // Result: V([1]) + HasId([2]).
        let steps = vec![v_all(), has_id(vec![1]), has_id(vec![2])];
        let mut plan = LogicalPlan { steps };
        let opt = merge_v_id_filter(&mut plan).unwrap();
        assert!(opt);
        assert_eq!(plan.steps.len(), 2);
        let LogicalStep::V(v) = &plan.steps[0] else { panic!("expected VStep") };
        assert_eq!(&v.ids[..], &[1i64]);
        let LogicalStep::HasId(hi) = &plan.steps[1] else { panic!("expected HasIdStep") };
        let ids = super::super::extract_ids_from_predicate(&hi.pred).unwrap().unwrap();
        assert_eq!(&ids[..], &[2]);
    }

    // `V([]).hasId([])` must NOT fold into `V([])` — an empty `Within` means "match nothing",
    // but an empty `VStep.ids` means "scan everything" (see `VStep::produce`). Folding here
    // would silently turn "match nothing" into "match everything".
    #[test]
    fn test_hasid_empty_within_not_folded() {
        let steps = vec![v_all(), has_id(vec![])];
        let mut plan = LogicalPlan { steps };
        let opt = merge_v_id_filter(&mut plan).unwrap();
        assert!(!opt);
        assert_eq!(plan.steps.len(), 2);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasId(_)));
    }

    // Same hazard via `.has("id", within([]))`, which takes the HasProperty("id", …) path.
    #[test]
    fn test_id_prop_empty_within_not_folded() {
        let steps = vec![
            v_all(),
            LogicalStep::HasProperty(HasPropertyStep {
                key: SmolStr::new("id"),
                pred: PrimitivePredicate::Within(vec![]),
            }),
        ];
        let mut plan = LogicalPlan { steps };
        let opt = merge_v_id_filter(&mut plan).unwrap();
        assert!(!opt);
        assert_eq!(plan.steps.len(), 2);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasProperty(_)));
    }

    // HasProperty("id", …) path — produced by .has("id", N) where "id" is a &str

    #[test]
    fn test_id_prop_i32_folded_into_v_step() {
        let steps = vec![v_all(), has_id_prop(Primitive::Int32(7))];
        let mut plan = LogicalPlan { steps };
        let opt = merge_v_id_filter(&mut plan).unwrap();
        assert!(opt);
        assert_eq!(plan.steps.len(), 1);
        let LogicalStep::V(v) = &plan.steps[0] else { panic!("expected VStep") };
        assert_eq!(&v.ids[..], &[7]);
    }

    #[test]
    fn test_id_prop_i64_folded_into_v_step() {
        let steps = vec![v_all(), has_id_prop(Primitive::Int64(42))];
        let mut plan = LogicalPlan { steps };
        let opt = merge_v_id_filter(&mut plan).unwrap();
        assert!(opt);
        assert_eq!(plan.steps.len(), 1);
        let LogicalStep::V(v) = &plan.steps[0] else { panic!("expected VStep") };
        assert_eq!(&v.ids[..], &[42i64]);
    }

    #[test]
    fn test_id_prop_not_folded_when_v_already_seeded() {
        let steps = vec![v_ids(vec![2]), has_id_prop(Primitive::Int32(3))];
        let mut plan = LogicalPlan { steps };
        let opt = merge_v_id_filter(&mut plan).unwrap();
        assert!(!opt);
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn test_id_prop_non_int_returns_error() {
        let steps = vec![v_all(), has_id_prop(Primitive::String(SmolStr::new("abc")))];
        let mut plan = LogicalPlan { steps };
        assert!(merge_v_id_filter(&mut plan).is_err());
    }

    // `.has("id", gt(5))` is a valid, well-typed query that simply isn't an id-allowlist shape —
    // it must be left unfolded (like the analogous `.hasId(gt(5))` already was), not hard-error
    // the whole plan just because this rule can't fold it.
    #[test]
    fn test_id_prop_non_foldable_shape_not_folded_no_error() {
        let steps = vec![
            v_all(),
            LogicalStep::HasProperty(HasPropertyStep {
                key: SmolStr::new("id"),
                pred: PrimitivePredicate::Gt(Primitive::Int64(5)),
            }),
        ];
        let mut plan = LogicalPlan { steps };
        let opt = merge_v_id_filter(&mut plan).unwrap();
        assert!(!opt);
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn test_id_prop_trailing_steps_preserved() {
        let steps =
            vec![v_all(), has_id_prop(Primitive::Int32(3)), has_prop("name", Primitive::String(SmolStr::new("lop")))];
        let mut plan = LogicalPlan { steps };
        let opt = merge_v_id_filter(&mut plan).unwrap();
        assert!(opt);
        assert_eq!(plan.steps.len(), 2);
        let LogicalStep::V(v) = &plan.steps[0] else { panic!("expected VStep") };
        assert_eq!(&v.ids[..], &[3]);
        assert!(matches!(plan.steps[1], LogicalStep::HasProperty(_)));
    }

    // Non-id properties are never folded

    #[test]
    fn test_non_id_has_not_folded() {
        let steps = vec![v_all(), has_prop("name", Primitive::String(SmolStr::new("marko")))];
        let mut plan = LogicalPlan { steps };
        let opt = merge_v_id_filter(&mut plan).unwrap();
        assert!(!opt);
        assert_eq!(plan.steps.len(), 2);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasProperty(_)));
    }
}
