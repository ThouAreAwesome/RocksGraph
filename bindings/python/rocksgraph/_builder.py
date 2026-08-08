from __future__ import annotations

from enum import Enum
from typing import Any

from ._codec import (
    OP_ADDE,
    OP_ADDV,
    OP_AND,
    OP_AS,
    OP_BOTH,
    OP_BOTHE,
    OP_CHOOSE,
    OP_COALESCE,
    OP_CONSTANT,
    OP_COUNT,
    OP_CYCLICPATH,
    OP_DEDUP,
    OP_DEGREE,
    OP_DROP,
    OP_E,
    OP_FOLD,
    OP_FROM,
    OP_GROUP,
    OP_GROUPCOUNT,
    OP_HASID,
    OP_HASLABEL,
    OP_HASPROPERTY,
    OP_HASRANK,
    OP_ID,
    OP_IDENTITY,
    OP_IN,
    OP_INE,
    OP_INV,
    OP_LABEL,
    OP_LIMIT,
    OP_LOCAL,
    OP_MAX,
    OP_MEAN,
    OP_MIN,
    OP_NEAREST,
    OP_NEIGHBORS,
    OP_NOT,
    OP_OR,
    OP_ORDER,
    OP_OTHERV,
    OP_OUT,
    OP_OUTE,
    OP_OUTV,
    OP_PATH,
    OP_PROPERTIES,
    OP_PROPERTY,
    OP_RANGE,
    OP_RANK,
    OP_REPEAT,
    OP_SCALARFILTER,
    OP_SELECT,
    OP_SIMILARITY,
    OP_SIMPLEPATH,
    OP_SKIP,
    OP_SUM,
    OP_TAIL,
    OP_TO,
    OP_UNFOLD,
    OP_UNION,
    OP_V,
    OP_VALUES,
    OP_WHERE,
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


class T:
    """Token constants for use with by(), values(), properties()."""

    id = "id"
    label = "label"
    key = "key"
    value = "value"


class Direction(str, Enum):
    """Traversal direction for degree()."""

    OUT = "out"
    IN = "in"
    BOTH = "both"


# Direction aliases
Direction.Out = Direction.OUT
Direction.In = Direction.IN
Direction.Both = Direction.BOTH


class Order(str, Enum):
    """Sort order for order().by()."""

    Asc = "asc"
    Desc = "desc"


# Order aliases
Order.asc = Order.Asc
Order.desc = Order.Desc

asc = Order.Asc
desc = Order.Desc


class Vertex:
    """A materialised vertex from a traversal result."""

    def __init__(self, d: dict):
        self._d = d

    def __getitem__(self, key: str):
        return self._d[key]

    def __contains__(self, key: str):
        return key in self._d

    def get(self, key: str, default=None):
        return self._d.get(key, default)

    def keys(self):
        return self._d.keys()

    def __hash__(self):
        return hash(self._d["id"])

    def __eq__(self, other):
        if isinstance(other, Vertex):
            return self._d["id"] == other._d["id"]
        return NotImplemented

    def items(self):
        return self._d.items()

    def __iter__(self):
        return iter(self._d)

    def __repr__(self):
        props = self._d.get("properties", {})
        summary = ", ".join(f"{k}={v!r}" for k, v in props.items())
        return f"Vertex(id={self._d['id']!r}, label={self._d.get('label', '')!r}{', ' + summary if summary else ''})"

    @property
    def id(self):
        return self._d["id"]

    @property
    def label(self):
        return self._d.get("label", "")

    @property
    def properties(self):
        return self._d.get("properties", {})


class Edge:
    """A materialised edge from a traversal result."""

    def __init__(self, d: dict):
        self._d = d

    def __getitem__(self, key: str):
        return self._d[key]

    def __contains__(self, key: str):
        return key in self._d

    def get(self, key: str, default=None):
        return self._d.get(key, default)

    def keys(self):
        return self._d.keys()

    def __hash__(self):
        return hash((self._d["src"], self._d["dst"], self._d["label"], self._d.get("rank", 0)))

    def __eq__(self, other):
        if isinstance(other, Edge):
            return (
                self._d["src"] == other._d["src"]
                and self._d["dst"] == other._d["dst"]
                and self._d["label"] == other._d["label"]
                and self._d.get("rank", 0) == other._d.get("rank", 0)
            )
        return NotImplemented

    def items(self):
        return self._d.items()

    def __iter__(self):
        return iter(self._d)

    def __repr__(self):
        props = self._d.get("properties", {})
        summary = ", ".join(f"{k}={v!r}" for k, v in props.items())
        return f"Edge(src={self._d['src']!r}, dst={self._d['dst']!r}, label={self._d.get('label', '')!r}, rank={self._d.get('rank', 0)!r}{', ' + summary if summary else ''})"

    @property
    def src(self):
        return self._d["src"]

    @property
    def dst(self):
        return self._d["dst"]

    @property
    def label(self):
        return self._d.get("label", "")

    @property
    def rank(self):
        return self._d.get("rank", 0)

    @property
    def properties(self):
        return self._d.get("properties", {})


class Property:
    """A materialised property element from `.properties()` traversal result."""

    def __init__(self, d: dict):
        self._d = d

    def __getitem__(self, key: str):
        return self._d[key]

    def __contains__(self, key: str):
        return key in self._d

    def get(self, key: str, default=None):
        return self._d.get(key, default)

    def __repr__(self):
        return f"Property(key={self._d.get('key', '')!r}, value={self._d.get('value', '')!r})"

    @property
    def key(self):
        return self._d.get("key", "")

    @property
    def value(self):
        return self._d.get("value", None)


def _post_process(value):
    """Recursively convert raw dicts to Vertex/Edge/Property objects."""
    if isinstance(value, dict):
        if "src" in value and "dst" in value:
            return Edge(value)
        if "id" in value and "label" in value:
            return Vertex(value)
        if set(value.keys()) == {"key", "value"}:
            return Property(value)
        if "objects" in value:  # Path
            value["objects"] = [_post_process(o) for o in value["objects"]]
            return value
        # Generic map (e.g. group() output) — recursively post-process values
        return {k: _post_process(v) for k, v in value.items()}
    if isinstance(value, list):
        return [_post_process(v) for v in value]
    return value


def _vertex_id(v):
    """Extract a vertex ID from a Vertex, dict, or raw int."""
    if isinstance(v, Vertex):
        return v.id
    if isinstance(v, dict):
        return v.get("id", v)
    if hasattr(v, "id"):
        return v.id
    return v


class Traversal:
    def __init__(self, session, steps=None, prop_keys=None):
        self.session = session
        self.steps = steps or []
        self.prop_keys = prop_keys

    def _clone(self):
        return self.__class__(self.session, list(self.steps), self.prop_keys)

    def _add(self, opcode, args):
        t = self._clone()
        t.steps.append((opcode, args))
        return t

    def next(self):
        if self.session is None:
            raise RuntimeError("Anonymous traversal cannot be executed")
        results = self.session._execute(encode(self.steps), self.prop_keys)
        return _post_process(results[0]) if results else None

    def to_list(self):
        if self.session is None:
            raise RuntimeError("Anonymous traversal cannot be executed")
        return _post_process(self.session._execute(encode(self.steps), self.prop_keys))

    toList = to_list  # Gremlin camelCase alias

    def iterate(self):
        """Execute traversal for side-effects only (e.g. drop, mutations). Returns None."""
        self.to_list()

    def to_set(self):
        """Return results as a Python set (requires hashable elements)."""
        return set(self.to_list())

    toSet = to_set  # Gremlin camelCase alias

    def explain(self) -> str:
        """Render the physical execution plan for this traversal."""
        if self.session is None:
            raise RuntimeError("Anonymous traversal cannot be explained")
        return self.session._explain(encode(self.steps), self.prop_keys)

    def withProperties(self, *keys):
        """Include only the named properties when vertices/edges are materialized.
        An empty call withProperties() fetches all properties."""
        t = self._clone()
        t.prop_keys = list(keys) if keys else []
        return t

    def V(self, *ids):
        return self._add(OP_V, ids)

    def E(self, *keys):
        return self._add(OP_E, keys)

    def addV(self, label: str, vid=None):
        return self._add(OP_ADDV, (label, vid))

    def addE(self, label: str):
        self._addE_label = label
        return self._add(OP_ADDE, (label, None, None, {}, None))

    def from_(self, v):
        vid = _vertex_id(v)
        if self.steps and self.steps[-1][0] == OP_ADDE:
            label, _, to_id, props, rank = self.steps[-1][1]
            t = self._clone()
            t.steps[-1] = (OP_ADDE, (label, vid, to_id, props, rank))
            return t
        return self._add(OP_FROM, vid)

    def to(self, v):
        vid = _vertex_id(v)
        if self.steps and self.steps[-1][0] == OP_ADDE:
            label, from_id, _, props, rank = self.steps[-1][1]
            t = self._clone()
            t.steps[-1] = (OP_ADDE, (label, from_id, vid, props, rank))
            return t
        return self._add(OP_TO, vid)

    def property(self, key: str, value: Any):
        if self.steps and self.steps[-1][0] == OP_ADDE:
            label, from_id, to_id, props, rank = self.steps[-1][1]
            if key == "rank":
                # Handle rank special case for edge
                t = self._clone()
                t.steps[-1] = (OP_ADDE, (label, from_id, to_id, props, value))
                return t
            new_props = dict(props)
            new_props[key] = value
            t = self._clone()
            t.steps[-1] = (OP_ADDE, (label, from_id, to_id, new_props, rank))
            return t
        return self._add(OP_PROPERTY, (key, value))

    def out(self, *labels):
        return self._add(OP_OUT, labels)

    def in_(self, *labels):
        return self._add(OP_IN, labels)

    def both(self, *labels):
        return self._add(OP_BOTH, labels)

    def outE(self, *labels):
        return self._add(OP_OUTE, labels)

    def inE(self, *labels):
        return self._add(OP_INE, labels)

    def bothE(self, *labels):
        return self._add(OP_BOTHE, labels)

    def outV(self):
        return self._add(OP_OUTV, None)

    def inV(self):
        return self._add(OP_INV, None)

    def otherV(self):
        return self._add(OP_OTHERV, None)

    def hasLabel(self, *labels):
        if len(labels) == 1:
            return self._add(OP_HASLABEL, P.eq(labels[0]))
        return self._add(OP_HASLABEL, P.within(*labels))

    def has(self, *args):
        # has(key), has(key, value), has(label, key, value)
        if len(args) == 1:
            return self._add(OP_HASPROPERTY, (args[0], P.neq(None)))
        if len(args) == 2:
            key, value = args
            if isinstance(value, P):
                return self._add(OP_HASPROPERTY, (key, value))
            return self._add(OP_HASPROPERTY, (key, P.eq(value)))
        if len(args) == 3:
            label, key, value = args
            t = self.hasLabel(label)
            if isinstance(value, P):
                return t._add(OP_HASPROPERTY, (key, value))
            return t._add(OP_HASPROPERTY, (key, P.eq(value)))
        raise ValueError("has() takes 1 to 3 arguments")

    def is_(self, value):
        """Filter the current traverser value with a predicate. Used after values()."""
        if isinstance(value, P):
            return self._add(OP_SCALARFILTER, value)
        return self._add(OP_SCALARFILTER, P.eq(value))

    def hasId(self, value):
        if isinstance(value, P):
            return self._add(OP_HASID, value)
        if isinstance(value, (list, tuple)):
            return self._add(OP_HASID, P.within(*value))
        return self._add(OP_HASID, P.eq(value))

    def hasRank(self, value):
        if isinstance(value, P):
            return self._add(OP_HASRANK, value)
        return self._add(OP_HASRANK, P.eq(value))

    def values(self, *keys):
        return self._add(OP_VALUES, keys)

    def properties(self, *keys):
        return self._add(OP_PROPERTIES, keys)

    def nearest(self, property, query, k):
        """Find the k most similar elements to the query vector."""
        return self._add(OP_NEAREST, (property, query, k, None, None))

    def with_ef_search(self, ef):
        """Override the HNSW beam width for the immediately preceding nearest() or neighbors() step.

        Higher values improve recall at the cost of latency.
        """
        if not self.steps or self.steps[-1][0] not in (OP_NEAREST, OP_NEIGHBORS):
            raise ValueError("with_ef_search() must immediately follow nearest() or neighbors()")
        t = self._clone()
        op, args = t.steps[-1]
        if op == OP_NEAREST:
            prop, query, k, _, metric = args
            t.steps[-1] = (OP_NEAREST, (prop, query, k, ef, metric))
        else:  # OP_NEIGHBORS
            source_prop, target_prop, k, _, entity_type = args
            t.steps[-1] = (OP_NEIGHBORS, (source_prop, target_prop, k, ef, entity_type))
        return t

    def with_metric(self, metric):
        """Override the distance metric for the immediately preceding nearest() step.
        Not applicable after similarity() (metric is a required parameter there) or
        neighbors() (index metric is fixed at build time)."""
        if not self.steps or self.steps[-1][0] != OP_NEAREST:
            raise ValueError("with_metric() must immediately follow nearest()")
        t = self._clone()
        prop, query, k, ef, _ = t.steps[-1][1]
        t.steps[-1] = (OP_NEAREST, (prop, query, k, ef, metric))
        return t

    def similarity(self, property, query, metric):
        """Compute similarity between each element's vector and the query using the given metric."""
        return self._add(OP_SIMILARITY, (property, query, metric))

    def neighbors(self, source_prop, target_prop, k, entity_type):
        """Flat-map: reads source_prop from each traverser as query, searches the target_prop
        HNSW index of entity_type, emits up to k nearest results. Requires a declared index."""
        return self._add(OP_NEIGHBORS, (source_prop, target_prop, k, None, entity_type))

    def id(self):
        return self._add(OP_ID, None)

    def label(self):
        return self._add(OP_LABEL, None)

    def rank(self):
        return self._add(OP_RANK, None)

    def identity(self):
        return self._add(OP_IDENTITY, None)

    def constant(self, value):
        return self._add(OP_CONSTANT, value)

    def limit(self, limit: int):
        return self._add(OP_LIMIT, limit)

    def range(self, lo: int, hi: int):
        return self._add(OP_RANGE, (lo, hi))

    def skip(self, n: int):
        return self._add(OP_SKIP, n)

    def tail(self, n: int):
        return self._add(OP_TAIL, n)

    def count(self):
        return self._add(OP_COUNT, None)

    def dedup(self):
        return self._add(OP_DEDUP, None)

    def degree(self, direction: str | Direction | None = None):
        if direction is not None:
            if isinstance(direction, Direction):
                dir_val = direction.value
            elif isinstance(direction, str):
                dir_lower = direction.lower()
                if dir_lower in ("out", "in", "both"):
                    dir_val = dir_lower
                else:
                    raise ValueError(
                        f"Invalid Direction: '{direction}'. Expected Direction.OUT, Direction.IN, or Direction.BOTH."
                    )
            else:
                raise TypeError(f"Expected Direction, str, or None, got {type(direction).__name__}")
        else:
            dir_val = None
        return self._add(OP_DEGREE, dir_val)

    def fold(self):
        return self._add(OP_FOLD, None)

    def unfold(self):
        return self._add(OP_UNFOLD, None)

    def sum(self):
        return self._add(OP_SUM, None)

    def max(self):
        return self._add(OP_MAX, None)

    def min(self):
        return self._add(OP_MIN, None)

    def mean(self):
        return self._add(OP_MEAN, None)

    def group(self):
        """Group objects by traverser value. Use .by('key') to group by property."""
        return self._add(OP_GROUP, None)

    def groupCount(self):
        """Count objects per group. Use .by('key') to count by property.
        NOTE: May raise TypeError on Vertex/Edge dicts (unhashable)."""
        return self._add(OP_GROUPCOUNT, None)

    def order(self):
        return self._add(OP_ORDER, [])

    def by(self, key_spec: Any = None, order: str | Order = Order.Asc):
        # Standard Gremlin support: .by(Order.Desc), .by(desc), .by("desc"), or .by("prop", Order.Desc)
        if isinstance(key_spec, Order):
            order_val = key_spec.value
            key_spec = None
        elif isinstance(key_spec, str) and key_spec.lower() in ("asc", "desc") and order == Order.Asc:
            order_val = key_spec.lower()
            key_spec = None
        else:
            if isinstance(order, Order):
                order_val = order.value
            elif isinstance(order, str):
                order_lower = order.lower()
                if order_lower in ("asc", "desc"):
                    order_val = order_lower
                else:
                    raise ValueError(f"Invalid Order: '{order}'. Expected Order.Asc or Order.Desc.")
            else:
                raise TypeError(f"Expected Order or str, got {type(order).__name__}")

        if self.steps and self.steps[-1][0] == OP_ORDER:
            t = self._clone()
            t.steps[-1] = (OP_ORDER, [*list(t.steps[-1][1]), (key_spec, order_val)])
            return t
        if self.steps and self.steps[-1][0] in (OP_GROUP, OP_GROUPCOUNT):
            # group().by("key")
            t = self._clone()
            t.steps[-1] = (t.steps[-1][0], key_spec)
            return t
        raise ValueError("by() must follow order(), group(), or groupCount()")

    def path(self):
        return self._add(OP_PATH, None)

    def simplePath(self):
        return self._add(OP_SIMPLEPATH, None)

    def cyclicPath(self):
        return self._add(OP_CYCLICPATH, None)

    def as_(self, *labels):
        return self._add(OP_AS, labels)

    def select(self, *labels):
        return self._add(OP_SELECT, labels)

    def coalesce(self, *traversals):
        return self._add(OP_COALESCE, [t.steps for t in traversals])

    def union(self, *traversals):
        return self._add(OP_UNION, [t.steps for t in traversals])

    def and_(self, *traversals):
        return self._add(OP_AND, [t.steps for t in traversals])

    def or_(self, *traversals):
        return self._add(OP_OR, [t.steps for t in traversals])

    def not_(self, traversal):
        return self._add(OP_NOT, traversal.steps)

    def where(self, traversal):
        return self._add(OP_WHERE, traversal.steps)

    def local(self, traversal):
        return self._add(OP_LOCAL, traversal.steps)

    def choose(self, predicate_t, true_t, false_t=None):
        return self._add(OP_CHOOSE, (predicate_t.steps, true_t.steps, false_t.steps if false_t else None))

    def repeat(self, traversal):
        return self._add(OP_REPEAT, (traversal.steps, None, None, None))

    def until(self, traversal):
        if self.steps and self.steps[-1][0] == OP_REPEAT:
            body, _, times, emit = self.steps[-1][1]
            t = self._clone()
            t.steps[-1] = (OP_REPEAT, (body, traversal.steps, times, emit))
            return t
        raise ValueError("until() must follow repeat()")

    def times(self, n):
        if self.steps and self.steps[-1][0] == OP_REPEAT:
            body, until, _, emit = self.steps[-1][1]
            t = self._clone()
            t.steps[-1] = (OP_REPEAT, (body, until, n, emit))
            return t
        raise ValueError("times() must follow repeat()")

    def emit(self, traversal=None):
        if self.steps and self.steps[-1][0] == OP_REPEAT:
            body, until, times, _ = self.steps[-1][1]
            t = self._clone()
            emit_spec = True if traversal is None else traversal.steps
            t.steps[-1] = (OP_REPEAT, (body, until, times, emit_spec))
            return t
        raise ValueError("emit() must follow repeat()")

    def drop(self):
        return self._add(OP_DROP, None)


class GraphTraversal(Traversal):
    pass


class __:
    @staticmethod
    def V(*ids):
        return Traversal(None).V(*ids)

    @staticmethod
    def out(*labels):
        return Traversal(None).out(*labels)

    @staticmethod
    def in_(*labels):
        return Traversal(None).in_(*labels)

    @staticmethod
    def both(*labels):
        return Traversal(None).both(*labels)

    @staticmethod
    def outE(*labels):
        return Traversal(None).outE(*labels)

    @staticmethod
    def inE(*labels):
        return Traversal(None).inE(*labels)

    @staticmethod
    def bothE(*labels):
        return Traversal(None).bothE(*labels)

    @staticmethod
    def outV():
        return Traversal(None).outV()

    @staticmethod
    def inV():
        return Traversal(None).inV()

    @staticmethod
    def otherV():
        return Traversal(None).otherV()

    @staticmethod
    def has(*args):
        return Traversal(None).has(*args)

    @staticmethod
    def hasLabel(*labels):
        return Traversal(None).hasLabel(*labels)

    @staticmethod
    def values(*keys):
        return Traversal(None).values(*keys)

    @staticmethod
    def properties(*keys):
        return Traversal(None).properties(*keys)

    @staticmethod
    def id():
        return Traversal(None).id()

    @staticmethod
    def label():
        return Traversal(None).label()

    @staticmethod
    def identity():
        return Traversal(None).identity()

    @staticmethod
    def constant(value):
        return Traversal(None).constant(value)

    @staticmethod
    def limit(limit: int):
        return Traversal(None).limit(limit)

    @staticmethod
    def count():
        return Traversal(None).count()

    @staticmethod
    def dedup():
        return Traversal(None).dedup()

    @staticmethod
    def degree(direction: str | Direction | None = None):
        return Traversal(None).degree(direction)

    @staticmethod
    def fold():
        return Traversal(None).fold()

    @staticmethod
    def unfold():
        return Traversal(None).unfold()

    @staticmethod
    def sum():
        return Traversal(None).sum()

    @staticmethod
    def max():
        return Traversal(None).max()

    @staticmethod
    def min():
        return Traversal(None).min()

    @staticmethod
    def mean():
        return Traversal(None).mean()

    @staticmethod
    def group():
        return Traversal(None).group()

    @staticmethod
    def groupCount():
        return Traversal(None).groupCount()

    @staticmethod
    def path():
        return Traversal(None).path()

    @staticmethod
    def simplePath():
        return Traversal(None).simplePath()

    @staticmethod
    def cyclicPath():
        return Traversal(None).cyclicPath()

    @staticmethod
    def as_(*labels):
        return Traversal(None).as_(*labels)

    @staticmethod
    def select(*labels):
        return Traversal(None).select(*labels)

    @staticmethod
    def coalesce(*traversals):
        return Traversal(None).coalesce(*traversals)

    @staticmethod
    def union(*traversals):
        return Traversal(None).union(*traversals)

    @staticmethod
    def and_(*traversals):
        return Traversal(None).and_(*traversals)

    @staticmethod
    def or_(*traversals):
        return Traversal(None).or_(*traversals)

    @staticmethod
    def not_(traversal):
        return Traversal(None).not_(traversal)

    @staticmethod
    def where(traversal):
        return Traversal(None).where(traversal)

    @staticmethod
    def local(traversal):
        return Traversal(None).local(traversal)

    @staticmethod
    def choose(predicate_t, true_t, false_t=None):
        return Traversal(None).choose(predicate_t, true_t, false_t)

    @staticmethod
    def repeat(traversal):
        return Traversal(None).repeat(traversal)

    @staticmethod
    def addV(label: str, vid=None):
        return Traversal(None).addV(label, vid)

    @staticmethod
    def hasId(value):
        return Traversal(None).hasId(value)

    @staticmethod
    def drop():
        return Traversal(None).drop()

    @staticmethod
    def addE(label: str):
        return Traversal(None).addE(label)

    @staticmethod
    def from_(v):
        return Traversal(None).from_(v)

    @staticmethod
    def to(v):
        return Traversal(None).to(v)

    @staticmethod
    def property(key: str, value):
        return Traversal(None).property(key, value)

    @staticmethod
    def nearest(property, query, k):
        return Traversal(None).nearest(property, query, k)

    @staticmethod
    def similarity(property, query, metric):
        return Traversal(None).similarity(property, query, metric)

    @staticmethod
    def neighbors(source_prop, target_prop, k, entity_type):
        return Traversal(None).neighbors(source_prop, target_prop, k, entity_type)


class P:
    def __init__(self, tag, value):
        self.tag = tag
        self.value = value

    def __iter__(self):
        yield self.tag
        yield self.value

    @staticmethod
    def eq(value):
        return P(PRED_EQ, value)

    @staticmethod
    def neq(value):
        return P(PRED_NEQ, value)

    @staticmethod
    def lt(value):
        return P(PRED_LT, value)

    @staticmethod
    def lte(value):
        return P(PRED_LTE, value)

    @staticmethod
    def gt(value):
        return P(PRED_GT, value)

    @staticmethod
    def gte(value):
        return P(PRED_GTE, value)

    @staticmethod
    def between(v1, v2):
        return P(PRED_BETWEEN, (v1, v2))

    @staticmethod
    def within(*values):
        return P(PRED_WITHIN, values)

    @staticmethod
    def without(*values):
        return P(PRED_WITHOUT, values)


class RocksOptions:
    """RocksDB storage tuning options. Mirrors rocksgraph::RocksOptions."""

    def __init__(
        self,
        *,
        block_cache_size: int = 1024 * 1024 * 1024,
        write_buffer_size: int = 64 * 1024 * 1024,
        max_write_buffer_number: int = 3,
        max_background_jobs: int = 2,
        vertex_block_size: int = 4096,
        edge_block_size: int = 4096,
        cache_index_and_filter_blocks: bool = True,
    ):
        self.block_cache_size = block_cache_size
        self.write_buffer_size = write_buffer_size
        self.max_write_buffer_number = max_write_buffer_number
        self.max_background_jobs = max_background_jobs
        self.vertex_block_size = vertex_block_size
        self.edge_block_size = edge_block_size
        self.cache_index_and_filter_blocks = cache_index_and_filter_blocks


class IndexOptions:
    """Vector index runtime options. Mirrors rocksgraph::IndexOptions."""

    def __init__(self, *, default_memory_limit: int = 0, per_index_overrides: list | None = None):
        self.default_memory_limit = default_memory_limit
        self.per_index_overrides = per_index_overrides or []


class GraphOptions:
    """Database open options. Mirrors rocksgraph::GraphOptions."""

    def __init__(
        self, *, mode: str = "auto", edge_mode: str = "single", storage: RocksOptions = None, index: IndexOptions = None
    ):
        self.mode = mode
        self.edge_mode = edge_mode
        self.storage = storage or RocksOptions()
        self.index = index or IndexOptions()


class IndexManager:
    """Handle for vector index maintenance operations (rebuild, save, future export/import)."""

    def __init__(self, manager):
        self._manager = manager

    def rebuild(self, entity_type, property: str):
        """Rebuild the in-memory vector index for (entity_type, property) from stored data."""
        et = entity_type.value if isinstance(entity_type, Enum) else str(entity_type)
        self._manager.rebuild(et, property)

    def save(self, entity_type, property: str):
        """Persist an on-disk snapshot for a specific named vector index."""
        et = entity_type.value if isinstance(entity_type, Enum) else str(entity_type)
        self._manager.save(et, property)

    def save_all(self):
        """Persist on-disk snapshots for all declared vector indexes."""
        self._manager.save_all()


class Graph:
    def __init__(self, path: str, *, options: GraphOptions = None):
        from rocksgraph._rocksgraph import PyGraph

        opts = options or GraphOptions()
        index_dict = None
        if opts.index.default_memory_limit or opts.index.per_index_overrides:
            index_dict = {}
            if opts.index.default_memory_limit:
                index_dict["default_memory_limit"] = opts.index.default_memory_limit
            if opts.index.per_index_overrides:
                index_dict["per_index_overrides"] = opts.index.per_index_overrides
        self._graph = PyGraph.open_with_options(
            path,
            mode=opts.mode.value if isinstance(opts.mode, Enum) else str(opts.mode),
            edge_mode=opts.edge_mode.value if isinstance(opts.edge_mode, Enum) else str(opts.edge_mode),
            storage={
                "block_cache_size": opts.storage.block_cache_size,
                "write_buffer_size": opts.storage.write_buffer_size,
                "max_write_buffer_number": opts.storage.max_write_buffer_number,
                "max_background_jobs": opts.storage.max_background_jobs,
                "vertex_block_size": opts.storage.vertex_block_size,
                "edge_block_size": opts.storage.edge_block_size,
                "cache_index_and_filter_blocks": opts.storage.cache_index_and_filter_blocks,
            },
            index=index_dict,
        )

    @staticmethod
    def open_with_options(path: str, *, options: GraphOptions = None):
        """Open a database with custom options.

        Args:
            path: Path to the database directory.
            options: GraphOptions instance (mode, edge_mode, storage, index).
        """
        return Graph(path, options=options)

    def read(self):
        return ReadSession(self._graph.read())

    def begin(self):
        return TxnSession(self._graph.begin())

    def open_schema(self):
        """Open a SchemaSession for declaring labels, property types, and vector indexes."""
        return SchemaSession(self._graph.open_schema())

    def open_bulk_loader(self):
        """Open a BulkLoader session for high-throughput batch SST ingestion."""
        return BulkLoader(self._graph.open_bulk_loader())

    def index_manager(self):
        """Return an IndexManager handle for index maintenance (rebuild, save, future export/import)."""
        return IndexManager(self._graph.index_manager())

    def close(self):
        self._graph.close()


class SchemaSession:
    def __init__(self, session):
        self._session = session

    def add_vertex_label(self, name: str):
        self._session.add_vertex_label(str(name))
        return self

    def add_edge_label(self, name: str):
        self._session.add_edge_label(str(name))
        return self

    def add_property_key(self, name: str, data_type):
        dt = int(data_type)
        self._session.add_property_key(str(name), dt)
        return self

    def set_edge_mode(self, mode):
        m = mode.value if isinstance(mode, Enum) else str(mode)
        self._session.set_edge_mode(m)
        return self

    def set_schema_mode(self, mode):
        m = mode.value if isinstance(mode, Enum) else str(mode)
        self._session.set_schema_mode(m)
        return self

    def add_vector_index(
        self,
        config=None,
        *,
        entity_type="vertex",
        property: str | None = None,
        dimension: int | None = None,
        metric="cosine",
        algorithm="hnsw",
        m: int = 16,
        ef_construction: int = 200,
        ef_search: int = 50,
        quantization="f16",
    ):
        if config is not None:
            et = config.entity_type
            prop = config.property
            dim = config.dimension
            met = config.metric
            alg = config.algorithm
            m_val = config.m
            ef_c = config.ef_construction
            ef_s = config.ef_search
            quant = config.quantization
        else:
            if property is None or dimension is None:
                raise ValueError("property and dimension are required when config is not provided")
            et = entity_type.value if isinstance(entity_type, Enum) else str(entity_type)
            prop = str(property)
            dim = int(dimension)
            met = metric.value if isinstance(metric, Enum) else str(metric)
            alg = algorithm.value if isinstance(algorithm, Enum) else str(algorithm)
            m_val = int(m)
            ef_c = int(ef_construction)
            ef_s = int(ef_search)
            quant = quantization.value if isinstance(quantization, Enum) else str(quantization)

        self._session.add_vector_index(
            et,
            prop,
            dim,
            metric=met,
            algorithm=alg,
            m=m_val,
            ef_construction=ef_c,
            ef_search=ef_s,
            quantization=quant,
        )
        return self

    def drop_vector_index(self, entity_type, property: str):
        et = entity_type.value if isinstance(entity_type, Enum) else str(entity_type)
        self._session.drop_vector_index(et, str(property))
        return self

    def commit(self):
        self._session.commit()

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        if exc_type is None:
            self._session.commit()
        return False


class BulkLoader:
    def __init__(self, loader):
        self._loader = loader

    def with_work_dir(self, path: str):
        self._loader.with_work_dir(str(path))
        return self

    def with_max_sst_size(self, bytes: int):
        self._loader.with_max_sst_size(int(bytes))
        return self

    def with_max_memory(self, bytes: int):
        self._loader.with_max_memory(int(bytes))
        return self

    def load_vertices(self, vertices):
        self._loader.load_vertices(vertices)
        return self

    def load_edges(self, edges):
        self._loader.load_edges(edges)
        return self

    def commit(self):
        from rocksgraph._types import BulkLoadStats

        stats = self._loader.commit()
        return BulkLoadStats(
            vertices_written=stats["vertices_written"],
            edges_written=stats["edges_written"],
            sst_files=stats["sst_files"],
            duration_secs=stats["duration_secs"],
        )

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        if exc_type is None:
            self.commit()
        return False


class ReadSession:
    def __init__(self, session):
        self._session = session

    def g(self):
        if self._session is None:
            raise RuntimeError("ReadSession is already closed")
        return GraphTraversal(self._session)

    def close(self):
        """Release the snapshot, allowing the database to fully close."""
        self._session = None

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.close()
        return False


class TxnSession:
    def __init__(self, session):
        self._session = session

    def g(self):
        if self._session is None:
            raise RuntimeError("TxnSession is already closed")
        return GraphTraversal(self._session)

    def commit(self):
        if self._session is None:
            raise RuntimeError("TxnSession is already closed")
        self._session.commit()
        self._session = None

    def rollback(self):
        if self._session is None:
            raise RuntimeError("TxnSession is already closed")
        self._session.rollback()
        self._session = None

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        if self._session is None:
            return False  # already committed or rolled back explicitly
        if exc_type is None:
            self._session.commit()
        else:
            self._session.rollback()
        self._session = None
        return False
