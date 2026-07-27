use crate::engine::GraphCtx;
use crate::gremlin::value::Value;
use crate::planner::logical_step::*;
use crate::types::gvalue::{Primitive, PrimitivePredicate};
use crate::types::keys::DegreeDirection;
use crate::types::StoreError;
use smol_str::SmolStr;

pub const VERSION: u8 = 0x01;

pub const OP_BOTH: u8 = 1;
pub const OP_BOTHE: u8 = 2;
pub const OP_COUNT: u8 = 3;
pub const OP_DEGREE: u8 = 4;
pub const OP_HASLABEL: u8 = 5;
pub const OP_HASPROPERTY: u8 = 6;
pub const OP_IN: u8 = 7;
pub const OP_INE: u8 = 8;
pub const OP_OUT: u8 = 9;
pub const OP_OUTE: u8 = 10;
pub const OP_INV: u8 = 11;
pub const OP_OTHERV: u8 = 12;
pub const OP_OUTV: u8 = 13;
pub const OP_SCALARFILTER: u8 = 14;
pub const OP_VALUES: u8 = 15;
pub const OP_PROPERTIES: u8 = 16;
pub const OP_WHERE: u8 = 17;
pub const OP_UNION: u8 = 18;
pub const OP_ADDV: u8 = 19;
pub const OP_ADDE: u8 = 20;
pub const OP_FROM: u8 = 21;
pub const OP_TO: u8 = 22;
pub const OP_PROPERTY: u8 = 23;
pub const OP_V: u8 = 24;
pub const OP_E: u8 = 25;
pub const OP_LIMIT: u8 = 26;
pub const OP_HASID: u8 = 27;
pub const OP_COALESCE: u8 = 28;
pub const OP_ENDVERTEXFILTER: u8 = 29;
pub const OP_DROP: u8 = 30;
pub const OP_PATH: u8 = 31;
pub const OP_DEDUP: u8 = 32;
pub const OP_FOLD: u8 = 33;
pub const OP_REPEAT: u8 = 34;
pub const OP_NOT: u8 = 35;
pub const OP_AND: u8 = 36;
pub const OP_OR: u8 = 37;
pub const OP_SUM: u8 = 38;
pub const OP_MEAN: u8 = 39;
pub const OP_MAX: u8 = 40;
pub const OP_MIN: u8 = 41;
pub const OP_UNFOLD: u8 = 42;
pub const OP_AS: u8 = 43;
pub const OP_SELECT: u8 = 44;
pub const OP_RANGE: u8 = 45;
pub const OP_SKIP: u8 = 46;
pub const OP_TAIL: u8 = 47;
pub const OP_ORDER: u8 = 48;
pub const OP_SIMPLEPATH: u8 = 49;
pub const OP_CYCLICPATH: u8 = 50;
pub const OP_CHOOSE: u8 = 51;
pub const OP_GROUP: u8 = 52;
pub const OP_GROUPCOUNT: u8 = 53;
pub const OP_ID: u8 = 54;
pub const OP_LABEL: u8 = 55;
pub const OP_RANK: u8 = 56;
pub const OP_HASRANK: u8 = 57;
pub const OP_CONSTANT: u8 = 58;
pub const OP_IDENTITY: u8 = 59;
pub const OP_LOCAL: u8 = 60;

pub fn encode(plan: &LogicalPlan) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.push(VERSION);
    encode_plan(plan, &mut buf);
    buf
}

fn encode_plan(plan: &LogicalPlan, buf: &mut Vec<u8>) {
    buf.extend_from_slice(&(plan.steps.len() as u16).to_be_bytes());
    for step in &plan.steps {
        encode_step(step, buf);
    }
}

