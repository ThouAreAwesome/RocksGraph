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
    types::{gvalue::Primitive, prop_key::ID, StoreError},
};
use smallvec::smallvec;

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
        let v_ids = match (&plan.steps[i], &plan.steps[j]) {
            (LogicalStep::V(v), LogicalStep::HasProperty(hp)) if hp.key.as_str() == ID && v.ids.is_empty() => {
                match hp.value {
                    Primitive::Int64(id) => Some(smallvec![id]),
                    Primitive::Int32(id) => Some(smallvec![id as i64]),
                    _ => return Err(StoreError::UnexpectedDataType("expect i32 or i64 type for vertex id".into())),
                }
            }
            (LogicalStep::V(v), LogicalStep::HasId(hi)) if v.ids.is_empty() => Some(hi.ids.clone()),
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

    fn v_all() -> LogicalStep {
        LogicalStep::V(VStep { ids: smallvec![] })
    }

    fn v_ids(ids: Vec<VertexKey>) -> LogicalStep {
        LogicalStep::V(VStep { ids: ids.into_iter().collect() })
    }

    fn has_id(ids: Vec<VertexKey>) -> LogicalStep {
        LogicalStep::HasId(HasIdStep { ids: ids.into_iter().collect() })
    }

    fn has_id_prop(value: Primitive) -> LogicalStep {
        LogicalStep::HasProperty(HasPropertyStep { key: SmolStr::new("id"), value })
    }

    fn has_prop(key: &str, value: Primitive) -> LogicalStep {
        LogicalStep::HasProperty(HasPropertyStep { key: SmolStr::new(key), value })
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
        let LogicalStep::HasId(HasIdStep { ids }) = &plan.steps[1] else { panic!("expected HasIdStep") };
        assert_eq!(&ids[..], &[2]);
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
