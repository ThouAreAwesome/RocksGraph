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

/// Folds `V().has("id", N)` into `V(N)`, removing the redundant property scan.
///
/// "id" is a structural key stored in the index, not in property storage. A bare
/// `HasPropertyStep` would never match it, so we must convert the filter into an
/// explicit seed ID on `VStep` where the storage layer can resolve it directly.
pub fn merget_v_id_filter(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut plan_changed = false;
    let mut i = 0; // current index of the last non-merged step
    let mut j = 1; // next step to consider for merging

    while j < plan.steps.len() {
        let v_ids = match (&plan.steps[i], &plan.steps[j]) {
            (LogicalStep::V(v), LogicalStep::HasProperty(hp)) if hp.key.as_str() == ID && v.ids.is_empty() => {
                match hp.value {
                    Primitive::Int64(id) => Some(smallvec![id]),
                    Primitive::Int32(id) => Some(smallvec![id as i64]),
                    _ => None,
                }
            }
            (LogicalStep::V(_), LogicalStep::HasId(hi)) => Some(hi.ids.clone()),
            _ => None,
        };
        if let Some(ids) = v_ids {
            let LogicalStep::V(v) = &mut plan.steps[i] else {
                unreachable!("should never reach here since we have checked the pattern already")
            };
            // merge the id filter into the V step
            v.ids.clear();
            v.ids.extend_from_slice(&ids);
            plan_changed = true;
            j += 1; // skip the merged HasProperty step. no need to remove the steps[j]
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
        planner::logical_step::{HasPropertyStep, VStep},
        types::{gvalue::Primitive, VertexKey},
    };
    use smallvec::smallvec;

    fn v_all() -> LogicalStep {
        LogicalStep::V(VStep { ids: smallvec![] })
    }

    fn v_ids(ids: Vec<VertexKey>) -> LogicalStep {
        LogicalStep::V(VStep { ids: ids.into_iter().collect() })
    }

    fn has(key: &str, value: Primitive) -> LogicalStep {
        LogicalStep::HasProperty(HasPropertyStep { key: SmolStr::new(key), value })
    }

    fn has_id(ids: Vec<VertexKey>) -> LogicalStep {
        LogicalStep::HasId(crate::planner::logical_step::HasIdStep { ids: ids.into_iter().collect() })
    }

    #[test]
    fn test_ids_filter_folded_into_v_step() {
        let steps = vec![v_all(), has_id(vec![7])];
        let mut plan = LogicalPlan { steps };
        let opt = merget_v_id_filter(&mut plan).unwrap();
        assert!(opt, "plan should be changed");
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::V(v) = &plan.steps[0] {
            assert_eq!(&v.ids[..], &[7]);
        } else {
            panic!("expected VStep");
        }
    }

    #[test]
    fn test_id_filter_folded_into_v_step() {
        let steps = vec![v_all(), has("id", Primitive::Int32(7))];
        let mut plan = LogicalPlan { steps };
        let opt = merget_v_id_filter(&mut plan).unwrap();
        assert!(opt, "plan should be changed");
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::V(v) = &plan.steps[0] {
            assert_eq!(&v.ids[..], &[7]);
        } else {
            panic!("expected VStep");
        }
    }

    #[test]
    fn test_non_id_has_not_folded() {
        let steps = vec![v_all(), has("name", Primitive::String(SmolStr::new("marko")))];
        let mut plan = LogicalPlan { steps };
        let opt = merget_v_id_filter(&mut plan).unwrap();
        assert!(!opt, "plan should not be changed");
        assert_eq!(plan.steps.len(), 2);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasProperty(_)));
    }

    #[test]
    fn test_v_with_explicit_ids_should_be_optimized() {
        let steps = vec![v_ids(vec![2]), has("id", Primitive::Int32(3))];
        let mut plan = LogicalPlan { steps };
        let opt = merget_v_id_filter(&mut plan).unwrap();
        assert!(!opt, "plan should not be changed");
        assert_eq!(plan.steps.len(), 2);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        if let LogicalStep::V(v) = &plan.steps[0] {
            assert_eq!(&v.ids[..], &[2]);
        } else {
            panic!("expected VStep");
        }
        if let LogicalStep::HasProperty(hp) = &plan.steps[1] {
            assert_eq!(hp.value, Primitive::Int32(3));
        } else {
            panic!("expected VStep");
        }
    }

    #[test]
    fn test_id_filter_with_non_int_value_not_folded() {
        let steps = vec![v_all(), has("id", Primitive::String(SmolStr::new("abc")))];
        let mut plan = LogicalPlan { steps };
        let opt = merget_v_id_filter(&mut plan).unwrap();
        assert!(!opt, "plan should not be changed");
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn test_trailing_steps_preserved() {
        let steps = vec![v_all(), has("id", Primitive::Int32(3)), has("name", Primitive::String(SmolStr::new("lop")))];
        let mut plan = LogicalPlan { steps };
        let opt = merget_v_id_filter(&mut plan).unwrap();
        assert!(opt, "plan should be changed");
        assert_eq!(plan.steps.len(), 2);
        if let LogicalStep::V(v) = &plan.steps[0] {
            assert_eq!(&v.ids[..], &[3]);
        } else {
            panic!("expected VStep");
        }
        assert!(matches!(plan.steps[1], LogicalStep::HasProperty(_)));
    }

    #[test]
    fn test_id_filter_int64_folded_into_v_step() {
        let steps = vec![v_all(), has("id", Primitive::Int64(42))];
        let mut plan = LogicalPlan { steps };
        let opt = merget_v_id_filter(&mut plan).unwrap();
        assert!(opt, "plan should be changed");
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::V(v) = &plan.steps[0] {
            assert_eq!(&v.ids[..], &[42i64]);
        } else {
            panic!("expected VStep");
        }
    }

    #[test]
    fn test_two_consecutive_has_id_second_wins() {
        // V().hasId(1).hasId(2): the HasId arm has no is_empty guard, so both fold in
        // sequence — the second value overwrites the first. Final plan: V(2), 1 step.
        let steps = vec![v_all(), has_id(vec![1]), has_id(vec![2])];
        let mut plan = LogicalPlan { steps };
        let opt = merget_v_id_filter(&mut plan).unwrap();
        assert!(opt, "plan should be changed");
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::V(v) = &plan.steps[0] {
            assert_eq!(&v.ids[..], &[2i64], "second hasId overwrites the first");
        } else {
            panic!("expected VStep");
        }
    }
}