#[allow(unused_variables)]
fn encode_step(step: &LogicalStep, buf: &mut Vec<u8>) {
    match step {
        LogicalStep::Both(s) => {
            buf.push(OP_BOTH);
            buf.extend_from_slice(&(s.labels.len() as u16).to_be_bytes());
            for item in &s.labels {
                encode_smolstr(item, buf);
            }
            if let Some(v) = &s.end_vertex_ids {
                buf.push(1);
                buf.extend_from_slice(&(v.len() as u16).to_be_bytes());
                for item in v {
                    buf.extend_from_slice(&item.to_be_bytes());
                }
            } else {
                buf.push(0);
            }
        }
        LogicalStep::BothE(s) => {
            buf.push(OP_BOTHE);
            buf.extend_from_slice(&(s.labels.len() as u16).to_be_bytes());
            for item in &s.labels {
                encode_smolstr(item, buf);
            }
            if let Some(v) = &s.end_vertex_ids {
                buf.push(1);
                buf.extend_from_slice(&(v.len() as u16).to_be_bytes());
                for item in v {
                    buf.extend_from_slice(&item.to_be_bytes());
                }
            } else {
                buf.push(0);
            }
            if let Some(r) = s.rank {
                buf.push(1);
                buf.extend_from_slice(&r.to_be_bytes());
            } else {
                buf.push(0);
            }
        }
        LogicalStep::Count(s) => {
            buf.push(OP_COUNT);
        }
        LogicalStep::Degree(s) => {
            buf.push(OP_DEGREE);
            buf.push(match s.direction {
                DegreeDirection::Out => 1,
                DegreeDirection::In => 2,
                DegreeDirection::Both => 3,
            });
        }
        LogicalStep::HasLabel(s) => {
            buf.push(OP_HASLABEL);
            encode_primitive_predicate(&s.pred, buf);
        }
        LogicalStep::HasProperty(s) => {
            buf.push(OP_HASPROPERTY);
            encode_smolstr(&s.key, buf);
            encode_primitive_predicate(&s.pred, buf);
        }
        LogicalStep::In(s) => {
            buf.push(OP_IN);
            buf.extend_from_slice(&(s.labels.len() as u16).to_be_bytes());
            for item in &s.labels {
                encode_smolstr(item, buf);
            }
            if let Some(v) = &s.end_vertex_ids {
                buf.push(1);
                buf.extend_from_slice(&(v.len() as u16).to_be_bytes());
                for item in v {
                    buf.extend_from_slice(&item.to_be_bytes());
                }
            } else {
                buf.push(0);
            }
        }
        LogicalStep::InE(s) => {
            buf.push(OP_INE);
            buf.extend_from_slice(&(s.labels.len() as u16).to_be_bytes());
            for item in &s.labels {
                encode_smolstr(item, buf);
            }
            if let Some(v) = &s.end_vertex_ids {
                buf.push(1);
                buf.extend_from_slice(&(v.len() as u16).to_be_bytes());
                for item in v {
                    buf.extend_from_slice(&item.to_be_bytes());
                }
            } else {
                buf.push(0);
            }
            if let Some(r) = s.rank {
                buf.push(1);
                buf.extend_from_slice(&r.to_be_bytes());
            } else {
                buf.push(0);
            }
        }
        LogicalStep::Out(s) => {
            buf.push(OP_OUT);
            buf.extend_from_slice(&(s.labels.len() as u16).to_be_bytes());
            for item in &s.labels {
                encode_smolstr(item, buf);
            }
            if let Some(v) = &s.end_vertex_ids {
                buf.push(1);
                buf.extend_from_slice(&(v.len() as u16).to_be_bytes());
                for item in v {
                    buf.extend_from_slice(&item.to_be_bytes());
                }
            } else {
                buf.push(0);
            }
        }
        LogicalStep::OutE(s) => {
            buf.push(OP_OUTE);
            buf.extend_from_slice(&(s.labels.len() as u16).to_be_bytes());
            for item in &s.labels {
                encode_smolstr(item, buf);
            }
            if let Some(v) = &s.end_vertex_ids {
                buf.push(1);
                buf.extend_from_slice(&(v.len() as u16).to_be_bytes());
                for item in v {
                    buf.extend_from_slice(&item.to_be_bytes());
                }
            } else {
                buf.push(0);
            }
            if let Some(r) = s.rank {
                buf.push(1);
                buf.extend_from_slice(&r.to_be_bytes());
            } else {
                buf.push(0);
            }
        }
        LogicalStep::InV(s) => {
            buf.push(OP_INV);
        }
        LogicalStep::OtherV(s) => {
            buf.push(OP_OTHERV);
        }
        LogicalStep::OutV(s) => {
            buf.push(OP_OUTV);
        }
        LogicalStep::ScalarFilter(s) => {
            buf.push(OP_SCALARFILTER);
            encode_primitive_predicate(&s.pred, buf);
        }
        LogicalStep::Values(s) => {
            buf.push(OP_VALUES);
            buf.extend_from_slice(&(s.property_keys.len() as u16).to_be_bytes());
            for item in &s.property_keys {
                encode_smolstr(item, buf);
            }
        }
        LogicalStep::Properties(s) => {
            buf.push(OP_PROPERTIES);
            buf.extend_from_slice(&(s.property_keys.len() as u16).to_be_bytes());
            for item in &s.property_keys {
                encode_smolstr(item, buf);
            }
        }
        LogicalStep::Where(s) => {
            buf.push(OP_WHERE);
            encode_plan(&s.plan, buf);
        }
        LogicalStep::Union(s) => {
            buf.push(OP_UNION);
            buf.extend_from_slice(&(s.plans.len() as u16).to_be_bytes());
            for item in &s.plans {
                encode_plan(item, buf);
            }
        }
        LogicalStep::AddV(s) => {
            buf.push(OP_ADDV);
            encode_smolstr(&s.label, buf);
            if let Some(v) = s.vertex_id {
                buf.push(1);
                buf.extend_from_slice(&v.to_be_bytes());
            } else {
                buf.push(0);
            }
            buf.extend_from_slice(&(s.properties.len() as u16).to_be_bytes());
            for (k, v) in &s.properties {
                encode_smolstr(k, buf);
                encode_primitive(v, buf);
            }
        }
        LogicalStep::AddE(s) => {
            buf.push(OP_ADDE);
            encode_smolstr(&s.label, buf);
            if let Some(v) = s.out_v_id {
                buf.push(1);
                buf.extend_from_slice(&v.to_be_bytes());
            } else {
                buf.push(0);
            }
            if let Some(v) = s.in_v_id {
                buf.push(1);
                buf.extend_from_slice(&v.to_be_bytes());
            } else {
                buf.push(0);
            }
            buf.extend_from_slice(&(s.properties.len() as u16).to_be_bytes());
            for (k, v) in &s.properties {
                encode_smolstr(k, buf);
                encode_primitive(v, buf);
            }
            if let Some(r) = s.rank {
                buf.push(1);
                buf.extend_from_slice(&r.to_be_bytes());
            } else {
                buf.push(0);
            }
        }
        LogicalStep::From(s) => {
            buf.push(OP_FROM);
            buf.extend_from_slice(&s.vertex_id.to_be_bytes());
        }
        LogicalStep::To(s) => {
            buf.push(OP_TO);
            buf.extend_from_slice(&s.vertex_id.to_be_bytes());
        }
        LogicalStep::Property(s) => {
            buf.push(OP_PROPERTY);
            encode_smolstr(&s.prop_key, buf);
            encode_primitive(&s.prop_value, buf);
        }
        LogicalStep::V(s) => {
            buf.push(OP_V);
            buf.extend_from_slice(&(s.ids.len() as u16).to_be_bytes());
            for item in &s.ids {
                buf.extend_from_slice(&item.to_be_bytes());
            }
        }
        LogicalStep::E(s) => {
            buf.push(OP_E);
            buf.extend_from_slice(&(s.keys.len() as u16).to_be_bytes());
            for item in &s.keys {
                encode_smolstr(item, buf);
            }
        }
        LogicalStep::Limit(s) => {
            buf.push(OP_LIMIT);
            buf.extend_from_slice(&s.limit.to_be_bytes());
        }
        LogicalStep::HasId(s) => {
            buf.push(OP_HASID);
            encode_primitive_predicate(&s.pred, buf);
        }
        LogicalStep::Coalesce(s) => {
            buf.push(OP_COALESCE);
            buf.extend_from_slice(&(s.plans.len() as u16).to_be_bytes());
            for item in &s.plans {
                encode_plan(item, buf);
            }
        }
        LogicalStep::EndVertexFilter(s) => {
            buf.push(OP_ENDVERTEXFILTER);
            if let Some(v) = &s.ids {
                buf.push(1);
                buf.extend_from_slice(&(v.len() as u16).to_be_bytes());
                for item in v {
                    buf.extend_from_slice(&item.to_be_bytes());
                }
            } else {
                buf.push(0);
            }
            buf.extend_from_slice(&(s.label_preds.len() as u16).to_be_bytes());
            for item in &s.label_preds {
                encode_primitive_predicate(item, buf);
            }
            buf.extend_from_slice(&(s.property_preds.len() as u16).to_be_bytes());
            for (k, v) in &s.property_preds {
                encode_smolstr(k, buf);
                encode_primitive_predicate(v, buf);
            }
        }
        LogicalStep::Drop(s) => {
            buf.push(OP_DROP);
        }
        LogicalStep::Path(s) => {
            buf.push(OP_PATH);
        }
        LogicalStep::Dedup(s) => {
            buf.push(OP_DEDUP);
        }
        LogicalStep::Fold(s) => {
            buf.push(OP_FOLD);
        }
        LogicalStep::Repeat(s) => {
            buf.push(OP_REPEAT);
            encode_plan(&s.body, buf);
            if let Some(v) = &s.until {
                buf.push(1);
                encode_plan(v, buf);
            } else {
                buf.push(0);
            }
            if let Some(v) = s.times {
                buf.push(1);
                buf.extend_from_slice(&v.to_be_bytes());
            } else {
                buf.push(0);
            }
            encode_emit_spec(&s.emit, buf);
        }
        LogicalStep::Not(s) => {
            buf.push(OP_NOT);
            encode_plan(&s.plan, buf);
        }
        LogicalStep::And(s) => {
            buf.push(OP_AND);
            buf.extend_from_slice(&(s.plans.len() as u16).to_be_bytes());
            for item in &s.plans {
                encode_plan(item, buf);
            }
        }
        LogicalStep::Or(s) => {
            buf.push(OP_OR);
            buf.extend_from_slice(&(s.plans.len() as u16).to_be_bytes());
            for item in &s.plans {
                encode_plan(item, buf);
            }
        }
        LogicalStep::Sum(s) => {
            buf.push(OP_SUM);
        }
        LogicalStep::Mean(s) => {
            buf.push(OP_MEAN);
        }
        LogicalStep::Max(s) => {
            buf.push(OP_MAX);
        }
        LogicalStep::Min(s) => {
            buf.push(OP_MIN);
        }
        LogicalStep::Unfold(s) => {
            buf.push(OP_UNFOLD);
        }
        LogicalStep::As(s) => {
            buf.push(OP_AS);
            buf.extend_from_slice(&(s.labels.len() as u16).to_be_bytes());
            for item in &s.labels {
                encode_smolstr(item, buf);
            }
        }
        LogicalStep::Select(s) => {
            buf.push(OP_SELECT);
            buf.extend_from_slice(&(s.labels.len() as u16).to_be_bytes());
            for item in &s.labels {
                encode_smolstr(item, buf);
            }
        }
        LogicalStep::Range(s) => {
            buf.push(OP_RANGE);
            buf.extend_from_slice(&s.lo.to_be_bytes());
            buf.extend_from_slice(&s.hi.to_be_bytes());
        }
        LogicalStep::Skip(s) => {
            buf.push(OP_SKIP);
            buf.extend_from_slice(&s.n.to_be_bytes());
        }
        LogicalStep::Tail(s) => {
            buf.push(OP_TAIL);
            buf.extend_from_slice(&s.n.to_be_bytes());
        }
        LogicalStep::Order(s) => {
            buf.push(OP_ORDER);
            buf.extend_from_slice(&(s.keys.len() as u16).to_be_bytes());
            for k in &s.keys {
                match &k.spec {
                    OrderKeySpec::Value => buf.push(0),
                    OrderKeySpec::Property(p) => {
                        buf.push(1);
                        encode_smolstr(p, buf);
                    }
                }
                match k.order {
                    Order::Asc => buf.push(0),
                    Order::Desc => buf.push(1),
                }
            }
        }
        LogicalStep::SimplePath(s) => {
            buf.push(OP_SIMPLEPATH);
        }
        LogicalStep::CyclicPath(s) => {
            buf.push(OP_CYCLICPATH);
        }
        LogicalStep::Choose(s) => {
            buf.push(OP_CHOOSE);
            encode_plan(&s.predicate, buf);
            encode_plan(&s.true_choice, buf);
            if let Some(v) = &s.false_choice {
                buf.push(1);
                encode_plan(v, buf);
            } else {
                buf.push(0);
            }
        }
        LogicalStep::Group(s) => {
            buf.push(OP_GROUP);
            if let Some(v) = &s.key {
                buf.push(1);
                encode_smolstr(v, buf);
            } else {
                buf.push(0);
            }
        }
        LogicalStep::GroupCount(s) => {
            buf.push(OP_GROUPCOUNT);
            if let Some(v) = &s.key {
                buf.push(1);
                encode_smolstr(v, buf);
            } else {
                buf.push(0);
            }
        }
        LogicalStep::Id(s) => {
            buf.push(OP_ID);
        }
        LogicalStep::Label(s) => {
            buf.push(OP_LABEL);
        }
        LogicalStep::Rank(s) => {
            buf.push(OP_RANK);
        }
        LogicalStep::HasRank(s) => {
            buf.push(OP_HASRANK);
            encode_primitive_predicate(&s.pred, buf);
        }
        LogicalStep::Constant(s) => {
            buf.push(OP_CONSTANT);
            encode_primitive(&s.value, buf);
        }
        LogicalStep::Identity(s) => {
            buf.push(OP_IDENTITY);
        }
        LogicalStep::Local(s) => {
            buf.push(OP_LOCAL);
            encode_plan(&s.plan, buf);
        }
    }
}

