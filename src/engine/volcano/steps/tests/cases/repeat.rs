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

//! Tests for the `repeat` / `until` / `emit` / `emit_if` physical step.

use super::*;

// ── RepeatStep tests ────────────────────────────────────────────────────

#[test]
fn test_repeat_times_n_hop() {
    let (store, _dir) = open_rocks_store();
    let mut graph = create_tinkerpop_modern_graph(&store);

    // V(1).repeat(out()).times(2) → 2-hop neighbors from marko
    let body =
        LogicalPlan { steps: vec![LogicalStep::Out(LogicalOutStep { labels: smallvec![], end_vertex_ids: None })] };
    let logical_plan = LogicalPlan {
        steps: vec![
            LogicalStep::V(LogicalVStep { ids: smallvec![1] }),
            LogicalStep::Repeat(LogicalRepeatStep { body, until: None, times: Some(2), emit: EmitSpec::Never }),
        ],
    };

    let mut builder: PhysicalPlanBuilder = Default::default();
    let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
    let mut results = Vec::new();
    while let Ok(Some(t)) = physical_plan.next(&mut graph) {
        results.push(t.as_ref().value.clone());
    }
    // marko → josh → [ripple(5), lop(3)] = 2 results
    assert_eq!(results.len(), 2);
    assert!(results.contains(&GValue::Vertex(3)));
    assert!(results.contains(&GValue::Vertex(5)));
}

#[test]
fn test_repeat_until_short_circuit() {
    let (store, _dir) = open_rocks_store();
    let mut graph = create_tinkerpop_modern_graph(&store);

    // V(1).repeat(out()).until(hasLabel("software"))
    let body =
        LogicalPlan { steps: vec![LogicalStep::Out(LogicalOutStep { labels: smallvec![], end_vertex_ids: None })] };
    let until_plan = LogicalPlan {
        steps: vec![LogicalStep::HasLabel(LogicalHasLabelStep {
            pred: PrimitivePredicate::Eq(Primitive::String(SmolStr::new("software"))),
        })],
    };
    let logical_plan = LogicalPlan {
        steps: vec![
            LogicalStep::V(LogicalVStep { ids: smallvec![1] }),
            LogicalStep::Repeat(LogicalRepeatStep {
                body,
                until: Some(until_plan),
                times: None,
                emit: EmitSpec::Never,
            }),
        ],
    };

    let mut builder: PhysicalPlanBuilder = Default::default();
    let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
    let mut results = Vec::new();
    while let Ok(Some(t)) = physical_plan.next(&mut graph) {
        results.push(t.as_ref().value.clone());
    }
    // marko→lop(software, emitted immediately), vadas(person, continues),
    // josh(person, continues). Then vadas→[], josh→[ripple(software), lop(software)].
    // So: lop(3), ripple(5), lop(3) = 3.
    results.sort_by_key(|v| if let GValue::Vertex(id) = v { *id } else { 0 });
    assert_eq!(results.len(), 3);
    assert_eq!(results[0], GValue::Vertex(3));
    assert_eq!(results[1], GValue::Vertex(3));
    assert_eq!(results[2], GValue::Vertex(5));
}

#[test]
fn test_repeat_emit_all_intermediates() {
    let (store, _dir) = open_rocks_store();
    let mut graph = create_tinkerpop_modern_graph(&store);

    // V(1).repeat(out()).times(3).emit()
    let body =
        LogicalPlan { steps: vec![LogicalStep::Out(LogicalOutStep { labels: smallvec![], end_vertex_ids: None })] };
    let logical_plan = LogicalPlan {
        steps: vec![
            LogicalStep::V(LogicalVStep { ids: smallvec![1] }),
            LogicalStep::Repeat(LogicalRepeatStep { body, until: None, times: Some(3), emit: EmitSpec::Always }),
        ],
    };

    let mut builder: PhysicalPlanBuilder = Default::default();
    let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
    let mut results = Vec::new();
    while let Ok(Some(t)) = physical_plan.next(&mut graph) {
        results.push(t.as_ref().value.clone());
    }
    // 1st iter: vadas(2), lop(3), josh(4) — all emitted (always)
    // 2nd iter: ripple(5), lop(3) — all emitted
    // 3rd iter: no outputs (dead ends) — frontier empty
    assert_eq!(results.len(), 5);
    let mut ids: Vec<i64> = results.iter().map(|v| if let GValue::Vertex(id) = v { *id } else { 0 }).collect();
    ids.sort();
    assert_eq!(ids, vec![2, 3, 3, 4, 5]);
}

