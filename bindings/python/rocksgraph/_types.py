class Int32:
    def __init__(self, value: int):
        self.value = value
    def __repr__(self): return f"Int32({self.value})"

class Int64:
    def __init__(self, value: int):
        self.value = value
    def __repr__(self): return f"Int64({self.value})"

class UInt16:
    def __init__(self, value: int):
        self.value = value
    def __repr__(self): return f"UInt16({self.value})"

class Float32:
    def __init__(self, value: float):
        self.value = value
    def __repr__(self): return f"Float32({self.value})"

class Float64:
    def __init__(self, value: float):
        self.value = value
    def __repr__(self): return f"Float64({self.value})"

class Uuid:
    def __init__(self, value: str):
        self.value = value
    def __repr__(self): return f"Uuid('{self.value}')"

class Vector:
    """Wrap a list[float] as a FloatVector for .property("embedding", Vector([0.1, 0.2]))
    
    Supports construction from list, tuple, numpy ndarray, or bytes (packed LE f32).
    """
    __slots__ = ('values',)
    def __init__(self, values):
        if isinstance(values, (list, tuple)):
            self.values = [float(v) for v in values]
        elif isinstance(values, bytes):
            # Packed LE f32 bytes — dimension = len / 4
            import struct
            n = len(values) // 4
            self.values = list(struct.unpack(f'<{n}f', values))
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
        packed = struct.pack(f'<{len(self.values)}f', *self.values)
        return hash(packed)
    def tolist(self):
        return list(self.values)
    def numpy(self):
        """Return as numpy ndarray (requires numpy)."""
        import numpy as np
        return np.array(self.values, dtype=np.float32)
    def __len__(self):
        return len(self.values)
