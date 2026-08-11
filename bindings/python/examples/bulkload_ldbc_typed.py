#!/usr/bin/env python3
"""
Loads the diversified synthetic LDBC-shaped dataset (see
scripts/generate_synthetic_ldbc.py) via the Python BulkLoader binding,
declaring every DataType plus a FloatVector vector index on "embedding".

Paired with rocksgraph/src/bin/bulk_load_ldbc_typed.rs (same CSV input,
loaded via the Rust BulkLoader) so the two can be cross-validated against
each other with rocksgraph/src/bin/cross_validate_load.rs — that binary
only cares about two on-disk database paths, not which language built them,
so this script's only job is to prove data survives the Python<->Rust PyO3
boundary intact (Vector/Uuid/Bytes marshaling in particular), not to
re-verify BulkLoader-vs-TxnSession equivalence a second time.

Usage: python3 bulkload_ldbc_typed.py <dataset_dir> <db_dir> [max_sst_size_bytes] [sort_memory_bytes]
"""
import os
import sys
import time
from rocksgraph import Graph, GraphOptions, BulkVertex, BulkEdge, DataType, VectorEntityType, DistanceMetric
from rocksgraph import Int32, Int64, Float32, Float64, UInt16, Uuid, Vector

DEFAULT_MAX_SST_SIZE = 2 * 1024 * 1024
DEFAULT_SORT_MEMORY_BYTES = 256 * 1024
VECTOR_DIM = 16


def stream_person_vertices(path):
    print(f"Streaming vertices from {path}...")
    # Naive split('|') — assumes no field value itself contains a literal '|'.
    # Safe for generate_synthetic_ldbc.py's controlled value pools; a real
    # CSV/TSV parser with quoting support would be required for arbitrary input.
    with open(path, "r") as f:
        next(f)  # header
        for line in f:
            parts = line.rstrip("\n").split("|")
            if len(parts) < 17:
                continue

            vid = int(parts[0])
            props = {
                "firstName": parts[1],
                "lastName": parts[2],
                "gender": parts[3],
                "birthday": parts[4],
                "creationDate": parts[5],
                "locationIP": parts[6],
                "browserUsed": parts[7],
                "age": Int32(int(parts[8])),
                "friendCount": Int64(int(parts[9])),
                "rating": Float32(float(parts[10])),
                "balance": Float64(float(parts[11])),
                "isVerified": parts[12] == "true",
                "sessionId": Uuid(parts[13]),
                "signupCode": UInt16(int(parts[14])),
                "avatarHash": bytes.fromhex(parts[15]),
                "embedding": Vector([float(x) for x in parts[16].split(",")]),
            }
            yield BulkVertex(vid, "Person", props)


def stream_knows_edges(path):
    print(f"Streaming edges from {path}...")
    with open(path, "r") as f:
        next(f)  # header
        for line in f:
            parts = line.rstrip("\n").split("|")
            if len(parts) < 4:
                continue

            src, dst = int(parts[0]), int(parts[1])
            props = {"creationDate": parts[2], "weight": Float32(float(parts[3]))}
            yield BulkEdge(src, dst, "knows", props)


def declare_schema(graph):
    schema = graph.open_schema()
    schema.add_vertex_label("Person")
    schema.add_edge_label("knows")

    schema.add_property_key("firstName", DataType.String)
    schema.add_property_key("lastName", DataType.String)
    schema.add_property_key("gender", DataType.String)
    schema.add_property_key("birthday", DataType.String)
    schema.add_property_key("creationDate", DataType.String)
    schema.add_property_key("locationIP", DataType.String)
    schema.add_property_key("browserUsed", DataType.String)
    schema.add_property_key("age", DataType.Int32)
    schema.add_property_key("friendCount", DataType.Int64)
    schema.add_property_key("rating", DataType.Float32)
    schema.add_property_key("balance", DataType.Float64)
    schema.add_property_key("isVerified", DataType.Bool)
    schema.add_property_key("sessionId", DataType.Uuid)
    schema.add_property_key("signupCode", DataType.UInt16)
    schema.add_property_key("avatarHash", DataType.Bytes)
    schema.add_property_key("weight", DataType.Float32)
    schema.add_property_key("embedding", DataType.FloatVector)

    schema.add_vector_index(
        entity_type=VectorEntityType.Vertex,
        property="embedding",
        dimension=VECTOR_DIM,
        metric=DistanceMetric.Cosine,
    )
    # Staged changes are NOT applied until commit() is called explicitly —
    # letting the session go out of scope (or `del`) silently discards them.
    schema.commit()


def main():
    if len(sys.argv) < 3:
        print("Usage: python3 bulkload_ldbc_typed.py <dataset_dir> <db_dir> [max_sst_size_bytes] [sort_memory_bytes]")
        sys.exit(1)

    ldbc_dir = sys.argv[1]
    db_dir = sys.argv[2]
    max_sst_size = int(sys.argv[3]) if len(sys.argv) > 3 else DEFAULT_MAX_SST_SIZE
    sort_memory_bytes = int(sys.argv[4]) if len(sys.argv) > 4 else DEFAULT_SORT_MEMORY_BYTES

    print(f"Opening graph database at {db_dir} (strict mode, typed schema + vector index)...")
    graph = Graph(db_dir, options=GraphOptions(mode="strict"))
    declare_schema(graph)

    loader = graph.open_bulk_loader()
    loader.with_max_sst_size(max_sst_size)
    loader.with_max_memory(sort_memory_bytes)
    print(f"Sort buffer: {sort_memory_bytes} bytes, max_sst_size: {max_sst_size} bytes")

    t0 = time.time()
    person_file = os.path.join(ldbc_dir, "person_0_0.csv")
    loader.load_vertices(stream_person_vertices(person_file))
    t1 = time.time()
    print(f"Finished vertices in {t1 - t0:.2f}s")

    knows_file = os.path.join(ldbc_dir, "person_knows_person_0_0.csv")
    loader.load_edges(stream_knows_edges(knows_file))
    t2 = time.time()
    print(f"Finished edges in {t2 - t1:.2f}s")

    print("Committing bulk load...")
    stats = loader.commit()
    t3 = time.time()
    print(
        f"Commit finished in {t3 - t2:.2f}s — {stats.vertices_written} vertices, "
        f"{stats.edges_written} edges, {stats.sst_files} SST files"
    )
    print(f"Bulk load completed successfully! Total time: {t3 - t0:.2f}s")


if __name__ == "__main__":
    main()