#[test]
fn test_repeat_emit_if() {
    let (store, _dir) = open_rocks_store();
    let mut graph = create_tinkerpop_modern_graph(&store);

    // V(1).repeat(out()).times(3).emit_if(hasLabel("person"))
    let body =
        LogicalPlan { steps: vec![LogicalStep::Out(LogicalOutStep { labels: smallvec![], end_vertex_ids: None })] };
    let emit_cond = LogicalPlan {
        steps: vec![LogicalStep::HasLabel(LogicalHasLabelStep {
            pred: PrimitivePredicate::Eq(Primitive::String(SmolStr::new("person"))),
        })],
    };
    let logical_plan = LogicalPlan {
        steps: vec![
            LogicalStep::V(LogicalVStep { ids: smallvec![1] }),
            LogicalStep::Repeat(LogicalRepeatStep { body, until: None, times: Some(3), emit: EmitSpec::If(emit_cond) }),
        ],
    };

    let mut builder: PhysicalPlanBuilder = Default::default();
    let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
    let mut results = Vec::new();
    while let Ok(Some(t)) = physical_plan.next(&mut graph) {
        results.push(t.as_ref().value.clone());
    }
    // emit_if(person): 1st iter emits vadas(2), josh(4);
    // 2nd iter: ripple(software) and lop(software) don't match emit_if, go to frontier;
    // 3rd iter: ripple and lop have no outgoing edges → body produces nothing → loop ends.
    // Final result: only 1st-iter intermediates that matched emit_if(person).
    let mut ids: Vec<i64> = results.iter().map(|v| if let GValue::Vertex(id) = v { *id } else { 0 }).collect();
    ids.sort();
    assert_eq!(ids, vec![2, 4]);
}

#[test]
fn test_repeat_path_tracking() {
    let (store, _dir) = open_rocks_store();
    let mut graph = create_tinkerpop_modern_graph(&store);

    // V(1).repeat(out()).times(2).path()
    let body =
        LogicalPlan { steps: vec![LogicalStep::Out(LogicalOutStep { labels: smallvec![], end_vertex_ids: None })] };
    let logical_plan = LogicalPlan {
        steps: vec![
            LogicalStep::V(LogicalVStep { ids: smallvec![1] }),
            LogicalStep::Repeat(LogicalRepeatStep { body, until: None, times: Some(2), emit: EmitSpec::Never }),
            LogicalStep::Path(crate::planner::logical_step::PathStep {}),
        ],
    };

    let mut builder: PhysicalPlanBuilder = Default::default();
    let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
    let mut results = Vec::new();
    while let Ok(Some(t)) = physical_plan.next(&mut graph) {
        results.push(t.as_ref().value.clone());
    }
    // 2-hop paths: marko→josh→ripple, marko→josh→lop
    assert_eq!(results.len(), 2);
    for res in &results {
        if let GValue::Path(path) = res {
            assert_eq!(path.len(), 3, "each path should have 3 elements");
            assert_eq!(path[0].0, GValue::Vertex(1));
        } else {
            panic!("Expected Path, got {:?}", res);
        }
    }
}

#[test]
fn test_repeat_cycle_terminates_with_times() {
    let (store, _dir) = open_rocks_store();
    let mut graph = create_tinkerpop_modern_graph(&store);

    // Add a back-edge to create a cycle: vadas(2) → marko(1)
    let vadas_id = graph.get_vertex(2).unwrap().unwrap();
    let marko_id = graph.get_vertex(1).unwrap().unwrap();
    let knows_label_id = graph.schema.read().unwrap().edge_label_id("knows").unwrap();
    graph
        .add_edge(&EdgeKey {
            primary_id: vadas_id,
            direction: Direction::OUT,
            label_id: knows_label_id,
            secondary_id: marko_id,
            rank: 0,
        })
        .unwrap();
    graph.commit().unwrap();

    // V(1).repeat(out()).times(5) — must terminate, not hang
    let body =
        LogicalPlan { steps: vec![LogicalStep::Out(LogicalOutStep { labels: smallvec![], end_vertex_ids: None })] };
    let logical_plan = LogicalPlan {
        steps: vec![
            LogicalStep::V(LogicalVStep { ids: smallvec![1] }),
            LogicalStep::Repeat(LogicalRepeatStep { body, until: None, times: Some(5), emit: EmitSpec::Never }),
        ],
    };

    let mut builder: PhysicalPlanBuilder = Default::default();
    let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
    let mut results = Vec::new();
    while let Ok(Some(t)) = physical_plan.next(&mut graph) {
        results.push(t.as_ref().value.clone());
    }
    // Must produce results (not hang) and terminate.
    assert!(!results.is_empty(), "cycle with times(5) must produce results");
}
