#!/usr/bin/env python3
import os
import sys
import time
from rocksgraph import Graph, GraphOptions, BulkVertex, BulkEdge, DataType

def stream_person_vertices(path):
    print(f"Streaming vertices from {path}...")
    # Naive split('|') — assumes no field value itself contains a literal '|'
    # (true for LDBC SNB's generated fields, but not safe for arbitrary
    # pipe-delimited input; use the csv module with delimiter='|' if adapting
    # this to less controlled data).
    with open(path, 'r') as f:
        # Skip header
        next(f)
        for line in f:
            parts = line.strip().split('|')
            if len(parts) < 8: continue
            
            vid = int(parts[0])
            props = {
                "firstName": parts[1],
                "lastName": parts[2],
                "gender": parts[3],
                "birthday": parts[4],
                "creationDate": parts[5],
                "locationIP": parts[6],
                "browserUsed": parts[7]
            }
            yield BulkVertex(vid, "Person", props)

def stream_knows_edges(path):
    print(f"Streaming edges from {path}...")
    with open(path, 'r') as f:
        # Skip header
        next(f)
        for line in f:
            parts = line.strip().split('|')
            if len(parts) < 3: continue
            
            src, dst = int(parts[0]), int(parts[1])
            props = {
                "creationDate": parts[2]
            }
            yield BulkEdge(src, dst, "knows", props)

def main():
    if len(sys.argv) < 5:
        print("Usage: python3 bulkload_ldbc.py <dataset_dir> <db_dir> <mode> <memory_mb>")
        sys.exit(1)
        
    ldbc_dir = sys.argv[1]
    db_dir = sys.argv[2]
    mode = sys.argv[3]
    memory_mb = int(sys.argv[4])
    
    strict = mode == "strict"
    print(f"Opening graph database at {db_dir} (mode: {'strict' if strict else 'auto'})...")
    graph = Graph(db_dir, options=GraphOptions(mode="strict") if strict else None)

    if strict:
        print("Defining schema explicitly before bulk load...")
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
        # Staged changes are NOT applied until commit() is called explicitly —
        # letting the session go out of scope (or `del`) silently discards them.
        schema.commit()
    else:
        print("Mode: AUTO. Relying on BulkLoader auto-schema discovery...")
    
    loader = graph.open_bulk_loader()
    loader.with_max_memory(1024 * 1024 * memory_mb)
    
    t0 = time.time()
    
    person_file = os.path.join(ldbc_dir, "person_0_0.csv")
    loader.load_vertices(stream_person_vertices(person_file))
    t1 = time.time()
    print(f"Finished Phase 1 (vertices) in {t1 - t0:.2f}s")
    
    knows_file = os.path.join(ldbc_dir, "person_knows_person_0_0.csv")
    loader.load_edges(stream_knows_edges(knows_file))
    t2 = time.time()
    print(f"Finished Phase 2 (edges) in {t2 - t1:.2f}s")
    
    print("\nPhase 3: Committing bulk load to database...")
    loader.commit()
    t3 = time.time()
    print(f"Finished Phase 3 (commit) in {t3 - t2:.2f}s")
    
    print(f"\nBulk load completed successfully! Total time: {t3 - t0:.2f}s")

if __name__ == "__main__":
    main()
