"""Pure-Python codec unit tests — no native module needed."""

import struct

from rocksgraph import Int64, P, Traversal, __
from rocksgraph._codec import (
    OP_DEGREE,
    OP_HASLABEL,
    OP_HASPROPERTY,
    OP_LIMIT,
    OP_OUT,
    OP_OUTE,
    OP_RANGE,
    OP_REPEAT,
    OP_SKIP,
    OP_TAIL,
    PRED_BETWEEN,
    PRED_EQ,
    PRED_GT,
    PRED_GTE,
    PRED_LT,
    PRED_LTE,
    PRED_NEQ,
    PRED_WITHIN,
    PRED_WITHOUT,
    encode,
)


class TestPredicateTags:
    def test_eq(self):
        assert P.eq("x").tag == PRED_EQ == 0x00

    def test_neq(self):
        assert P.neq("x").tag == PRED_NEQ == 0x01

    def test_gt(self):
        assert P.gt(1).tag == PRED_GT == 0x02

    def test_gte(self):
        assert P.gte(1).tag == PRED_GTE == 0x03

    def test_lt(self):
        assert P.lt(1).tag == PRED_LT == 0x04

    def test_lte(self):
        assert P.lte(1).tag == PRED_LTE == 0x05

    def test_between(self):
        p = P.between(1, 5)
        assert p.tag == PRED_BETWEEN == 0x06
        assert p.value == (1, 5)

    def test_within(self):
        p = P.within("a", "b")
        assert p.tag == PRED_WITHIN == 0x07
        assert p.value == ("a", "b")

    def test_without(self):
        p = P.without("a")
        assert p.tag == PRED_WITHOUT == 0x08

    def test_eq_encodes_tag_byte(self):
        """P.eq('x') → tag byte 0x00 in output."""
        t = Traversal(None).V(1).has("age", P.eq(Int64(30)))
        buf = encode(t.steps)
        # Find the predicate tag byte: after V(1) encoding and OP_HASPROPERTY+key
        assert buf[3] == 0x18  # OP_V (after 3-byte header)
        assert buf[14] == OP_HASPROPERTY
        # Key: 0x00 0x03 "age", then predicate tag 0x00
        idx = buf.find(b"age")
        # buf.find returns index of 'a'; "age" is 3 bytes, predicate tag follows at idx+3.
        pred_tag = buf[idx + 3]
        assert pred_tag == PRED_EQ


class TestIntegerFormats:
    def test_limit_is_signed_i64(self):
        t = Traversal(None).V().limit(5)
        buf = encode(t.steps)
        # After V(): 0x0018 (OP_V) 0x0000 (no ids) — that's 3 bytes.
        # Then OP_LIMIT (0x1a) at offset 3.
        assert buf[6] == OP_LIMIT
        val = struct.unpack(">q", buf[7:15])[0]
        assert val == 5

    def test_range_is_two_signed_i64(self):
        t = Traversal(None).V().range(5, 10)
        buf = encode(t.steps)
        assert buf[6] == OP_RANGE
        lo, hi = struct.unpack(">qq", buf[7:23])
        assert (lo, hi) == (5, 10)

    def test_skip_is_signed_i64(self):
        t = Traversal(None).V().skip(3)
        buf = encode(t.steps)
        assert buf[6] == OP_SKIP
        val = struct.unpack(">q", buf[7:15])[0]
        assert val == 3

    def test_tail_is_signed_i64(self):
        t = Traversal(None).V().tail(3)
        buf = encode(t.steps)
        assert buf[6] == OP_TAIL
        val = struct.unpack(">q", buf[7:15])[0]
        assert val == 3

    def test_repeat_times_is_signed_i64(self):
        t = Traversal(None).V().repeat(__().out()).times(7)
        buf = encode(t.steps)
        # V(), then OP_REPEAT
        assert buf[6] == OP_REPEAT
        # Sub-plan (OP_OUT) then until=None(0x00), then times=1(0x01), then times value
        # Byte stream: ... [body_plan] [until_flag=0x00] [times_flag=0x01] [times:i64]
        # Find the 0x01 flag for times
        times_flag_idx = buf.rfind(b"\x01", 4)
        assert times_flag_idx > 0
        val = struct.unpack(">q", buf[times_flag_idx + 1 : times_flag_idx + 9])[0]
        assert val == 7