pub fn decode(bytes: &[u8]) -> Result<LogicalPlan, StoreError> {
    if bytes.is_empty() || bytes[0] != VERSION {
        return Err(StoreError::UnsupportedOperation("Unknown version".into()));
    }
    let mut offset = 1;
    decode_plan(bytes, &mut offset)
}

fn decode_plan(bytes: &[u8], offset: &mut usize) -> Result<LogicalPlan, StoreError> {
    let step_count = read_u16(bytes, offset)?;
    let mut steps = Vec::with_capacity(step_count as usize);
    for _ in 0..step_count {
        steps.push(decode_step(bytes, offset)?);
    }
    Ok(LogicalPlan { steps })
}

#[allow(unused_variables)]
fn decode_step(bytes: &[u8], offset: &mut usize) -> Result<LogicalStep, StoreError> {
    let op = read_u8(bytes, offset)?;
    match op {
        OP_BOTH => Ok(LogicalStep::Both(BothStep {
            labels: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_smolstr(bytes, offset)?);
                }
                v
            },
            end_vertex_ids: if read_u8(bytes, offset)? == 1 {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_i64(bytes, offset)?);
                }
                Some(v)
            } else {
                None
            },
        })),
        OP_BOTHE => Ok(LogicalStep::BothE(BothEStep {
            labels: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_smolstr(bytes, offset)?);
                }
                v
            },
            end_vertex_ids: if read_u8(bytes, offset)? == 1 {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_i64(bytes, offset)?);
                }
                Some(v)
            } else {
                None
            },
            rank: if read_u8(bytes, offset)? == 1 { Some(read_u16(bytes, offset)?) } else { None },
        })),
        OP_COUNT => Ok(LogicalStep::Count(CountStep {})),
        OP_DEGREE => Ok(LogicalStep::Degree(DegreeStep {
            direction: match read_u8(bytes, offset)? {
                1 => DegreeDirection::Out,
                2 => DegreeDirection::In,
                _ => DegreeDirection::Both,
            },
        })),
        OP_HASLABEL => Ok(LogicalStep::HasLabel(HasLabelStep { pred: decode_primitive_predicate(bytes, offset)? })),
        OP_HASPROPERTY => Ok(LogicalStep::HasProperty(HasPropertyStep {
            key: read_smolstr(bytes, offset)?,
            pred: decode_primitive_predicate(bytes, offset)?,
        })),
        OP_IN => Ok(LogicalStep::In(InStep {
            labels: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_smolstr(bytes, offset)?);
                }
                v
            },
            end_vertex_ids: if read_u8(bytes, offset)? == 1 {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_i64(bytes, offset)?);
                }
                Some(v)
            } else {
                None
            },
        })),
        OP_INE => Ok(LogicalStep::InE(InEStep {
            labels: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_smolstr(bytes, offset)?);
                }
                v
            },
            end_vertex_ids: if read_u8(bytes, offset)? == 1 {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_i64(bytes, offset)?);
                }
                Some(v)
            } else {
                None
            },
            rank: if read_u8(bytes, offset)? == 1 { Some(read_u16(bytes, offset)?) } else { None },
        })),
        OP_OUT => Ok(LogicalStep::Out(OutStep {
            labels: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_smolstr(bytes, offset)?);
                }
                v
            },
            end_vertex_ids: if read_u8(bytes, offset)? == 1 {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_i64(bytes, offset)?);
                }
                Some(v)
            } else {
                None
            },
        })),
        OP_OUTE => Ok(LogicalStep::OutE(OutEStep {
            labels: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_smolstr(bytes, offset)?);
                }
                v
            },
            end_vertex_ids: if read_u8(bytes, offset)? == 1 {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_i64(bytes, offset)?);
                }
                Some(v)
            } else {
                None
            },
            rank: if read_u8(bytes, offset)? == 1 { Some(read_u16(bytes, offset)?) } else { None },
        })),
        OP_INV => Ok(LogicalStep::InV(InVStep {})),
        OP_OTHERV => Ok(LogicalStep::OtherV(OtherVStep {})),
        OP_OUTV => Ok(LogicalStep::OutV(OutVStep {})),
        OP_SCALARFILTER => {
            Ok(LogicalStep::ScalarFilter(ScalarFilterStep { pred: decode_primitive_predicate(bytes, offset)? }))
        }
        OP_VALUES => Ok(LogicalStep::Values(ValuesStep {
            property_keys: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_smolstr(bytes, offset)?);
                }
                v
            },
        })),
        OP_PROPERTIES => Ok(LogicalStep::Properties(PropertiesStep {
            property_keys: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_smolstr(bytes, offset)?);
                }
                v
            },
        })),
        OP_WHERE => Ok(LogicalStep::Where(WhereStep { plan: decode_plan(bytes, offset)? })),
        OP_UNION => Ok(LogicalStep::Union(UnionStep {
            plans: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(decode_plan(bytes, offset)?);
                }
                v
            },
        })),
        OP_ADDV => Ok(LogicalStep::AddV(AddVStep {
            label: read_smolstr(bytes, offset)?,
            vertex_id: if read_u8(bytes, offset)? == 1 { Some(read_i64(bytes, offset)?) } else { None },
            properties: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push((read_smolstr(bytes, offset)?, decode_primitive(bytes, offset)?));
                }
                v
            },
        })),
        OP_ADDE => Ok(LogicalStep::AddE(AddEStep {
            label: read_smolstr(bytes, offset)?,
            out_v_id: if read_u8(bytes, offset)? == 1 { Some(read_i64(bytes, offset)?) } else { None },
            in_v_id: if read_u8(bytes, offset)? == 1 { Some(read_i64(bytes, offset)?) } else { None },
            properties: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push((read_smolstr(bytes, offset)?, decode_primitive(bytes, offset)?));
                }
                v
            },
            rank: if read_u8(bytes, offset)? == 1 { Some(read_u16(bytes, offset)?) } else { None },
        })),
        OP_FROM => Ok(LogicalStep::From(FromStep { vertex_id: read_i64(bytes, offset)? })),
        OP_TO => Ok(LogicalStep::To(ToStep { vertex_id: read_i64(bytes, offset)? })),
        OP_PROPERTY => Ok(LogicalStep::Property(PropertyStep {
            prop_key: read_smolstr(bytes, offset)?,
            prop_value: decode_primitive(bytes, offset)?,
        })),
        OP_V => Ok(LogicalStep::V(VStep {
            ids: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_i64(bytes, offset)?);
                }
                v
            },
        })),
        OP_E => Ok(LogicalStep::E(EStep {
            keys: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_smolstr(bytes, offset)?.to_string());
                }
                v
            },
        })),
        OP_LIMIT => Ok(LogicalStep::Limit(LimitStep { limit: read_i64(bytes, offset)? })),
        OP_HASID => Ok(LogicalStep::HasId(HasIdStep { pred: decode_primitive_predicate(bytes, offset)? })),
        OP_COALESCE => Ok(LogicalStep::Coalesce(CoalesceStep {
            plans: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = Vec::new();
                for _ in 0..c {
                    v.push(decode_plan(bytes, offset)?);
                }
                v
            },
        })),
        OP_ENDVERTEXFILTER => Err(StoreError::UnsupportedOperation("Internal only: OP_ENDVERTEXFILTER".into())),
        OP_DROP => Ok(LogicalStep::Drop(DropStep {})),
        OP_PATH => Ok(LogicalStep::Path(PathStep {})),
        OP_DEDUP => Ok(LogicalStep::Dedup(DedupStep {})),
        OP_FOLD => Ok(LogicalStep::Fold(FoldStep {})),
        OP_REPEAT => Ok(LogicalStep::Repeat(RepeatStep {
            body: decode_plan(bytes, offset)?,
            until: if read_u8(bytes, offset)? == 1 { Some(decode_plan(bytes, offset)?) } else { None },
            times: if read_u8(bytes, offset)? == 1 { Some(read_i64(bytes, offset)?) } else { None },
            emit: decode_emit_spec(bytes, offset)?,
        })),
        OP_NOT => Ok(LogicalStep::Not(NotStep { plan: decode_plan(bytes, offset)? })),
        OP_AND => Ok(LogicalStep::And(AndStep {
            plans: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = Vec::new();
                for _ in 0..c {
                    v.push(decode_plan(bytes, offset)?);
                }
                v
            },
        })),
        OP_OR => Ok(LogicalStep::Or(OrStep {
            plans: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = Vec::new();
                for _ in 0..c {
                    v.push(decode_plan(bytes, offset)?);
                }
                v
            },
        })),
        OP_SUM => Ok(LogicalStep::Sum(SumStep {})),
        OP_MEAN => Ok(LogicalStep::Mean(MeanStep {})),
        OP_MAX => Ok(LogicalStep::Max(MaxStep {})),
        OP_MIN => Ok(LogicalStep::Min(MinStep {})),
        OP_UNFOLD => Ok(LogicalStep::Unfold(UnfoldStep {})),
        OP_AS => Ok(LogicalStep::As(AsStep {
            labels: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_smolstr(bytes, offset)?);
                }
                v
            },
        })),
        OP_SELECT => Ok(LogicalStep::Select(SelectStep {
            labels: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    v.push(read_smolstr(bytes, offset)?);
                }
                v
            },
        })),
        OP_RANGE => Ok(LogicalStep::Range(RangeStep { lo: read_i64(bytes, offset)?, hi: read_i64(bytes, offset)? })),
        OP_SKIP => Ok(LogicalStep::Skip(SkipStep { n: read_i64(bytes, offset)? })),
        OP_TAIL => Ok(LogicalStep::Tail(TailStep { n: read_i64(bytes, offset)? })),
        OP_ORDER => Ok(LogicalStep::Order(OrderStep {
            keys: {
                let c = read_u16(bytes, offset)? as usize;
                let mut v = smallvec::smallvec![];
                for _ in 0..c {
                    let spec = match read_u8(bytes, offset)? {
                        0 => OrderKeySpec::Value,
                        1 => OrderKeySpec::Property(read_smolstr(bytes, offset)?),
                        _ => return Err(StoreError::UnsupportedOperation("Unknown OrderKeySpec".into())),
                    };
                    let order = match read_u8(bytes, offset)? {
                        0 => Order::Asc,
                        1 => Order::Desc,
                        _ => return Err(StoreError::UnsupportedOperation("Unknown Order".into())),
                    };
                    v.push(OrderKey { spec, order });
                }
                v
            },
        })),
        OP_SIMPLEPATH => Ok(LogicalStep::SimplePath(SimplePathStep {})),
        OP_CYCLICPATH => Ok(LogicalStep::CyclicPath(CyclicPathStep {})),
        OP_CHOOSE => Ok(LogicalStep::Choose(ChooseStep {
            predicate: decode_plan(bytes, offset)?,
            true_choice: decode_plan(bytes, offset)?,
            false_choice: if read_u8(bytes, offset)? == 1 { Some(decode_plan(bytes, offset)?) } else { None },
        })),
        OP_GROUP => Ok(LogicalStep::Group(GroupStep {
            key: if read_u8(bytes, offset)? == 1 { Some(read_smolstr(bytes, offset)?) } else { None },
        })),
        OP_GROUPCOUNT => Ok(LogicalStep::GroupCount(GroupCountStep {
            key: if read_u8(bytes, offset)? == 1 { Some(read_smolstr(bytes, offset)?) } else { None },
        })),
        OP_ID => Ok(LogicalStep::Id(IdStep {})),
        OP_LABEL => Ok(LogicalStep::Label(LabelStep {})),
        OP_RANK => Ok(LogicalStep::Rank(RankStep {})),
        OP_HASRANK => Ok(LogicalStep::HasRank(HasRankStep { pred: decode_primitive_predicate(bytes, offset)? })),
        OP_CONSTANT => Ok(LogicalStep::Constant(ConstantStep { value: decode_primitive(bytes, offset)? })),
        OP_IDENTITY => Ok(LogicalStep::Identity(IdentityStep {})),
        OP_LOCAL => Ok(LogicalStep::Local(LocalStep { plan: decode_plan(bytes, offset)? })),
        _ => Err(StoreError::UnsupportedOperation(format!("Unknown opcode 0x{:02x}", op))),
    }
}

