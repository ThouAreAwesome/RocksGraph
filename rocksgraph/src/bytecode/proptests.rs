use super::{decode, encode};
use crate::{
    planner::logical_step::*,
    types::{
        gvalue::{Primitive, PrimitivePredicate},
        keys::DegreeDirection,
    },
    vector::{error::VectorEntityType, traits::DistanceMetric},
};
use proptest::prelude::*;
use smol_str::SmolStr;

fn distance_metric_strategy() -> impl Strategy<Value = DistanceMetric> {
    prop_oneof![Just(DistanceMetric::Cosine), Just(DistanceMetric::Euclidean), Just(DistanceMetric::DotProduct),]
}

fn vector_entity_type_strategy() -> impl Strategy<Value = VectorEntityType> {
    prop_oneof![Just(VectorEntityType::Vertex), Just(VectorEntityType::Edge),]
}

fn degree_direction_strategy() -> impl Strategy<Value = DegreeDirection> {
    prop_oneof![Just(DegreeDirection::Out), Just(DegreeDirection::In), Just(DegreeDirection::Both)]
}

fn order_strategy() -> impl Strategy<Value = Order> {
    prop_oneof![Just(Order::Asc), Just(Order::Desc)]
}

// Also used for `PropKey` (a `SmolStr` alias) and `EStep.keys`-adjacent String fields.
fn smolstr_strategy() -> impl Strategy<Value = SmolStr> {
    ".*".prop_map(SmolStr::from)
}

/// Every `Primitive` variant, bounded well below any wire-format length limit —
/// this proptest is exercising the bytecode step *shape*, not stress-testing
/// individual value encoding (see `bulk::proptests`/`types::prop_codec::tests`
/// for that).
fn primitive_strategy() -> BoxedStrategy<Primitive> {
    prop_oneof![
        Just(Primitive::Null),
        any::<bool>().prop_map(Primitive::Bool),
        any::<i32>().prop_map(Primitive::Int32),
        any::<i64>().prop_map(Primitive::Int64),
        any::<u16>().prop_map(Primitive::UInt16),
        any::<f32>().prop_filter("finite", |f| f.is_finite()).prop_map(Primitive::Float32),
        any::<f64>().prop_filter("finite", |f| f.is_finite()).prop_map(Primitive::Float64),
        smolstr_strategy().prop_map(Primitive::String),
        any::<u128>().prop_map(Primitive::Uuid),
        prop::collection::vec(any::<u8>(), 0..16).prop_map(Primitive::Bytes),
        prop::collection::vec(any::<f32>(), 0..8).prop_map(Primitive::FloatVector),
    ]
    .boxed()
}

/// All 9 `PrimitivePredicate` variants (the original version of this file only
/// covered `Eq`/`Gt`, which under-tested every `Has*`/`ScalarFilter` step).
fn primitive_predicate_strategy() -> BoxedStrategy<PrimitivePredicate> {
    prop_oneof![
        primitive_strategy().prop_map(PrimitivePredicate::Eq),
        primitive_strategy().prop_map(PrimitivePredicate::Ne),
        primitive_strategy().prop_map(PrimitivePredicate::Gt),
        primitive_strategy().prop_map(PrimitivePredicate::Gte),
        primitive_strategy().prop_map(PrimitivePredicate::Lt),
        primitive_strategy().prop_map(PrimitivePredicate::Lte),
        (primitive_strategy(), primitive_strategy()).prop_map(|(lo, hi)| PrimitivePredicate::Between(lo, hi)),
        prop::collection::vec(primitive_strategy(), 0..4).prop_map(PrimitivePredicate::Within),
        prop::collection::vec(primitive_strategy(), 0..4).prop_map(PrimitivePredicate::Without),
    ]
    .boxed()
}

fn end_vertex_ids_strategy() -> impl Strategy<Value = Option<smallvec::SmallVec<[i64; 4]>>> {
    proptest::option::of(prop::collection::vec(any::<i64>(), 0..3).prop_map(|v| v.into_iter().collect()))
}