class TestHasEncoding:
    def test_hasproperty_encodes_key_then_predicate(self):
        t = Traversal(None).V().has("age", P.gt(Int64(28)))
        buf = encode(t.steps)
        assert buf[6] == OP_HASPROPERTY
        # key "age" is at buf[4:9] (u16 length 0x0003 + "age")
        # After key, predicate tag byte follows
        key_len = struct.unpack(">H", buf[7:9])[0]
        assert key_len == 3
        assert buf[9:12] == b"age"
        pred_tag = buf[12]
        assert pred_tag == PRED_GT

    def test_haslabel_encodes_predicate(self):
        t = Traversal(None).V().hasLabel("person")
        buf = encode(t.steps)
        assert buf[6] == OP_HASLABEL
        # Should be predicate, not a label count+string
        pred_tag = buf[7]
        assert pred_tag in (PRED_EQ, PRED_WITHIN)
        # If single label: EQ. If multi: WITHIN.
        assert pred_tag == PRED_EQ  # single label → eq


class TestEdgeTraversalEncoding:
    def test_oute_encodes_labels_endvertexids_rank(self):
        t = Traversal(None).V(1).outE("knows")
        buf = encode(t.steps)
        # Find OP_OUTE in the byte stream
        idx = list(buf).index(OP_OUTE)
        assert idx > 0, f"OP_OUTE not found in bytes: {buf.hex()}"
        # After opcode: u16 labels count (2 bytes) + u16 len (2 bytes) + "knows" (5 bytes) = 9 bytes
        # Then end_vertex_ids flag (0x00) + rank flag (0x00) = 2 more bytes
        after_labels = idx + 1 + 2 + 2 + 5  # opcode + u16 count + u16 len + "knows"
        assert buf[after_labels] == 0x00, (
            f"Expected end_vertex_ids=0x00 at offset {after_labels}, got {buf[after_labels]:#x}"
        )
        assert buf[after_labels + 1] == 0x00, (
            f"Expected rank=0x00 at offset {after_labels + 1}, got {buf[after_labels + 1]:#x}"
        )

    def test_oute_and_out_have_different_byte_lengths(self):
        """OP_OUTE should be 1 byte longer than OP_OUT due to rank byte."""
        t_oute = Traversal(None).V(1).outE("knows")
        t_out = Traversal(None).V(1).out("knows")
        buf_oute = encode(t_oute.steps)
        buf_out = encode(t_out.steps)
        assert len(buf_oute) == len(buf_out) + 1

    def test_out_does_not_have_rank_byte(self):
        """OP_OUT should not include rank."""
        t = Traversal(None).V(1).out("knows")
        buf = encode(t.steps)
        idx = list(buf).index(OP_OUT)
        after_labels = idx + 1 + 2 + 2 + 5  # opcode + u16 count + u16 len + "knows"
        assert buf[after_labels] == 0x00, "end_vertex_ids flag should be 0x00"


class TestDegreeEncoding:
    def test_degree_defaults_to_direction_both(self):
        t = Traversal(None).V(1).degree()
        buf = encode(t.steps)
        idx = list(buf).index(OP_DEGREE)
        assert buf[idx + 1] == 0  # Both = 0

    def test_degree_with_direction(self):
        t = Traversal(None).V(1).degree("out")
        buf = encode(t.steps)
        idx = list(buf).index(OP_DEGREE)
        assert buf[idx + 1] == 1  # Out = 1

    def test_degree_in_direction(self):
        t = Traversal(None).V(1).degree("in")
        buf = encode(t.steps)
        idx = list(buf).index(OP_DEGREE)
        assert buf[idx + 1] == 2  # In = 2


class TestCloningDoesNotMutate:
    def test_order_by_does_not_mutate_parent(self):
        t1 = Traversal(None).V().order()
        t2 = t1.by("age", "asc")
        # t1 should not have a by() spec
        assert t1.steps[-1][1] == []
        assert t2.steps[-1][1] == [("age", "asc")]


class TestVertexId:
    def test_vertex_id_from_dict(self):
        from rocksgraph._builder import _vertex_id

        assert _vertex_id({"id": 7}) == 7

    def test_vertex_id_from_int(self):
        from rocksgraph._builder import _vertex_id

        assert _vertex_id(42) == 42
