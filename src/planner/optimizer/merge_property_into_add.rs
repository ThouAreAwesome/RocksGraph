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
    planner::logical_step::{LogicalPlan, LogicalStep, PropertyStep},
    types::{
        error::StoreError,
        gvalue::Primitive,
        keys::Rank,
        prop_key::{PropKey, ID, LABEL, RANK},
    },
};

use super::primitive_to_rank;

/// Folds chained `property("key", value)` into an adjacent `addV()` or `addE()`,
/// eliminating the separate `Property` pipeline steps.
///
/// Reserved-key dispatch:
/// - `id` on `addV`   → `AddVStep.vertex_id`
/// - `rank` on `addE` → `AddEStep.rank`
/// - `label` / `rank` on `addV` → rejected
/// - anything else → `{AddV,AddE}Step.properties` (SmallVec, appended in order)
///
/// Because the rule stays at position `i` after each merge (it never advances
/// past the anchor step), properties in arbitrary order are all handled in a
/// single pass.
pub fn merge_property_into_add(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut plan_changed = false;
    let mut i = 0;
    let mut j = 1;
    while j < plan.steps.len() {
        enum Action {
            Prop { key: PropKey, value: Primitive },
            Rank(Rank),
            Id(i64),
        }

        let action = match (&plan.steps[i], &plan.steps[j]) {
            (LogicalStep::AddV(_), LogicalStep::Property(PropertyStep { prop_key, prop_value })) if ID == *prop_key => {
                let id = match prop_value {
                    Primitive::Int32(v) => *v as i64,
                    Primitive::Int64(v) => *v,
                    _ => return Err(StoreError::UnexpectedDataType("only i32 and i64 can be vertex id".into())),
                };
                Some(Action::Id(id))
            }
            (LogicalStep::AddE(_), LogicalStep::Property(PropertyStep { prop_key, prop_value }))
                if RANK == *prop_key =>
            {
                Some(Action::Rank(primitive_to_rank(prop_value)?))
            }
            (
                LogicalStep::AddV(_) | LogicalStep::AddE(_),
                LogicalStep::Property(PropertyStep { prop_key, prop_value }),
            ) => {
                if LABEL == *prop_key || RANK == *prop_key {
                    return Err(StoreError::SchemaViolation(format!(
                        "cannot set property with reserved key '{}' on addV",
                        prop_key
                    )));
                }
                Some(Action::Prop { key: prop_key.clone(), value: prop_value.clone() })
            }
            _ => None,
        };

        if let Some(action) = action {
            match &mut plan.steps[i] {
                LogicalStep::AddV(av) => match action {
                    Action::Id(id) => {
                        if av.vertex_id.is_some() {
                            return Err(StoreError::UnsupportedOperation(
                                "cannot assign vertex id several times".into(),
                            ));
                        }
                        av.vertex_id = Some(id);
                    }
                    Action::Rank(_) => {
                        return Err(StoreError::SchemaViolation("rank is not a valid property for addV".into()));
                    }
                    Action::Prop { key, value } => {
                        if av.properties.iter().any(|(k, _)| k == &key) {
                            return Err(StoreError::UnsupportedOperation(format!(
                                "cannot assign property '{}' several times",
                                key
                            )));
                        }
                        av.properties.push((key, value));
                    }
                },
                LogicalStep::AddE(ae) => match action {
                    Action::Id(_) => unreachable!("id is only matched for AddV"),
                    Action::Rank(r) => {
                        if ae.rank.is_some() {
                            return Err(StoreError::UnsupportedOperation(
                                "cannot assign edge rank several times".into(),
                            ));
                        }
                        ae.rank = Some(r);
                    }
                    Action::Prop { key, value } => {
                        if ae.properties.iter().any(|(k, _)| k == &key) {
                            return Err(StoreError::UnsupportedOperation(format!(
                                "cannot assign property '{}' several times",
                                key
                            )));
                        }
                        ae.properties.push((key, value));
                    }
                },
                _ => unreachable!(),
            }
            plan_changed = true;
            plan.steps.remove(j);
            // Don't advance j — the element at j is now what was at j+1.
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
    use super::*;
    use crate::planner::logical_step::{AddEStep, AddVStep, PropertyStep};
    use crate::types::gvalue::Primitive;
    use smallvec::smallvec;
    use smol_str::SmolStr;

    /// Linear search in the SmallVec properties list.
    fn find_prop<'a>(props: &'a [(SmolStr, Primitive)], key: &str) -> Option<&'a Primitive> {
        props.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v)
    }

    fn addv() -> LogicalStep {
        LogicalStep::AddV(AddVStep { label: "person".into(), vertex_id: None, properties: smallvec![] })
    }

    fn adde() -> LogicalStep {
        LogicalStep::AddE(AddEStep {
            label: "knows".into(),
            out_v_id: None,
            in_v_id: None,
            properties: smallvec![],
            rank: None,
        })
    }

    fn prop(key: &str, value: Primitive) -> LogicalStep {
        LogicalStep::Property(PropertyStep { prop_key: SmolStr::new(key), prop_value: value })
    }

    // ── addV: id ──

    #[test]
    fn test_id_merged_into_addv() {
        let mut plan = LogicalPlan { steps: vec![addv(), prop("id", Primitive::Int32(7))] };
        let changed = merge_property_into_add(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddV(av) = &plan.steps[0] {
            assert_eq!(av.vertex_id, Some(7));
        }
    }

    #[test]
    fn test_id_merged_after_ordinary_property() {
        // addV().property("name","x").property("id",5) — id must still fold even
        // though a non-id property separates it from addV.
        let mut plan = LogicalPlan {
            steps: vec![addv(), prop("name", Primitive::String(SmolStr::new("x"))), prop("id", Primitive::Int32(5))],
        };
        let changed = merge_property_into_add(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddV(av) = &plan.steps[0] {
            assert_eq!(av.vertex_id, Some(5));
            assert_eq!(find_prop(&av.properties, "name"), Some(&Primitive::String(SmolStr::new("x"))));
        }
    }

    #[test]
    fn test_duplicate_id_errors() {
        let mut plan =
            LogicalPlan { steps: vec![addv(), prop("id", Primitive::Int32(1)), prop("id", Primitive::Int32(2))] };
        assert!(merge_property_into_add(&mut plan).is_err());
    }

    // ── addV: ordinary ──

    #[test]
    fn test_single_property_merged_into_addv() {
        let mut plan = LogicalPlan { steps: vec![addv(), prop("name", Primitive::String(SmolStr::new("alice")))] };
        let changed = merge_property_into_add(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddV(av) = &plan.steps[0] {
            assert_eq!(find_prop(&av.properties, "name"), Some(&Primitive::String(SmolStr::new("alice"))));
        }
    }

    #[test]
    fn test_multiple_properties_merged_into_addv() {
        let mut plan = LogicalPlan {
            steps: vec![
                addv(),
                prop("name", Primitive::String(SmolStr::new("alice"))),
                prop("age", Primitive::Int32(30)),
            ],
        };
        let changed = merge_property_into_add(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddV(av) = &plan.steps[0] {
            assert_eq!(av.properties.len(), 2);
        }
    }

    #[test]
    fn test_duplicate_property_errors() {
        let mut plan = LogicalPlan {
            steps: vec![
                addv(),
                prop("name", Primitive::String(SmolStr::new("alice"))),
                prop("name", Primitive::String(SmolStr::new("bob"))),
            ],
        };
        assert!(merge_property_into_add(&mut plan).is_err());
    }

    #[test]
    fn test_label_property_rejected() {
        let mut plan = LogicalPlan { steps: vec![addv(), prop("label", Primitive::String(SmolStr::new("person")))] };
        assert!(merge_property_into_add(&mut plan).is_err());
    }

    #[test]
    fn test_non_add_step_not_merged() {
        let mut plan = LogicalPlan { steps: vec![prop("name", Primitive::String(SmolStr::new("x"))), addv()] };
        assert!(!merge_property_into_add(&mut plan).unwrap());
        assert_eq!(plan.steps.len(), 2);
    }

    // ── addE ──

    #[test]
    fn test_single_property_merged_into_adde() {
        let mut plan = LogicalPlan { steps: vec![adde(), prop("weight", Primitive::Float64(0.5))] };
        let changed = merge_property_into_add(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddE(ae) = &plan.steps[0] {
            assert_eq!(find_prop(&ae.properties, "weight"), Some(&Primitive::Float64(0.5)));
        }
    }

    #[test]
    fn test_multiple_properties_merged_into_adde() {
        let mut plan = LogicalPlan {
            steps: vec![adde(), prop("weight", Primitive::Float64(0.5)), prop("since", Primitive::Int64(2020))],
        };
        let changed = merge_property_into_add(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddE(ae) = &plan.steps[0] {
            assert_eq!(ae.properties.len(), 2);
        }
    }

    #[test]
    fn test_rank_merged_into_adde_rank_field() {
        let mut plan = LogicalPlan { steps: vec![adde(), prop("rank", Primitive::Int32(5))] };
        let changed = merge_property_into_add(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddE(ae) = &plan.steps[0] {
            assert_eq!(ae.rank, Some(5));
            assert!(ae.properties.is_empty());
        }
    }

    #[test]
    fn test_arbitrary_order_rank_weight_merged() {
        let mut plan = LogicalPlan {
            steps: vec![adde(), prop("weight", Primitive::Float64(0.5)), prop("rank", Primitive::Int32(5))],
        };
        let changed = merge_property_into_add(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddE(ae) = &plan.steps[0] {
            assert_eq!(ae.rank, Some(5));
            assert_eq!(find_prop(&ae.properties, "weight"), Some(&Primitive::Float64(0.5)));
        }
    }

    #[test]
    fn test_rank_on_addv_errors() {
        let mut plan = LogicalPlan { steps: vec![addv(), prop("rank", Primitive::Int32(5))] };
        assert!(merge_property_into_add(&mut plan).is_err());
    }

    #[test]
    fn test_duplicate_rank_errors() {
        let mut plan =
            LogicalPlan { steps: vec![adde(), prop("rank", Primitive::Int32(5)), prop("rank", Primitive::Int32(6))] };
        assert!(merge_property_into_add(&mut plan).is_err());
    }

    #[test]
    fn test_id_int64_merged_into_addv() {
        let mut plan = LogicalPlan { steps: vec![addv(), prop("id", Primitive::Int64(1_000_000_000))] };
        let changed = merge_property_into_add(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddV(av) = &plan.steps[0] {
            assert_eq!(av.vertex_id, Some(1_000_000_000));
        }
    }

    #[test]
    fn test_id_prop_bad_type_errors() {
        let mut plan = LogicalPlan { steps: vec![addv(), prop("id", Primitive::String(SmolStr::new("bad")))] };
        assert!(merge_property_into_add(&mut plan).is_err());
    }

    #[test]
    fn test_interleaved_id_and_props_all_merged() {
        // addV().property("name","x").property("id",5).property("age",30)
        // — all three should be folded in arbitrary order.
        let mut plan = LogicalPlan {
            steps: vec![
                addv(),
                prop("name", Primitive::String(SmolStr::new("x"))),
                prop("id", Primitive::Int32(5)),
                prop("age", Primitive::Int32(30)),
            ],
        };
        let changed = merge_property_into_add(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddV(av) = &plan.steps[0] {
            assert_eq!(av.vertex_id, Some(5));
            assert_eq!(av.properties.len(), 2);
            assert_eq!(find_prop(&av.properties, "name"), Some(&Primitive::String(SmolStr::new("x"))));
            assert_eq!(find_prop(&av.properties, "age"), Some(&Primitive::Int32(30)));
        }
    }

    #[test]
    fn test_chained_addv_addv_props_not_confused() {
        let mut plan = LogicalPlan {
            steps: vec![
                addv(),
                LogicalStep::AddV(AddVStep { label: "b".into(), vertex_id: None, properties: smallvec![] }),
                prop("name", Primitive::String(SmolStr::new("x"))),
            ],
        };
        let changed = merge_property_into_add(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 2);
        if let LogicalStep::AddV(av) = &plan.steps[0] {
            assert!(av.properties.is_empty());
        }
        if let LogicalStep::AddV(av) = &plan.steps[1] {
            assert_eq!(find_prop(&av.properties, "name"), Some(&Primitive::String(SmolStr::new("x"))));
        }
    }
}