fn read_u8(bytes: &[u8], offset: &mut usize) -> Result<u8, StoreError> {
    if *offset >= bytes.len() {
        return Err(StoreError::UnsupportedOperation("EOF".into()));
    }
    let v = bytes[*offset];
    *offset += 1;
    Ok(v)
}
fn read_u16(bytes: &[u8], offset: &mut usize) -> Result<u16, StoreError> {
    if *offset + 2 > bytes.len() {
        return Err(StoreError::UnsupportedOperation("EOF".into()));
    }
    let v = u16::from_be_bytes([bytes[*offset], bytes[*offset + 1]]);
    *offset += 2;
    Ok(v)
}
fn read_i64(bytes: &[u8], offset: &mut usize) -> Result<i64, StoreError> {
    if *offset + 8 > bytes.len() {
        return Err(StoreError::UnsupportedOperation("EOF".into()));
    }
    let v = i64::from_be_bytes(bytes[*offset..*offset + 8].try_into().unwrap());
    *offset += 8;
    Ok(v)
}
fn encode_smolstr(s: &str, buf: &mut Vec<u8>) {
    let b = s.as_bytes();
    buf.extend_from_slice(&(b.len() as u16).to_be_bytes());
    buf.extend_from_slice(b);
}
fn read_smolstr(bytes: &[u8], offset: &mut usize) -> Result<SmolStr, StoreError> {
    let l = read_u16(bytes, offset)? as usize;
    if *offset + l > bytes.len() {
        return Err(StoreError::UnsupportedOperation("EOF".into()));
    }
    let s = std::str::from_utf8(&bytes[*offset..*offset + l])
        .map_err(|_| StoreError::UnsupportedOperation("Invalid UTF-8".into()))?;
    *offset += l;
    Ok(SmolStr::new(s))
}
fn encode_primitive(p: &Primitive, buf: &mut Vec<u8>) {
    match p {
        Primitive::Null => buf.push(0),
        Primitive::Bool(b) => {
            buf.push(1);
            buf.push(*b as u8);
        }
        Primitive::Int32(v) => {
            buf.push(2);
            buf.extend_from_slice(&v.to_be_bytes());
        }
        Primitive::Int64(v) => {
            buf.push(3);
            buf.extend_from_slice(&v.to_be_bytes());
        }
        Primitive::UInt16(v) => {
            buf.push(4);
            buf.extend_from_slice(&v.to_be_bytes());
        }
        Primitive::Float32(v) => {
            buf.push(5);
            buf.extend_from_slice(&v.to_be_bytes());
        }
        Primitive::Float64(v) => {
            buf.push(6);
            buf.extend_from_slice(&v.to_be_bytes());
        }
        Primitive::String(s) => {
            buf.push(7);
            encode_smolstr(s, buf);
        }
        Primitive::Uuid(v) => {
            buf.push(8);
            buf.extend_from_slice(&v.to_be_bytes());
        }
        Primitive::Bytes(b) => {
            buf.push(9);
            buf.extend_from_slice(&(b.len() as u32).to_be_bytes());
            buf.extend_from_slice(b);
        }
    }
}
fn decode_primitive(bytes: &[u8], offset: &mut usize) -> Result<Primitive, StoreError> {
    let tag = read_u8(bytes, offset)?;
    match tag {
        0 => Ok(Primitive::Null),
        1 => Ok(Primitive::Bool(read_u8(bytes, offset)? != 0)),
        2 => {
            if *offset + 4 > bytes.len() {
                return Err(StoreError::UnsupportedOperation("EOF".into()));
            }
            let v = i32::from_be_bytes(bytes[*offset..*offset + 4].try_into().unwrap());
            *offset += 4;
            Ok(Primitive::Int32(v))
        }
        3 => Ok(Primitive::Int64(read_i64(bytes, offset)?)),
        4 => Ok(Primitive::UInt16(read_u16(bytes, offset)?)),
        5 => {
            if *offset + 4 > bytes.len() {
                return Err(StoreError::UnsupportedOperation("EOF".into()));
            }
            let v = f32::from_be_bytes(bytes[*offset..*offset + 4].try_into().unwrap());
            *offset += 4;
            Ok(Primitive::Float32(v))
        }
        6 => {
            if *offset + 8 > bytes.len() {
                return Err(StoreError::UnsupportedOperation("EOF".into()));
            }
            let v = f64::from_be_bytes(bytes[*offset..*offset + 8].try_into().unwrap());
            *offset += 8;
            Ok(Primitive::Float64(v))
        }
        7 => Ok(Primitive::String(read_smolstr(bytes, offset)?)),
        8 => {
            if *offset + 16 > bytes.len() {
                return Err(StoreError::UnsupportedOperation("EOF".into()));
            }
            let v = u128::from_be_bytes(bytes[*offset..*offset + 16].try_into().unwrap());
            *offset += 16;
            Ok(Primitive::Uuid(v))
        }
        9 => {
            if *offset + 4 > bytes.len() {
                return Err(StoreError::UnsupportedOperation("EOF".into()));
            }
            let len = u32::from_be_bytes(bytes[*offset..*offset + 4].try_into().unwrap()) as usize;
            *offset += 4;
            if *offset + len > bytes.len() {
                return Err(StoreError::UnsupportedOperation("EOF".into()));
            }
            let v = bytes[*offset..*offset + len].to_vec();
            *offset += len;
            Ok(Primitive::Bytes(v))
        }
        _ => Err(StoreError::UnsupportedOperation(format!("Unknown primitive tag {}", tag))),
    }
}
fn encode_primitive_predicate(p: &PrimitivePredicate, buf: &mut Vec<u8>) {
    match p {
        PrimitivePredicate::Eq(v) => {
            buf.push(0);
            encode_primitive(v, buf);
        }
        PrimitivePredicate::Ne(v) => {
            buf.push(1);
            encode_primitive(v, buf);
        }
        PrimitivePredicate::Gt(v) => {
            buf.push(2);
            encode_primitive(v, buf);
        }
        PrimitivePredicate::Gte(v) => {
            buf.push(3);
            encode_primitive(v, buf);
        }
        PrimitivePredicate::Lt(v) => {
            buf.push(4);
            encode_primitive(v, buf);
        }
        PrimitivePredicate::Lte(v) => {
            buf.push(5);
            encode_primitive(v, buf);
        }
        PrimitivePredicate::Between(lo, hi) => {
            buf.push(6);
            encode_primitive(lo, buf);
            encode_primitive(hi, buf);
        }
        PrimitivePredicate::Within(vs) => {
            buf.push(7);
            buf.extend_from_slice(&(vs.len() as u16).to_be_bytes());
            for v in vs {
                encode_primitive(v, buf);
            }
        }
        PrimitivePredicate::Without(vs) => {
            buf.push(8);
            buf.extend_from_slice(&(vs.len() as u16).to_be_bytes());
            for v in vs {
                encode_primitive(v, buf);
            }
        }
    }
}
fn decode_primitive_predicate(bytes: &[u8], offset: &mut usize) -> Result<PrimitivePredicate, StoreError> {
    let tag = read_u8(bytes, offset)?;
    match tag {
        0 => Ok(PrimitivePredicate::Eq(decode_primitive(bytes, offset)?)),
        1 => Ok(PrimitivePredicate::Ne(decode_primitive(bytes, offset)?)),
        2 => Ok(PrimitivePredicate::Gt(decode_primitive(bytes, offset)?)),
        3 => Ok(PrimitivePredicate::Gte(decode_primitive(bytes, offset)?)),
        4 => Ok(PrimitivePredicate::Lt(decode_primitive(bytes, offset)?)),
        5 => Ok(PrimitivePredicate::Lte(decode_primitive(bytes, offset)?)),
        6 => Ok(PrimitivePredicate::Between(decode_primitive(bytes, offset)?, decode_primitive(bytes, offset)?)),
        7 => {
            let len = read_u16(bytes, offset)? as usize;
            let mut vs = Vec::with_capacity(len);
            for _ in 0..len {
                vs.push(decode_primitive(bytes, offset)?);
            }
            Ok(PrimitivePredicate::Within(vs))
        }
        8 => {
            let len = read_u16(bytes, offset)? as usize;
            let mut vs = Vec::with_capacity(len);
            for _ in 0..len {
                vs.push(decode_primitive(bytes, offset)?);
            }
            Ok(PrimitivePredicate::Without(vs))
        }
        _ => Err(StoreError::UnsupportedOperation(format!("Unknown predicate tag {}", tag))),
    }
}
#[allow(clippy::ptr_arg)]
fn encode_emit_spec(p: &EmitSpec, buf: &mut Vec<u8>) {
    match p {
        EmitSpec::Never => buf.push(0),
        EmitSpec::Always => buf.push(1),
        EmitSpec::If(plan) => {
            buf.push(2);
            encode_plan(plan, buf);
        }
    }
}
fn decode_emit_spec(bytes: &[u8], offset: &mut usize) -> Result<EmitSpec, StoreError> {
    let tag = read_u8(bytes, offset)?;
    match tag {
        0 => Ok(EmitSpec::Never),
        1 => Ok(EmitSpec::Always),
        2 => {
            let plan = decode_plan(bytes, offset)?;
            Ok(EmitSpec::If(plan))
        }
        _ => Err(StoreError::UnsupportedOperation(format!("Unknown EmitSpec tag {}", tag))),
    }
}
pub fn execute_read(graph: &mut dyn GraphCtx, bytes: &[u8]) -> Result<Vec<Value>, StoreError> {
    let plan = decode(bytes)?;
    let traversal = crate::gremlin::traversal::ReadTraversal::from_plan(plan, graph);
    traversal.to_list()
}
pub fn execute_write(graph: &mut dyn GraphCtx, bytes: &[u8]) -> Result<Vec<Value>, StoreError> {
    let plan = decode(bytes)?;
    let traversal = crate::gremlin::traversal::WriteTraversal::from_plan(plan, graph);
    traversal.to_list()
}

