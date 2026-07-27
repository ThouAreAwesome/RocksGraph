import struct
from typing import List, Any, Optional

VERSION = 1

OP_BOTH = 1
OP_BOTHE = 2
OP_COUNT = 3
OP_DEGREE = 4
OP_HASLABEL = 5
OP_HASPROPERTY = 6
OP_IN = 7
OP_INE = 8
OP_OUT = 9
OP_OUTE = 10
OP_INV = 11
OP_OTHERV = 12
OP_OUTV = 13
OP_SCALARFILTER = 14
OP_VALUES = 15
OP_PROPERTIES = 16
OP_WHERE = 17
OP_UNION = 18
OP_ADDV = 19
OP_ADDE = 20
OP_FROM = 21
OP_TO = 22
OP_PROPERTY = 23
OP_V = 24
OP_E = 25
OP_LIMIT = 26
OP_HASID = 27
OP_COALESCE = 28
OP_ENDVERTEXFILTER = 29
OP_DROP = 30
OP_PATH = 31
OP_DEDUP = 32
OP_FOLD = 33
OP_REPEAT = 34
OP_NOT = 35
OP_AND = 36
OP_OR = 37
OP_SUM = 38
OP_MEAN = 39
OP_MAX = 40
OP_MIN = 41
OP_UNFOLD = 42
OP_AS = 43
OP_SELECT = 44
OP_RANGE = 45
OP_SKIP = 46
OP_TAIL = 47
OP_ORDER = 48
OP_SIMPLEPATH = 49
OP_CYCLICPATH = 50
OP_CHOOSE = 51
OP_GROUP = 52
OP_GROUPCOUNT = 53
OP_ID = 54
OP_LABEL = 55
OP_RANK = 56
OP_HASRANK = 57
OP_CONSTANT = 58
OP_IDENTITY = 59
OP_LOCAL = 60

# Primitive types matching rocksgraph/src/types/gvalue.rs
PRIM_NULL = 0
PRIM_BOOL = 1
PRIM_INT32 = 2
PRIM_INT64 = 3
PRIM_UINT16 = 4
PRIM_FLOAT32 = 5
PRIM_FLOAT64 = 6
PRIM_STRING = 7
PRIM_UUID = 8
PRIM_BYTES = 9

# Predicate tags matching rocksgraph/src/gremlin/value.rs
PRED_EQ = 0
PRED_NEQ = 1
PRED_GT = 2
PRED_GTE = 3
PRED_LT = 4
PRED_LTE = 5
PRED_BETWEEN = 6
PRED_WITHIN = 7
PRED_WITHOUT = 8

def encode(steps: List[Any]) -> bytes:
    """Encode a sequence of steps into the binary format."""
    buf = bytearray()
    buf.append(VERSION)
    _encode_plan(steps, buf)
    return bytes(buf)

def _encode_plan(steps: List[Any], buf: bytearray):
    buf.extend(struct.pack(">H", len(steps)))
    for opcode, args in steps:
        _encode_step(opcode, args, buf)