// `OrderKey`/`EmitSpec` don't implement `Debug` (same reason `LogicalStep`/
// `LogicalPlan` needed the `DebugStep`/`DebugPlan` wrappers below) — proptest's
// `prop_map`/`Just` require the output type to be `Debug`.
struct DebugOrderKey(OrderKey);

impl std::fmt::Debug for DebugOrderKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OrderKey")
    }
}

struct DebugEmitSpec(EmitSpec);

impl std::fmt::Debug for DebugEmitSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EmitSpec")
    }
}

/// One `OrderKey`. `OrderKeySpec::Traversal` is the only recursive case — gated
/// on `depth` like every other sub-plan-carrying strategy in this file.
fn order_key_strategy(depth: u32) -> BoxedStrategy<DebugOrderKey> {
    let non_recursive = prop_oneof![
        order_strategy().prop_map(|order| DebugOrderKey(OrderKey { spec: OrderKeySpec::Value, order })),
        (smolstr_strategy(), order_strategy())
            .prop_map(|(p, order)| DebugOrderKey(OrderKey { spec: OrderKeySpec::Property(p), order })),
    ];
    if depth > 0 {
        prop_oneof![
            2 => non_recursive,
            1 => (plan_strategy(depth - 1), order_strategy())
                .prop_map(|(p, order)| DebugOrderKey(OrderKey { spec: OrderKeySpec::Traversal(p.0), order })),
        ]
        .boxed()
    } else {
        non_recursive.boxed()
    }
}

fn emit_spec_strategy(depth: u32) -> BoxedStrategy<DebugEmitSpec> {
    // `Just(EmitSpec::Never)` would itself require `EmitSpec: Debug` before
    // `.prop_map` even runs — route through `Just(())` instead.
    let never = Just(()).prop_map(|_| DebugEmitSpec(EmitSpec::Never));
    let always = Just(()).prop_map(|_| DebugEmitSpec(EmitSpec::Always));
    if depth > 0 {
        prop_oneof![
            2 => never,
            2 => always,
            1 => plan_strategy(depth - 1).prop_map(|p| DebugEmitSpec(EmitSpec::If(p.0))),
        ]
        .boxed()
    } else {
        prop_oneof![never, always].boxed()
    }
}

#[derive(Clone)]
struct DebugStep(LogicalStep);

impl std::fmt::Debug for DebugStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LogicalStep")
    }
}

