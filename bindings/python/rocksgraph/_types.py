from __future__ import annotations

from enum import Enum, IntEnum
from typing import Any


class DataType(IntEnum):
    Null = 0
    Bool = 1
    Int32 = 2
    Int64 = 3
    Float32 = 4
    Float64 = 5
    String = 6
    Uuid = 7
    UInt16 = 8
    Bytes = 9
    FloatVector = 10


class SchemaMode(str, Enum):
    Auto = "auto"
    Strict = "strict"


class EdgeMode(str, Enum):
    Single = "single"
    Multi = "multi"


class VectorEntityType(str, Enum):
    Vertex = "vertex"
    Edge = "edge"


class DistanceMetric(str, Enum):
    Cosine = "cosine"
    Euclidean = "euclidean"
    L2 = "l2"
    DotProduct = "dot_product"


class AnnAlgorithm(str, Enum):
    BruteForce = "brute_force"
    Hnsw = "hnsw"


class Quantization(str, Enum):
    F16 = "f16"
    F32 = "f32"


def _to_enum_value(val: Any, enum_cls: type[Enum]) -> str:
    if isinstance(val, enum_cls):
        return val.value
    if isinstance(val, str):
        val_lower = val.lower()
        for member in enum_cls:
            if val_lower == member.value.lower() or val_lower == member.name.lower():
                return member.value
        valid = [m.value for m in enum_cls]
        raise ValueError(f"Invalid {enum_cls.__name__}: '{val}'. Valid options: {valid}")
    raise TypeError(f"Expected {enum_cls.__name__} or str, got {type(val).__name__}")


class VectorIndexConfig:
    def __init__(
        self,
        property: str,
        dimension: int,
        *,
        entity_type: str | VectorEntityType = VectorEntityType.Vertex,
        metric: str | DistanceMetric = DistanceMetric.Cosine,
        algorithm: str | AnnAlgorithm = AnnAlgorithm.Hnsw,
        m: int = 16,
        ef_construction: int = 200,
        ef_search: int = 50,
        quantization: str | Quantization = Quantization.F16,
    ):
        self.property = str(property)
        self.dimension = int(dimension)
        self.entity_type = _to_enum_value(entity_type, VectorEntityType)
        self.metric = _to_enum_value(metric, DistanceMetric)
        self.algorithm = _to_enum_value(algorithm, AnnAlgorithm)
        self.m = int(m)
        self.ef_construction = int(ef_construction)
        self.ef_search = int(ef_search)
        self.quantization = _to_enum_value(quantization, Quantization)

    def __repr__(self):
        return (
            f"VectorIndexConfig(property='{self.property}', dimension={self.dimension}, "
            f"entity_type='{self.entity_type}', metric='{self.metric}', algorithm='{self.algorithm}')"
        )


class BulkVertex:
    """Represents a vertex to be ingested via BulkLoader."""

    def __init__(self, id: int, label: str, props: dict | None = None):
        self.id = int(id)
        self.label = str(label)
        self.props = props or {}

    def __repr__(self):
        return f"BulkVertex(id={self.id}, label='{self.label}', props={self.props})"


class BulkEdge:
    """Represents an edge to be ingested via BulkLoader."""

    def __init__(self, src: int, dst: int, label: str, props: dict | None = None, rank: int | None = None):
        self.src = int(src)
        self.dst = int(dst)
        self.label = str(label)
        self.props = props or {}
        self.rank = rank

    def __repr__(self):
        return f"BulkEdge(src={self.src}, dst={self.dst}, label='{self.label}', props={self.props}, rank={self.rank})"


class BulkLoadStats:
    """Statistics returned by BulkLoader.commit()."""

    def __init__(self, vertices_written: int, edges_written: int, sst_files: int, duration_secs: float):
        self.vertices_written = vertices_written
        self.edges_written = edges_written
        self.sst_files = sst_files
        self.duration_secs = duration_secs

    def __repr__(self):
        return (
            f"BulkLoadStats(vertices_written={self.vertices_written}, "
            f"edges_written={self.edges_written}, sst_files={self.sst_files}, "
            f"duration_secs={self.duration_secs:.4f})"
        )


class Int32:
    def __init__(self, value: int):
        self.value = value

    def __repr__(self):
        return f"Int32({self.value})"


class Int64:
    def __init__(self, value: int):
        self.value = value

    def __repr__(self):
        return f"Int64({self.value})"


class UInt16:
    def __init__(self, value: int):
        self.value = value

    def __repr__(self):
        return f"UInt16({self.value})"


class Float32:
    def __init__(self, value: float):
        self.value = value

    def __repr__(self):
        return f"Float32({self.value})"


class Float64:
    def __init__(self, value: float):
        self.value = value

    def __repr__(self):
        return f"Float64({self.value})"


class Uuid:
    def __init__(self, value: str):
        self.value = value

    def __repr__(self):
        return f"Uuid('{self.value}')"


class Vector:
    """Wrap a list[float] as a FloatVector for .property("embedding", Vector([0.1, 0.2]))

    Supports construction from list, tuple, numpy ndarray, or bytes (packed LE f32).
    """

    __slots__ = ("values",)

    def __init__(self, values):
        if isinstance(values, (list, tuple)):
            self.values = [float(v) for v in values]
        elif isinstance(values, bytes):
            # Packed LE f32 bytes — dimension = len / 4
            import struct

            n = len(values) // 4
            self.values = list(struct.unpack(f"<{n}f", values))
        else:
            # numpy array or similar
            try:
                self.values = [float(v) for v in values]
            except TypeError:
                raise TypeError(f"Cannot convert {type(values).__name__} to Vector")

    def __repr__(self):
        return f"Vector({self.values})"

    def __eq__(self, other):
        return isinstance(other, Vector) and self.values == other.values

    def __hash__(self):
        # Hash via f32::to_bits — matches Rust implementation with NaN canonicalization
        import struct

        packed = struct.pack(f"<{len(self.values)}f", *self.values)
        return hash(packed)

    def tolist(self):
        return list(self.values)

    def numpy(self):
        """Return as numpy ndarray (requires numpy)."""
        import numpy as np

        return np.array(self.values, dtype=np.float32)

    def __len__(self):
        return len(self.values)