pub const TAG_NULL: u8 = 0;
pub const TAG_BOOL: u8 = 1;
pub const TAG_INT32: u8 = 2;
pub const TAG_INT64: u8 = 3;
pub const TAG_FLOAT32: u8 = 4;
pub const TAG_FLOAT64: u8 = 5;
pub const TAG_STRING: u8 = 6;
pub const TAG_UUID: u8 = 7;
pub const TAG_BYTES: u8 = 8;
pub const TAG_VERTEX: u8 = 9;
pub const TAG_EDGE: u8 = 10;
pub const TAG_PATH: u8 = 11;
pub const TAG_LIST: u8 = 12;
pub const TAG_MAP: u8 = 13;
pub const TAG_UINT16: u8 = 14;
pub const TAG_PROPERTY: u8 = 15;

fn encode_value(v: &Value, buf: &mut Vec<u8>) {
    match v {
        Value::Null => buf.push(TAG_NULL),
        Value::Bool(b) => {
            buf.push(TAG_BOOL);
            buf.push(*b as u8);
        }
        Value::Int32(i) => {
            buf.push(TAG_INT32);
            buf.extend_from_slice(&i.to_be_bytes());
        }
        Value::Int64(i) => {
            buf.push(TAG_INT64);
            buf.extend_from_slice(&i.to_be_bytes());
        }
        Value::UInt16(i) => {
            buf.push(TAG_UINT16);
            buf.extend_from_slice(&i.to_be_bytes());
        }
        Value::Float32(f) => {
            buf.push(TAG_FLOAT32);
            buf.extend_from_slice(&f.to_bits().to_be_bytes());
        }
        Value::Float64(f) => {
            buf.push(TAG_FLOAT64);
            buf.extend_from_slice(&f.to_bits().to_be_bytes());
        }
        Value::String(s) => {
            buf.push(TAG_STRING);
            encode_smolstr(s, buf);
        }
        Value::Uuid(u) => {
            buf.push(TAG_UUID);
            buf.extend_from_slice(&u.to_be_bytes());
        }
        Value::Bytes(b) => {
            buf.push(TAG_BYTES);
            buf.extend_from_slice(&(b.len() as u32).to_be_bytes());
            buf.extend_from_slice(b);
        }
        Value::Vertex(v) => {
            buf.push(TAG_VERTEX);
            buf.extend_from_slice(&v.id.to_be_bytes());
            encode_smolstr(&v.label, buf);
            buf.extend_from_slice(&(v.properties.len() as u16).to_be_bytes());
            for (k, vals) in &v.properties {
                encode_smolstr(k, buf);
                buf.extend_from_slice(&(vals.len() as u16).to_be_bytes());
                for val in vals {
                    encode_value(val, buf);
                }
            }
        }
        Value::Edge(e) => {
            buf.push(TAG_EDGE);
            encode_smolstr(&e.id, buf);
            buf.extend_from_slice(&e.out_v.to_be_bytes());
            buf.extend_from_slice(&e.in_v.to_be_bytes());
            encode_smolstr(&e.label, buf);
            buf.extend_from_slice(&e.rank.to_be_bytes());
            buf.extend_from_slice(&(e.properties.len() as u16).to_be_bytes());
            for (k, val) in &e.properties {
                encode_smolstr(k, buf);
                encode_value(val, buf);
            }
        }
        Value::Path(p) => {
            buf.push(TAG_PATH);
            buf.extend_from_slice(&(p.objects.len() as u16).to_be_bytes());
            for obj in &p.objects {
                encode_value(obj, buf);
            }
            buf.extend_from_slice(&(p.labels.len() as u16).to_be_bytes());
            for ls in &p.labels {
                buf.extend_from_slice(&(ls.len() as u16).to_be_bytes());
                for l in ls {
                    encode_smolstr(l, buf);
                }
            }
        }
        Value::List(l) => {
            buf.push(TAG_LIST);
            buf.extend_from_slice(&(l.len() as u16).to_be_bytes());
            for val in l {
                encode_value(val, buf);
            }
        }
        Value::Map(m) => {
            buf.push(TAG_MAP);
            buf.extend_from_slice(&(m.entries.len() as u16).to_be_bytes());
            for (k, val) in &m.entries {
                encode_value(k, buf);
                encode_value(val, buf);
            }
        }
        Value::Property(p) => {
            buf.push(TAG_PROPERTY);
            encode_smolstr(&p.key, buf);
            encode_value(&p.value, buf);
        }
    }
}