fn step_strategy(depth: u32) -> BoxedStrategy<DebugStep> {
    let leaves = prop_oneof![
        Just(DebugStep(LogicalStep::Drop(DropStep {}))),
        Just(DebugStep(LogicalStep::Path(PathStep {}))),
        Just(DebugStep(LogicalStep::Dedup(DedupStep {}))),
        Just(DebugStep(LogicalStep::Fold(FoldStep {}))),
        Just(DebugStep(LogicalStep::Sum(SumStep {}))),
        Just(DebugStep(LogicalStep::Mean(MeanStep {}))),
        Just(DebugStep(LogicalStep::Max(MaxStep {}))),
        Just(DebugStep(LogicalStep::Min(MinStep {}))),
        Just(DebugStep(LogicalStep::Unfold(UnfoldStep {}))),
        Just(DebugStep(LogicalStep::SimplePath(SimplePathStep {}))),
        Just(DebugStep(LogicalStep::CyclicPath(CyclicPathStep {}))),
        Just(DebugStep(LogicalStep::Identity(IdentityStep {}))),
        Just(DebugStep(LogicalStep::Id(IdStep {}))),
        Just(DebugStep(LogicalStep::Label(LabelStep {}))),
        Just(DebugStep(LogicalStep::Rank(RankStep {}))),
        Just(DebugStep(LogicalStep::Count(CountStep {}))),
        Just(DebugStep(LogicalStep::InV(InVStep {}))),
        Just(DebugStep(LogicalStep::OtherV(OtherVStep {}))),
        Just(DebugStep(LogicalStep::OutV(OutVStep {}))),
        any::<i64>().prop_map(|limit| DebugStep(LogicalStep::Limit(LimitStep { limit }))),
        prop::collection::vec(smolstr_strategy(), 0..3).prop_map(|labels| DebugStep(LogicalStep::Out(OutStep {
            labels: labels.into_iter().collect(),
            end_vertex_ids: None,
        }))),
        prop::collection::vec(smolstr_strategy(), 0..3).prop_map(|labels| DebugStep(LogicalStep::In(InStep {
            labels: labels.into_iter().collect(),
            end_vertex_ids: None,
        }))),
        primitive_predicate_strategy().prop_map(|pred| DebugStep(LogicalStep::HasLabel(HasLabelStep { pred }))),
        (
            any::<String>(),
            prop::collection::vec(any::<f32>(), 0..5),
            any::<usize>(),
            any::<Option<usize>>(),
            proptest::option::of(distance_metric_strategy()),
            any::<bool>()
        )
            .prop_map(|(prop_key, query_vec, k, ef_search, metric_override, is_root)| {
                DebugStep(LogicalStep::Nearest(NearestLogicalStep {
                    prop_key,
                    query_vec,
                    k,
                    ef_search,
                    metric_override,
                    is_root,
                }))
            }),
        (any::<String>(), prop::collection::vec(any::<f32>(), 0..5), distance_metric_strategy()).prop_map(
            |(prop_key, query_vec, metric)| {
                DebugStep(LogicalStep::Similarity(SimilarityLogicalStep { prop_key, query_vec, metric }))
            }
        ),
        (any::<String>(), any::<String>(), any::<usize>(), any::<Option<usize>>(), vector_entity_type_strategy())
            .prop_map(|(source_prop, target_prop, k, ef_search, entity_type)| {
                DebugStep(LogicalStep::Neighbors(NeighborsLogicalStep {
                    source_prop,
                    target_prop,
                    k,
                    ef_search,
                    entity_type,
                }))
            }),
        // ── Traversal-shape steps not covered above ─────────────────────────
        (prop::collection::vec(smolstr_strategy(), 0..3), end_vertex_ids_strategy()).prop_map(|(labels, ids)| {
            DebugStep(LogicalStep::Both(BothStep { labels: labels.into_iter().collect(), end_vertex_ids: ids }))
        }),
        (
            prop::collection::vec(smolstr_strategy(), 0..3),
            end_vertex_ids_strategy(),
            proptest::option::of(any::<u16>())
        )
            .prop_map(|(labels, ids, rank)| {
                DebugStep(LogicalStep::BothE(BothEStep {
                    labels: labels.into_iter().collect(),
                    end_vertex_ids: ids,
                    rank,
                }))
            }),
        (
            prop::collection::vec(smolstr_strategy(), 0..3),
            end_vertex_ids_strategy(),
            proptest::option::of(any::<u16>())
        )
            .prop_map(|(labels, ids, rank)| {
                DebugStep(LogicalStep::InE(InEStep { labels: labels.into_iter().collect(), end_vertex_ids: ids, rank }))
            }),
        (
            prop::collection::vec(smolstr_strategy(), 0..3),
            end_vertex_ids_strategy(),
            proptest::option::of(any::<u16>())
        )
            .prop_map(|(labels, ids, rank)| {
                DebugStep(LogicalStep::OutE(OutEStep {
                    labels: labels.into_iter().collect(),
                    end_vertex_ids: ids,
                    rank,
                }))
            }),
        degree_direction_strategy().prop_map(|direction| DebugStep(LogicalStep::Degree(DegreeStep { direction }))),
        prop::collection::vec(any::<i64>(), 0..3)
            .prop_map(|ids| DebugStep(LogicalStep::V(VStep { ids: ids.into_iter().collect() }))),
        prop::collection::vec(any::<String>(), 0..3)
            .prop_map(|keys| DebugStep(LogicalStep::E(EStep { keys: keys.into_iter().collect() }))),
        any::<i64>().prop_map(|vertex_id| DebugStep(LogicalStep::From(FromStep { vertex_id }))),
        any::<i64>().prop_map(|vertex_id| DebugStep(LogicalStep::To(ToStep { vertex_id }))),
        (smolstr_strategy(), primitive_strategy())
            .prop_map(|(prop_key, prop_value)| DebugStep(LogicalStep::Property(PropertyStep { prop_key, prop_value }))),
        (
            smolstr_strategy(),
            proptest::option::of(any::<i64>()),
            prop::collection::vec((smolstr_strategy(), primitive_strategy()), 0..3),
        )
            .prop_map(|(label, vertex_id, properties)| {
                DebugStep(LogicalStep::AddV(AddVStep {
                    label,
                    vertex_id,
                    properties: properties.into_iter().collect(),
                }))
            }),
        (
            smolstr_strategy(),
            proptest::option::of(any::<i64>()),
            proptest::option::of(any::<i64>()),
            prop::collection::vec((smolstr_strategy(), primitive_strategy()), 0..3),
            proptest::option::of(any::<u16>()),
        )
            .prop_map(|(label, out_v_id, in_v_id, properties, rank)| {
                DebugStep(LogicalStep::AddE(AddEStep {
                    label,
                    out_v_id,
                    in_v_id,
                    properties: properties.into_iter().collect(),
                    rank,
                }))
            }),
        // ── Filter/predicate steps not covered above ─────────────────────────
        (smolstr_strategy(), primitive_predicate_strategy())
            .prop_map(|(key, pred)| DebugStep(LogicalStep::HasProperty(HasPropertyStep { key, pred }))),
        primitive_predicate_strategy().prop_map(|pred| DebugStep(LogicalStep::ScalarFilter(ScalarFilterStep { pred }))),
        primitive_predicate_strategy().prop_map(|pred| DebugStep(LogicalStep::HasId(HasIdStep { pred }))),
        primitive_predicate_strategy().prop_map(|pred| DebugStep(LogicalStep::HasRank(HasRankStep { pred }))),
        // Deliberately NOT generating LogicalStep::EndVertexFilter: `decode()`
        // explicitly rejects `OP_ENDVERTEXFILTER` with "Internal only" (see
        // `bytecode/mod.rs`) — it's an optimizer-internal artifact `encode()`
        // can still write out, but never valid as decoded input. That's the
        // only step with this asymmetry; round-tripping it is expected to
        // fail by design, not a bug this proptest should be flagging.
        // ── Projection / aggregation / paging steps not covered above ────────
        prop::collection::vec(smolstr_strategy(), 0..3)
            .prop_map(|keys| DebugStep(LogicalStep::Values(ValuesStep { property_keys: keys.into_iter().collect() }))),
        prop::collection::vec(smolstr_strategy(), 0..3).prop_map(|keys| {
            DebugStep(LogicalStep::Properties(PropertiesStep { property_keys: keys.into_iter().collect() }))
        }),
        prop::collection::vec(smolstr_strategy(), 0..2)
            .prop_map(|labels| DebugStep(LogicalStep::As(AsStep { labels: labels.into_iter().collect() }))),
        prop::collection::vec(smolstr_strategy(), 0..2)
            .prop_map(|labels| DebugStep(LogicalStep::Select(SelectStep { labels: labels.into_iter().collect() }))),
        (any::<i64>(), any::<i64>()).prop_map(|(lo, hi)| DebugStep(LogicalStep::Range(RangeStep { lo, hi }))),
        any::<i64>().prop_map(|n| DebugStep(LogicalStep::Skip(SkipStep { n }))),
        any::<i64>().prop_map(|n| DebugStep(LogicalStep::Tail(TailStep { n }))),
        proptest::option::of(smolstr_strategy()).prop_map(|key| DebugStep(LogicalStep::Group(GroupStep { key }))),
        proptest::option::of(smolstr_strategy())
            .prop_map(|key| DebugStep(LogicalStep::GroupCount(GroupCountStep { key }))),
        primitive_strategy().prop_map(|value| DebugStep(LogicalStep::Constant(ConstantStep { value }))),
        prop::collection::vec(order_key_strategy(depth), 0..3).prop_map(|keys| {
            DebugStep(LogicalStep::Order(OrderStep { keys: keys.into_iter().map(|k| k.0).collect() }))
        }),
    ];

    if depth > 0 {
        let recursives = prop_oneof![
            plan_strategy(depth - 1).prop_map(|p| DebugStep(LogicalStep::Where(WhereStep { plan: p.0 }))),
            plan_strategy(depth - 1).prop_map(|p| DebugStep(LogicalStep::Not(NotStep { plan: p.0 }))),
            plan_strategy(depth - 1).prop_map(|p| DebugStep(LogicalStep::Local(LocalStep { plan: p.0 }))),
            prop::collection::vec(plan_strategy(depth - 1), 0..2).prop_map(|plans| DebugStep(LogicalStep::And(
                AndStep { plans: plans.into_iter().map(|p| p.0).collect() }
            ))),
            prop::collection::vec(plan_strategy(depth - 1), 0..2).prop_map(|plans| DebugStep(LogicalStep::Or(
                OrStep { plans: plans.into_iter().map(|p| p.0).collect() }
            ))),
            prop::collection::vec(plan_strategy(depth - 1), 0..2).prop_map(|plans| DebugStep(LogicalStep::Coalesce(
                CoalesceStep { plans: plans.into_iter().map(|p| p.0).collect() }
            ))),
            prop::collection::vec(plan_strategy(depth - 1), 0..2).prop_map(|plans| DebugStep(LogicalStep::Union(
                UnionStep { plans: plans.into_iter().map(|p| p.0).collect() }
            ))),
            (
                plan_strategy(depth - 1),
                proptest::option::of(plan_strategy(depth - 1)),
                proptest::option::of(any::<i64>()),
                emit_spec_strategy(depth - 1),
            )
                .prop_map(|(body, until, times, emit)| {
                    DebugStep(LogicalStep::Repeat(RepeatStep {
                        body: body.0,
                        until: until.map(|p| p.0),
                        times,
                        emit: emit.0,
                    }))
                }),
            (plan_strategy(depth - 1), plan_strategy(depth - 1), proptest::option::of(plan_strategy(depth - 1)))
                .prop_map(|(predicate, true_choice, false_choice)| {
                    DebugStep(LogicalStep::Choose(ChooseStep {
                        predicate: predicate.0,
                        true_choice: true_choice.0,
                        false_choice: false_choice.map(|p| p.0),
                    }))
                }),
        ];
        prop_oneof![leaves, recursives].boxed()
    } else {
        leaves.boxed()
    }
}

#[derive(Clone)]
struct DebugPlan(LogicalPlan);

impl std::fmt::Debug for DebugPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LogicalPlan ({} steps)", self.0.steps.len())
    }
}

fn plan_strategy(depth: u32) -> BoxedStrategy<DebugPlan> {
    prop::collection::vec(step_strategy(depth), 0..5)
        .prop_map(|steps| {
            let mut plan = LogicalPlan { steps: vec![] };
            for step in steps {
                plan.steps.push(step.0);
            }
            DebugPlan(plan)
        })
        .boxed()
}

proptest! {
    #[test]
    fn test_bytecode_roundtrip(original_plan in plan_strategy(2)) {
        let bytes = encode(&original_plan.0);
        let decoded_plan = decode(&bytes).expect("Failed to decode a valid plan");

        // LogicalPlan doesn't implement PartialEq in this codebase, so we'll encode it
        // again and ensure the encoded bytes match, which is a strong proxy for structural equality.
        let bytes_again = encode(&decoded_plan);
        assert_eq!(bytes, bytes_again, "Roundtrip failed to preserve structural identity");
    }
}
