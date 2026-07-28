from typing import Any, List, Optional
from ._codec import *
from enum import Enum


class T:
    """Token constants for use with by(), values(), properties()."""
    id = "id"
    label = "label"
    key = "key"
    value = "value"


class Direction(Enum):
    """Traversal direction for degree()."""
    OUT = "out"
    IN = "in"
    BOTH = None  # default


class Order(Enum):
    """Sort order for order().by()."""
    asc = "asc"
    desc = "desc"


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
        return f"Vertex(id={self._d['id']!r}, label={self._d.get('label','')!r}{', ' + summary if summary else ''})"

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
            return (self._d["src"] == other._d["src"]
                    and self._d["dst"] == other._d["dst"]
                    and self._d["label"] == other._d["label"]
                    and self._d.get("rank", 0) == other._d.get("rank", 0))
        return NotImplemented

    def items(self):
        return self._d.items()

    def __iter__(self):
        return iter(self._d)

    def __repr__(self):
        props = self._d.get("properties", {})
        summary = ", ".join(f"{k}={v!r}" for k, v in props.items())
        return f"Edge(src={self._d['src']!r}, dst={self._d['dst']!r}, label={self._d.get('label','')!r}, rank={self._d.get('rank', 0)!r}{', ' + summary if summary else ''})"

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
        return f"Property(key={self._d.get('key','')!r}, value={self._d.get('value','')!r})"

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
    if hasattr(v, 'id'):
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
        return None

    def to_set(self):
        """Return results as a Python set (requires hashable elements)."""
        return set(self.to_list())

    toSet = to_set  # Gremlin camelCase alias

    def withProperties(self, *keys):
        """Include only the named properties when vertices/edges are materialized.
        An empty call withProperties() fetches all properties."""
        t = self._clone()
        t.prop_keys = list(keys) if keys else []
        return t

    def V(self, *ids): return self._add(OP_V, ids)
    def E(self, *keys): return self._add(OP_E, keys)
    def addV(self, label: str, vid=None): return self._add(OP_ADDV, (label, vid))

    def addE(self, label: str):
        self._addE_label = label
        return self._add(OP_ADDE, (label, None, None, {}, None))
    
    def from_(self, v):
        vid = _vertex_id(v)
        if self.steps and self.steps[-1][0] == OP_ADDE:
            label, from_id, to_id, props, rank = self.steps[-1][1]
            t = self._clone()
            t.steps[-1] = (OP_ADDE, (label, vid, to_id, props, rank))
            return t
        return self._add(OP_FROM, vid)

    def to(self, v):
        vid = _vertex_id(v)
        if self.steps and self.steps[-1][0] == OP_ADDE:
            label, from_id, to_id, props, rank = self.steps[-1][1]
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
            else:
                new_props = dict(props)
                new_props[key] = value
                t = self._clone()
                t.steps[-1] = (OP_ADDE, (label, from_id, to_id, new_props, rank))
                return t
        return self._add(OP_PROPERTY, (key, value))
        
    def out(self, *labels): return self._add(OP_OUT, labels)
    def in_(self, *labels): return self._add(OP_IN, labels)
    def both(self, *labels): return self._add(OP_BOTH, labels)
    def outE(self, *labels): return self._add(OP_OUTE, labels)
    def inE(self, *labels): return self._add(OP_INE, labels)
    def bothE(self, *labels): return self._add(OP_BOTHE, labels)
    def outV(self): return self._add(OP_OUTV, None)
    def inV(self): return self._add(OP_INV, None)
    def otherV(self): return self._add(OP_OTHERV, None)
    def hasLabel(self, *labels):
        if len(labels) == 1:
            return self._add(OP_HASLABEL, P.eq(labels[0]))
        else:
            return self._add(OP_HASLABEL, P.within(*labels))
    
    def has(self, *args):
        # has(key), has(key, value), has(label, key, value)
        if len(args) == 1:
            return self._add(OP_HASPROPERTY, (args[0], P.neq(None)))
        elif len(args) == 2:
            key, value = args
            if isinstance(value, P):
                return self._add(OP_HASPROPERTY, (key, value))
            else:
                return self._add(OP_HASPROPERTY, (key, P.eq(value)))
        elif len(args) == 3:
            label, key, value = args
            t = self.hasLabel(label)
            if isinstance(value, P):
                return t._add(OP_HASPROPERTY, (key, value))
            else:
                return t._add(OP_HASPROPERTY, (key, P.eq(value)))
        else:
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
        
    def values(self, *keys): return self._add(OP_VALUES, keys)
    def properties(self, *keys): return self._add(OP_PROPERTIES, keys)
    def id(self): return self._add(OP_ID, None)
    def label(self): return self._add(OP_LABEL, None)
    def rank(self): return self._add(OP_RANK, None)
    def identity(self): return self._add(OP_IDENTITY, None)
    def constant(self, value): return self._add(OP_CONSTANT, value)
    
    def limit(self, limit: int): return self._add(OP_LIMIT, limit)
    def range(self, lo: int, hi: int): return self._add(OP_RANGE, (lo, hi))
    def skip(self, n: int): return self._add(OP_SKIP, n)
    def tail(self, n: int): return self._add(OP_TAIL, n)
    
    def count(self): return self._add(OP_COUNT, None)
    def dedup(self): return self._add(OP_DEDUP, None)
    def degree(self, direction=None):
        if isinstance(direction, Direction):
            direction = direction.value
        return self._add(OP_DEGREE, direction)
    def fold(self): return self._add(OP_FOLD, None)
    def unfold(self): return self._add(OP_UNFOLD, None)
    def sum(self): return self._add(OP_SUM, None)
    def max(self): return self._add(OP_MAX, None)
    def min(self): return self._add(OP_MIN, None)
    def mean(self): return self._add(OP_MEAN, None)
    
    def group(self): 
        """Group objects by traverser value. Use .by('key') to group by property."""
        return self._add(OP_GROUP, None)
    def groupCount(self):
        """Count objects per group. Use .by('key') to count by property.
        NOTE: May raise TypeError on Vertex/Edge dicts (unhashable)."""
        return self._add(OP_GROUPCOUNT, None)
        
    def order(self): return self._add(OP_ORDER, [])
    def by(self, key_spec, order="asc"):
        if isinstance(order, Order):
            order = order.value
        if self.steps and self.steps[-1][0] == OP_ORDER:
            t = self._clone()
            t.steps[-1] = (OP_ORDER, list(t.steps[-1][1]) + [(key_spec, order)])
            return t
        elif self.steps and self.steps[-1][0] in (OP_GROUP, OP_GROUPCOUNT):
            # group().by("key")
            t = self._clone()
            t.steps[-1] = (t.steps[-1][0], key_spec)
            return t
        else:
            raise ValueError("by() must follow order(), group(), or groupCount()")
            
    def path(self): return self._add(OP_PATH, None)
    def simplePath(self): return self._add(OP_SIMPLEPATH, None)
    def cyclicPath(self): return self._add(OP_CYCLICPATH, None)
    
    def as_(self, *labels): return self._add(OP_AS, labels)
    def select(self, *labels): return self._add(OP_SELECT, labels)
    
    def coalesce(self, *traversals): return self._add(OP_COALESCE, [t.steps for t in traversals])
    def union(self, *traversals): return self._add(OP_UNION, [t.steps for t in traversals])
    def and_(self, *traversals): return self._add(OP_AND, [t.steps for t in traversals])
    def or_(self, *traversals): return self._add(OP_OR, [t.steps for t in traversals])
    def not_(self, traversal): return self._add(OP_NOT, traversal.steps)
    def where(self, traversal): return self._add(OP_WHERE, traversal.steps)
    def local(self, traversal): return self._add(OP_LOCAL, traversal.steps)
    def choose(self, predicate_t, true_t, false_t=None):
        return self._add(OP_CHOOSE, (predicate_t.steps, true_t.steps, false_t.steps if false_t else None))
        
    def repeat(self, traversal): return self._add(OP_REPEAT, (traversal.steps, None, None, None))
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

    def drop(self): return self._add(OP_DROP, None)