fn decode_value(bytes: &[u8], offset: &mut usize) -> Result<Value, StoreError> {
    let tag = read_u8(bytes, offset)?;
    match tag {
        TAG_NULL => Ok(Value::Null),
        TAG_BOOL => Ok(Value::Bool(read_u8(bytes, offset)? == 1)),
        TAG_INT32 => {
            if *offset + 4 > bytes.len() {
                return Err(StoreError::UnsupportedOperation("EOF".into()));
            }
            let v = i32::from_be_bytes(bytes[*offset..*offset + 4].try_into().unwrap());
            *offset += 4;
            Ok(Value::Int32(v))
        }
        TAG_INT64 => Ok(Value::Int64(read_i64(bytes, offset)?)),
        TAG_UINT16 => Ok(Value::UInt16(read_u16(bytes, offset)?)),
        TAG_FLOAT32 => {
            if *offset + 4 > bytes.len() {
                return Err(StoreError::UnsupportedOperation("EOF".into()));
            }
            let v = f32::from_bits(u32::from_be_bytes(bytes[*offset..*offset + 4].try_into().unwrap()));
            *offset += 4;
            Ok(Value::Float32(v))
        }
        TAG_FLOAT64 => {
            if *offset + 8 > bytes.len() {
                return Err(StoreError::UnsupportedOperation("EOF".into()));
            }
            let v = f64::from_bits(u64::from_be_bytes(bytes[*offset..*offset + 8].try_into().unwrap()));
            *offset += 8;
            Ok(Value::Float64(v))
        }
        TAG_STRING => Ok(Value::String(read_smolstr(bytes, offset)?.to_string())),
        TAG_UUID => {
            if *offset + 16 > bytes.len() {
                return Err(StoreError::UnsupportedOperation("EOF".into()));
            }
            let v = u128::from_be_bytes(bytes[*offset..*offset + 16].try_into().unwrap());
            *offset += 16;
            Ok(Value::Uuid(v))
        }
        TAG_BYTES => {
            if *offset + 4 > bytes.len() {
                return Err(StoreError::UnsupportedOperation("EOF".into()));
            }
            let len = u32::from_be_bytes([bytes[*offset], bytes[*offset + 1], bytes[*offset + 2], bytes[*offset + 3]])
                as usize;
            *offset += 4;
            if *offset + len > bytes.len() {
                return Err(StoreError::UnsupportedOperation("EOF".into()));
            }
            let v = bytes[*offset..*offset + len].to_vec();
            *offset += len;
            Ok(Value::Bytes(v))
        }
        TAG_VERTEX => {
            let id = read_i64(bytes, offset)?;
            let label = read_smolstr(bytes, offset)?;
            let prop_len = read_u16(bytes, offset)? as usize;
            let mut properties = std::collections::HashMap::new();
            for _ in 0..prop_len {
                let k = read_smolstr(bytes, offset)?;
                let vals_len = read_u16(bytes, offset)? as usize;
                let mut vals = Vec::with_capacity(vals_len);
                for _ in 0..vals_len {
                    vals.push(decode_value(bytes, offset)?);
                }
                properties.insert(k, vals);
            }
            Ok(Value::Vertex(crate::gremlin::value::Vertex { id, label, properties }))
        }
        TAG_EDGE => {
            let id = read_smolstr(bytes, offset)?;
            let out_v = read_i64(bytes, offset)?;
            let in_v = read_i64(bytes, offset)?;
            let label = read_smolstr(bytes, offset)?;
            let rank = read_u16(bytes, offset)?;
            let prop_len = read_u16(bytes, offset)? as usize;
            let mut properties = std::collections::HashMap::new();
            for _ in 0..prop_len {
                properties.insert(read_smolstr(bytes, offset)?, decode_value(bytes, offset)?);
            }
            Ok(Value::Edge(crate::gremlin::value::Edge { id, out_v, in_v, label, rank, properties }))
        }
        TAG_PATH => {
            let obj_len = read_u16(bytes, offset)? as usize;
            let mut objects = Vec::with_capacity(obj_len);
            for _ in 0..obj_len {
                objects.push(decode_value(bytes, offset)?);
            }
            let lbl_len = read_u16(bytes, offset)? as usize;
            let mut labels = Vec::with_capacity(lbl_len);
            for _ in 0..lbl_len {
                let count = read_u16(bytes, offset)? as usize;
                let mut l = Vec::with_capacity(count);
                for _ in 0..count {
                    l.push(read_smolstr(bytes, offset)?.to_string());
                }
                labels.push(l);
            }
            Ok(Value::Path(crate::gremlin::value::Path { objects, labels }))
        }
        TAG_LIST => {
            let len = read_u16(bytes, offset)? as usize;
            let mut v = Vec::with_capacity(len);
            for _ in 0..len {
                v.push(decode_value(bytes, offset)?);
            }
            Ok(Value::List(v))
        }
        TAG_MAP => {
            let len = read_u16(bytes, offset)? as usize;
            let mut entries = Vec::with_capacity(len);
            for _ in 0..len {
                entries.push((decode_value(bytes, offset)?, decode_value(bytes, offset)?));
            }
            Ok(Value::Map(crate::gremlin::value::Map { entries }))
        }
        TAG_PROPERTY => {
            let key = read_smolstr(bytes, offset)?;
            let value = Box::new(decode_value(bytes, offset)?);
            Ok(Value::Property(crate::gremlin::value::Property { key, value }))
        }
        _ => Err(StoreError::UnsupportedOperation(format!("Unknown Value tag {}", tag))),
    }
}

