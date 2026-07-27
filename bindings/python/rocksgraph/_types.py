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