class GraphTraversal(Traversal):
    pass

class __:
    @staticmethod
    def V(*ids): return Traversal(None).V(*ids)
    @staticmethod
    def out(*labels): return Traversal(None).out(*labels)
    @staticmethod
    def in_(*labels): return Traversal(None).in_(*labels)
    @staticmethod
    def both(*labels): return Traversal(None).both(*labels)
    @staticmethod
    def outE(*labels): return Traversal(None).outE(*labels)
    @staticmethod
    def inE(*labels): return Traversal(None).inE(*labels)
    @staticmethod
    def bothE(*labels): return Traversal(None).bothE(*labels)
    @staticmethod
    def outV(): return Traversal(None).outV()
    @staticmethod
    def inV(): return Traversal(None).inV()
    @staticmethod
    def otherV(): return Traversal(None).otherV()
    @staticmethod
    def has(*args): return Traversal(None).has(*args)
    @staticmethod
    def hasLabel(*labels): return Traversal(None).hasLabel(*labels)
    @staticmethod
    def values(*keys): return Traversal(None).values(*keys)
    @staticmethod
    def properties(*keys): return Traversal(None).properties(*keys)
    @staticmethod
    def id(): return Traversal(None).id()
    @staticmethod
    def label(): return Traversal(None).label()
    @staticmethod
    def identity(): return Traversal(None).identity()
    @staticmethod
    def constant(value): return Traversal(None).constant(value)
    @staticmethod
    def limit(limit: int): return Traversal(None).limit(limit)
    @staticmethod
    def count(): return Traversal(None).count()
    @staticmethod
    def dedup(): return Traversal(None).dedup()
    @staticmethod
    def degree(): return Traversal(None).degree()
    @staticmethod
    def fold(): return Traversal(None).fold()
    @staticmethod
    def unfold(): return Traversal(None).unfold()
    @staticmethod
    def sum(): return Traversal(None).sum()
    @staticmethod
    def max(): return Traversal(None).max()
    @staticmethod
    def min(): return Traversal(None).min()
    @staticmethod
    def mean(): return Traversal(None).mean()
    @staticmethod
    def group(): return Traversal(None).group()
    @staticmethod
    def groupCount(): return Traversal(None).groupCount()
    @staticmethod
    def path(): return Traversal(None).path()
    @staticmethod
    def simplePath(): return Traversal(None).simplePath()
    @staticmethod
    def cyclicPath(): return Traversal(None).cyclicPath()
    @staticmethod
    def as_(*labels): return Traversal(None).as_(*labels)
    @staticmethod
    def select(*labels): return Traversal(None).select(*labels)
    @staticmethod
    def coalesce(*traversals): return Traversal(None).coalesce(*traversals)
    @staticmethod
    def union(*traversals): return Traversal(None).union(*traversals)
    @staticmethod
    def and_(*traversals): return Traversal(None).and_(*traversals)
    @staticmethod
    def or_(*traversals): return Traversal(None).or_(*traversals)
    @staticmethod
    def not_(traversal): return Traversal(None).not_(traversal)
    @staticmethod
    def where(traversal): return Traversal(None).where(traversal)
    @staticmethod
    def local(traversal): return Traversal(None).local(traversal)
    @staticmethod
    def choose(predicate_t, true_t, false_t=None): return Traversal(None).choose(predicate_t, true_t, false_t)
    @staticmethod
    def repeat(traversal): return Traversal(None).repeat(traversal)
    @staticmethod
    def addV(label: str, vid=None): return Traversal(None).addV(label, vid)
    @staticmethod
    def hasId(value): return Traversal(None).hasId(value)
    @staticmethod
    def drop(): return Traversal(None).drop()
    @staticmethod
    def addE(label: str): return Traversal(None).addE(label)
    @staticmethod
    def from_(v): return Traversal(None).from_(v)
    @staticmethod
    def to(v): return Traversal(None).to(v)
    @staticmethod
    def property(key: str, value): return Traversal(None).property(key, value)