pub fn encode_response(values: &[Value]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.push(VERSION);
    buf.extend_from_slice(&(values.len() as u32).to_be_bytes());
    for val in values {
        encode_value(val, &mut buf);
    }
    buf
}

pub fn decode_response(bytes: &[u8]) -> Result<Vec<Value>, StoreError> {
    if bytes.is_empty() || bytes[0] != VERSION {
        return Err(StoreError::UnsupportedOperation("Unknown version".into()));
    }
    let mut offset = 1;
    if offset + 4 > bytes.len() {
        return Err(StoreError::UnsupportedOperation("EOF".into()));
    }
    let row_count = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;
    let mut values = Vec::with_capacity(row_count);
    for _ in 0..row_count {
        values.push(decode_value(bytes, &mut offset)?);
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_simple() {
        let plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(VStep { ids: smallvec::smallvec![1, 2] }),
                LogicalStep::Out(OutStep { labels: smallvec::smallvec!["knows".into()], end_vertex_ids: None }),
                LogicalStep::Count(CountStep {}),
            ],
        };
        let encoded = encode(&plan);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.steps.len(), 3);
        match &decoded.steps[0] {
            LogicalStep::V(v) => assert_eq!(v.ids.as_slice(), &[1, 2]),
            _ => panic!("Expected VStep"),
        }
    }

    #[test]
    fn test_roundtrip_subplan() {
        let plan = LogicalPlan {
            steps: vec![LogicalStep::Where(WhereStep {
                plan: LogicalPlan {
                    steps: vec![LogicalStep::Out(OutStep {
                        labels: smallvec::smallvec!["knows".into()],
                        end_vertex_ids: None,
                    })],
                },
            })],
        };
        let encoded = encode(&plan);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.steps.len(), 1);
        match &decoded.steps[0] {
            LogicalStep::Where(w) => assert_eq!(w.plan.steps.len(), 1),
            _ => panic!("Expected WhereStep"),
        }
    }

    #[test]
    fn test_roundtrip_predicate() {
        let plan = LogicalPlan {
            steps: vec![LogicalStep::HasProperty(HasPropertyStep {
                key: "age".into(),
                pred: PrimitivePredicate::Gt(Primitive::Int32(30)),
            })],
        };
        let encoded = encode(&plan);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.steps.len(), 1);
        match &decoded.steps[0] {
            LogicalStep::HasProperty(h) => {
                assert_eq!(h.key, "age");
                match &h.pred {
                    PrimitivePredicate::Gt(Primitive::Int32(v)) => assert_eq!(*v, 30),
                    _ => panic!("Expected Gt(30)"),
                }
            }
            _ => panic!("Expected HasPropertyStep"),
        }
    }

    #[test]
    fn test_roundtrip_response() {
        let encoded = super::encode_response(&[Value::Int64(1), Value::String("hi".into())]);
        let decoded = super::decode_response(&encoded).unwrap();
        assert_eq!(decoded.len(), 2);
        match &decoded[0] {
            Value::Int64(v) => assert_eq!(*v, 1),
            _ => panic!("Expected Int64(1)"),
        }
        match &decoded[1] {
            Value::String(s) => assert_eq!(s, "hi"),
            _ => panic!("Expected String('hi')"),
        }
    }

    #[test]
    fn test_roundtrip_more_steps() {
        let plan = LogicalPlan {
            steps: vec![
                LogicalStep::Repeat(RepeatStep {
                    body: LogicalPlan {
                        steps: vec![LogicalStep::Out(OutStep { labels: smallvec::smallvec![], end_vertex_ids: None })],
                    },
                    until: None,
                    emit: EmitSpec::Always,
                    times: Some(3),
                }),
                LogicalStep::Choose(ChooseStep {
                    predicate: LogicalPlan { steps: vec![] },
                    true_choice: LogicalPlan { steps: vec![] },
                    false_choice: None,
                }),
                LogicalStep::AddV(AddVStep {
                    label: "person".into(),
                    vertex_id: Some(100),
                    properties: smallvec::smallvec![("name".into(), Primitive::String("Alice".into()))],
                }),
                LogicalStep::Group(GroupStep { key: None }),
                LogicalStep::Order(OrderStep {
                    keys: smallvec::smallvec![OrderKey {
                        spec: OrderKeySpec::Property("age".into()),
                        order: Order::Desc
                    }],
                }),
                LogicalStep::BothE(BothEStep {
                    labels: smallvec::smallvec!["knows".into()],
                    end_vertex_ids: None,
                    rank: Some(10),
                }),
            ],
        };
        let encoded = encode(&plan);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.steps.len(), 6);
        match &decoded.steps[0] {
            LogicalStep::Repeat(r) => assert_eq!(r.times, Some(3)),
            _ => panic!("Expected RepeatStep"),
        }
        match &decoded.steps[1] {
            LogicalStep::Choose(c) => assert!(c.false_choice.is_none()),
            _ => panic!("Expected ChooseStep"),
        }
        match &decoded.steps[2] {
            LogicalStep::AddV(a) => {
                assert_eq!(a.label, "person");
                assert_eq!(a.vertex_id, Some(100));
            }
            _ => panic!("Expected AddVStep"),
        }
        match &decoded.steps[3] {
            LogicalStep::Group(g) => assert!(g.key.is_none()),
            _ => panic!("Expected GroupStep"),
        }
        match &decoded.steps[4] {
            LogicalStep::Order(o) => {
                assert_eq!(o.keys.len(), 1);
                match &o.keys[0].spec {
                    OrderKeySpec::Property(p) => assert_eq!(p.as_str(), "age"),
                    _ => panic!("Expected Property spec"),
                }
                assert!(matches!(o.keys[0].order, Order::Desc));
            }
            _ => panic!("Expected OrderStep"),
        }
        match &decoded.steps[5] {
            LogicalStep::BothE(b) => {
                assert_eq!(b.labels.len(), 1);
                assert_eq!(b.labels[0].as_str(), "knows");
                assert_eq!(b.rank, Some(10));
            }
            _ => panic!("Expected BothEStep"),
        }
    }
}
