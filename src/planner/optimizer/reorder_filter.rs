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

/// Reorder pair with partial ordering hasId = has("id"..) > hasLabel > has(not id..) > where():
/// 1. has().has("id"..) into has("id"..).has(),
/// 2. has().hasId() into hasId().has()
/// 3. where().has() into has().where()
/// 4. where().hasId() into hasId().where()
/// 5. where().hasLabel() into hasLabel().where()
/// 6. hasLabel().hasId() into hasId().hasLabel() (already covered by 2)
/// 7. hasLabel().has("id"..) into has("id"..).hasLabel() (already covered by 1)
/// 8. has(not id..).hasLabel() into hasLabel().has(not id..)
pub fn reorder_filters(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut changed = false;
    let mut i = 0;
    // Iterate up to `plan.steps.len() - 1` to ensure `i + 1` is always a valid index.
    while i + 1 < plan.steps.len() {
        let should_swap = match (&plan.steps[i], &plan.steps[i + 1]) {
            // Rule 1: has().has("id"..) -> has("id"..).has()
            (LogicalStep::HasProperty(hp0), LogicalStep::HasProperty(hp1))
                if hp0.key.as_str() != ID && hp1.key.as_str() == ID =>
            {
                true
            }
            // Rule 2: has().hasId() -> hasId().has()
            (LogicalStep::HasProperty(_), LogicalStep::HasId(_)) => true,
            // Rule 3: where().has() -> has().where()
            (LogicalStep::Where(_), LogicalStep::HasProperty(_)) => true,
            // Rule 4: where().hasId() -> hasId().where()
            (LogicalStep::Where(_), LogicalStep::HasId(_)) => true,
            // Rule 5: where().hasLabel() -> hasLabel().where()
            (LogicalStep::Where(_), LogicalStep::HasLabel(_)) => true,
            // Rule 6: hasLabel().hasId() -> hasId().hasLabel()
            (LogicalStep::HasLabel(_), LogicalStep::HasId(_)) => true,
            // Rule 7: hasLabel().has("id"..) -> has("id"..).hasLabel()
            (LogicalStep::HasLabel(_), LogicalStep::HasProperty(hp)) if hp.key.as_str() == ID => true,
            // Rule 8: has().hasLabel() -> hasLabel().has()
            (LogicalStep::HasProperty(hp), LogicalStep::HasLabel(_)) if hp.key.as_str() != ID => true,
            _ => false,
        };

        if should_swap {
            plan.steps.swap(i, i + 1);
            changed = true;
            // After a swap, it's often beneficial to re-evaluate from the beginning
            // or at least from the swapped position, as the new order might enable
            // further optimizations or satisfy a different reordering rule.
            // For simplicity, we'll just move to the next pair, but a more robust
            // optimizer might reset `i` to 0 or `i.saturating_sub(1)`.
            // For this specific reordering, advancing `i` is fine.
        }
        i += 1;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;

    use super::*;
    use crate::{
        planner::logical_step::{HasIdStep, HasLabelStep, HasPropertyStep, VStep, WhereStep},
        types::{gvalue::Primitive, keys::VertexKey},
    };
    use smallvec::smallvec;

    fn v_all() -> LogicalStep {
        LogicalStep::V(VStep { ids: smallvec![] })
    }

    fn has_prop(key: &str, value: Primitive) -> LogicalStep {
        LogicalStep::HasProperty(HasPropertyStep { key: SmolStr::new(key), value })
    }

    fn has_id(ids: Vec<VertexKey>) -> LogicalStep {
        LogicalStep::HasId(HasIdStep { ids: ids.into_iter().collect() })
    }

    fn has_label(labels: Vec<&str>) -> LogicalStep {
        LogicalStep::HasLabel(HasLabelStep { labels: labels.into_iter().map(SmolStr::new).collect() })
    }

    fn whr(sub_steps: Vec<LogicalStep>) -> LogicalStep {
        LogicalStep::Where(WhereStep { plan: LogicalPlan { steps: sub_steps } })
    }

    #[test]
    fn test_has_prop_then_has_id_swapped() {
        let mut plan = LogicalPlan {
            steps: vec![v_all(), has_prop("name", Primitive::String(SmolStr::new("marko"))), has_id(vec![1])],
        };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasId(_))); // hasId should be first
        assert!(matches!(plan.steps[2], LogicalStep::HasProperty(_))); // then hasProperty
    }

    #[test]
    fn test_has_label_then_has_id_swapped() {
        let mut plan = LogicalPlan { steps: vec![v_all(), has_label(vec!["10"]), has_id(vec![1])] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasId(_))); // hasId should be first
        assert!(matches!(plan.steps[2], LogicalStep::HasLabel(_))); // then hasLabel
    }

    #[test]
    fn test_has_label_then_has_prop_id_swapped() {
        let mut plan = LogicalPlan { steps: vec![v_all(), has_label(vec!["10"]), has_prop("id", Primitive::Int32(1))] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasProperty(_))); // has("id",..) should be first
        assert!(matches!(plan.steps[2], LogicalStep::HasLabel(_))); // then hasLabel
        if let LogicalStep::HasProperty(hp) = &plan.steps[1] {
            assert_eq!(hp.key.as_str(), ID);
        }
    }

    #[test]
    fn test_where_then_has_prop_swapped() {
        let mut plan = LogicalPlan {
            steps: vec![
                v_all(),
                whr(vec![has_id(vec![10])]),
                has_prop("name", Primitive::String(SmolStr::new("marko"))),
            ],
        };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasProperty(_))); // hasProperty should be first
        assert!(matches!(plan.steps[2], LogicalStep::Where(_))); // then where
    }

    #[test]
    fn test_where_then_has_id_swapped() {
        let mut plan = LogicalPlan { steps: vec![v_all(), whr(vec![has_id(vec![10])]), has_id(vec![1])] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasId(_))); // hasId should be first
        assert!(matches!(plan.steps[2], LogicalStep::Where(_))); // then where
    }

    #[test]
    fn test_where_then_has_label_swapped() {
        let mut plan = LogicalPlan { steps: vec![v_all(), whr(vec![has_id(vec![10])]), has_label(vec!["1"])] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasLabel(_))); // hasLabel should be first
        assert!(matches!(plan.steps[2], LogicalStep::Where(_))); // then where
    }

    #[test]
    fn test_no_swap_needed_unchanged() {
        let mut plan = LogicalPlan {
            steps: vec![v_all(), has_id(vec![1]), has_prop("name", Primitive::String(SmolStr::new("marko")))],
        };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(!changed); // Already in preferred order
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasId(_)));
        assert!(matches!(plan.steps[2], LogicalStep::HasProperty(_)));
    }

    #[test]
    fn test_multiple_swaps_in_one_pass() {
        // Initial: V().HasProp(name).HasLabel().HasId()
        // Expected: V().HasId().HasLabel().HasProp(name)
        let mut plan = LogicalPlan {
            steps: vec![
                v_all(),
                has_prop("name", Primitive::String(SmolStr::new("marko"))),
                has_label(vec!["10"]),
                has_id(vec![1]),
            ],
        };
        while reorder_filters(&mut plan).unwrap() {}
        assert_eq!(plan.steps.len(), 4);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasId(_)));
        assert!(matches!(plan.steps[2], LogicalStep::HasLabel(_)));
        assert!(matches!(plan.steps[3], LogicalStep::HasProperty(_)));
    }

    #[test]
    fn test_no_filters_unchanged() {
        let mut plan = LogicalPlan { steps: vec![v_all()] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 1);
    }

    #[test]
    fn test_single_filter_unchanged() {
        let mut plan = LogicalPlan { steps: vec![v_all(), has_id(vec![1])] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 2);
    }
}