class P:
    def __init__(self, tag, value):
        self.tag = tag
        self.value = value
        
    def __iter__(self):
        yield self.tag
        yield self.value
        
    @staticmethod
    def eq(value): return P(PRED_EQ, value)
    @staticmethod
    def neq(value): return P(PRED_NEQ, value)
    @staticmethod
    def lt(value): return P(PRED_LT, value)
    @staticmethod
    def lte(value): return P(PRED_LTE, value)
    @staticmethod
    def gt(value): return P(PRED_GT, value)
    @staticmethod
    def gte(value): return P(PRED_GTE, value)
    @staticmethod
    def between(v1, v2): return P(PRED_BETWEEN, (v1, v2))
    @staticmethod
    def within(*values): return P(PRED_WITHIN, values)
    @staticmethod
    def without(*values): return P(PRED_WITHOUT, values)

class Graph:
    def __init__(self, path: str):
        try:
            from rocksgraph._rocksgraph import PyGraph
        except ModuleNotFoundError:
            from _rocksgraph import PyGraph
        self._graph = PyGraph.open(path)

    def read(self):
        return ReadSession(self._graph.read())

    def tx(self):
        return TxSession(self._graph.tx())

class ReadSession:
    def __init__(self, session):
        self._session = session

    def traversal(self):
        return GraphTraversal(self._session)

class TxSession:
    def __init__(self, session):
        self._session = session

    def traversal(self):
        return GraphTraversal(self._session)

    def commit(self):
        self._session.commit()

    def rollback(self):
        self._session.rollback()

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        if exc_type is None:
            self._session.commit()
        else:
            self._session.rollback()
        return False