def _encode_step(opcode: int, args: Any, buf: bytearray):
    buf.append(opcode)
    
    if opcode in (OP_COUNT, OP_DROP, OP_PATH, OP_DEDUP, OP_FOLD, OP_SUM, OP_MEAN, 
                  OP_MAX, OP_MIN, OP_UNFOLD, OP_SIMPLEPATH, OP_CYCLICPATH, 
                  OP_ID, OP_LABEL, OP_RANK, OP_IDENTITY):
        return
        
    if opcode in (OP_BOTH, OP_BOTHE, OP_IN, OP_INE, OP_OUT, OP_OUTE):
        buf.extend(struct.pack(">H", len(args)))
        for label in args:
            _encode_string(label, buf)
        buf.append(0) # end_vertex_ids = None

    elif opcode in (OP_VALUES, OP_PROPERTIES, OP_AS, OP_SELECT, OP_E):
        buf.extend(struct.pack(">H", len(args)))
        for label in args:
            _encode_string(label, buf)
            
    elif opcode == OP_HASLABEL:
        _encode_predicate(args, buf)
            
    elif opcode == OP_SCALARFILTER:
        # Predicate
        _encode_predicate(args, buf)
        
    elif opcode == OP_HASPROPERTY:
        k, p = args
        _encode_string(k, buf)
        _encode_predicate(p, buf)
        
    elif opcode == OP_WHERE:
        # Single plan
        _encode_plan(args, buf)
        
    elif opcode in (OP_UNION, OP_COALESCE, OP_AND, OP_OR):
        # List of plans
        buf.extend(struct.pack(">H", len(args)))
        for plan in args:
            _encode_plan(plan, buf)
            
    elif opcode == OP_ADDV:
        # label: str, id: Option<i64>
        label, vid = args
        _encode_string(label, buf)
        if vid is not None:
            buf.append(1)
            buf.extend(struct.pack(">q", vid))
        else:
            buf.append(0)
            
    elif opcode == OP_ADDE:
        # label, from_id, to_id, properties, rank
        label, from_id, to_id, properties, rank = args
        _encode_string(label, buf)
        if from_id is not None:
            buf.append(1)
            buf.extend(struct.pack(">q", from_id))
        else:
            buf.append(0)
            
        if to_id is not None:
            buf.append(1)
            buf.extend(struct.pack(">q", to_id))
        else:
            buf.append(0)
            
        buf.extend(struct.pack(">H", len(properties)))
        for k, v in properties.items():
            _encode_string(k, buf)
            _encode_primitive(v, buf)
            
        if rank is not None:
            buf.append(1)
            buf.extend(struct.pack(">H", rank))
        else:
            buf.append(0)
            
    elif opcode in (OP_FROM, OP_TO):
        buf.extend(struct.pack(">q", args))
        
    elif opcode == OP_PROPERTY:
        k, v = args
        _encode_string(k, buf)
        _encode_primitive(v, buf)
        
    elif opcode == OP_V:
        buf.extend(struct.pack(">H", len(args)))
        for item in args:
            buf.extend(struct.pack(">q", item))
            
    elif opcode == OP_LIMIT:
        buf.extend(struct.pack(">q", args))
        
    elif opcode in (OP_HASID, OP_HASRANK):
        _encode_predicate(args, buf)
        
    elif opcode == OP_ENDVERTEXFILTER:
        ids, label_preds, property_preds = args
        if ids is not None:
            buf.append(1)
            buf.extend(struct.pack(">H", len(ids)))
            for item in ids:
                buf.extend(struct.pack(">q", item))
        else:
            buf.append(0)
            
        buf.extend(struct.pack(">H", len(label_preds)))
        for item in label_preds:
            _encode_predicate(item, buf)
            
        buf.extend(struct.pack(">H", len(property_preds)))
        for k, v in property_preds:
            _encode_string(k, buf)
            _encode_predicate(v, buf)
            
    elif opcode == OP_REPEAT:
        body, until, times, emit = args
        _encode_plan(body, buf)
        
        if until is not None:
            buf.append(1)
            _encode_plan(until, buf)
        else:
            buf.append(0)
            
        if times is not None:
            buf.append(1)
            buf.extend(struct.pack(">q", times))
        else:
            buf.append(0)
            
        _encode_emit_spec(emit, buf)
        
    elif opcode == OP_NOT:
        _encode_plan(args, buf)
        
    elif opcode == OP_RANGE:
        lo, hi = args
        buf.extend(struct.pack(">qq", lo, hi))
        
    elif opcode in (OP_SKIP, OP_TAIL):
        buf.extend(struct.pack(">q", args))
        
    elif opcode == OP_ORDER:
        buf.extend(struct.pack(">H", len(args)))
        for key_spec, order in args:
            if key_spec is None:
                buf.append(0)
            else:
                buf.append(1)
                _encode_string(key_spec, buf)
            buf.append(0 if order == "asc" else 1)
            
    elif opcode == OP_CHOOSE:
        predicate, true_choice, false_choice = args
        _encode_plan(predicate, buf)
        _encode_plan(true_choice, buf)
        if false_choice is not None:
            buf.append(1)
            _encode_plan(false_choice, buf)
        else:
            buf.append(0)
            
    elif opcode in (OP_GROUP, OP_GROUPCOUNT):
        if args is not None:
            buf.append(1)
            _encode_string(args, buf)
        else:
            buf.append(0)
            
    elif opcode == OP_CONSTANT:
        _encode_primitive(args, buf)
        
    elif opcode == OP_LOCAL:
        _encode_plan(args, buf)

def _encode_string(s: str, buf: bytearray):
    b = s.encode('utf-8')
    buf.extend(struct.pack(">H", len(b)))
    buf.extend(b)

def _encode_primitive(val: Any, buf: bytearray):
    from ._types import Int32, Int64, UInt16, Float32, Float64, Uuid
    import uuid
    if val is None:
        buf.append(PRIM_NULL)
    elif isinstance(val, bool):
        buf.append(PRIM_BOOL)
        buf.append(1 if val else 0)
    elif isinstance(val, Int32):
        buf.append(PRIM_INT32)
        buf.extend(struct.pack(">i", val.value))
    elif isinstance(val, Int64):
        buf.append(PRIM_INT64)
        buf.extend(struct.pack(">q", val.value))
    elif isinstance(val, UInt16):
        buf.append(PRIM_UINT16)
        buf.extend(struct.pack(">H", val.value))
    elif isinstance(val, Float32):
        buf.append(PRIM_FLOAT32)
        buf.extend(struct.pack(">f", val.value))
    elif isinstance(val, Float64):
        buf.append(PRIM_FLOAT64)
        buf.extend(struct.pack(">d", val.value))
    elif isinstance(val, int):
        buf.append(PRIM_INT64)
        buf.extend(struct.pack(">q", val))
    elif isinstance(val, float):
        buf.append(PRIM_FLOAT64)
        buf.extend(struct.pack(">d", val))
    elif isinstance(val, str):
        buf.append(PRIM_STRING)
        _encode_string(val, buf)
    elif isinstance(val, bytes) or isinstance(val, bytearray):
        buf.append(PRIM_BYTES)
        buf.extend(struct.pack(">I", len(val)))
        buf.extend(val)
    elif isinstance(val, Uuid):
        buf.append(PRIM_UUID)
        u = uuid.UUID(val.value)
        buf.extend(u.bytes)
    elif isinstance(val, uuid.UUID):
        buf.append(PRIM_UUID)
        buf.extend(val.bytes)
    else:
        raise ValueError(f"Unsupported primitive type: {type(val)}")

def _encode_predicate(pred: Any, buf: bytearray):
    tag, val = pred
    buf.append(tag)
    if tag == PRED_BETWEEN:
        _encode_primitive(val[0], buf)
        _encode_primitive(val[1], buf)
    elif tag in (PRED_WITHIN, PRED_WITHOUT):
        buf.extend(struct.pack(">H", len(val)))
        for item in val:
            _encode_primitive(item, buf)
    else:
        _encode_primitive(val, buf)

def _encode_emit_spec(emit: Any, buf: bytearray):
    if emit is None:
        buf.append(0) # Never
    elif emit == True:
        buf.append(1) # Always
    else:
        buf.append(2) # If(plan)
        _encode_plan(emit, buf)
