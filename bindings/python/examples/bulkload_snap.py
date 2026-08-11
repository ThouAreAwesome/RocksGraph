#!/usr/bin/env python3
"""
RocksGraph Bulk Loading Example

This script demonstrates how to bulk-load an edge-only dataset (like SNAP LiveJournal).
Since RocksGraph requires explicit BulkVertex records for every vertex referenced
by an edge, this script uses a two-pass approach:
1. Pass 1: Stream the file to collect all unique vertex IDs into a set.
2. Pass 2: Stream the file again to load the edges.

Usage:
  python3 bulkload_snap.py [dataset_path] [db_dir]
"""

import os
import sys
import time

from rocksgraph import Graph, GraphOptions, BulkVertex, BulkEdge

def collect_vertex_ids(path):
    print(f"Pass 1: Collecting unique vertex IDs from {path}...")
    
    # For a dataset with 69M edges (like LiveJournal), a Python set of integers
    # will consume ~200-300MB of RAM. If you are memory constrained, consider
    # using a bytearray bitset or numpy.unique.
    seen = set()
    
    with open(path, 'r') as f:
        for i, line in enumerate(f):
            parts = line.split()
            if len(parts) != 2:
                continue
                
            src, dst = int(parts[0]), int(parts[1])
            
            # Yield BulkVertex objects immediately upon discovering a new ID.
            # This allows BulkLoader to process them (serialize & push to sorter)
            # concurrently while we continue parsing the rest of the file.
            if src not in seen:
                seen.add(src)
                yield BulkVertex(src, "person")
                
            if dst not in seen:
                seen.add(dst)
                yield BulkVertex(dst, "person")
            
            if (i + 1) % 5_000_000 == 0:
                print(f"  Processed {i + 1} lines...")
                
    print(f"Found {len(seen)} unique vertices.")


def stream_edges_from_list(path):
    print(f"\nPass 2: Streaming edges from {path}...")
    with open(path, 'r') as f:
        for i, line in enumerate(f):
            parts = line.split()
            if len(parts) != 2:
                continue
                
            src, dst = int(parts[0]), int(parts[1])
            yield BulkEdge(src, dst, "knows")


def main():
    if len(sys.argv) < 5:
        print("Usage: python3 bulkload_snap.py <dataset_path> <db_dir> <mode> <memory_mb>")
        sys.exit(1)
        
    dataset_path = sys.argv[1]
    db_dir = sys.argv[2]
    mode = sys.argv[3]
    memory_mb = int(sys.argv[4])
    
    if not os.path.exists(dataset_path):
        print(f"Error: Dataset not found at {dataset_path}")
        print("Please download it or provide a valid path.")
        sys.exit(1)
        
    strict = mode == "strict"
    print(f"Opening graph database at {db_dir} (mode: {'strict' if strict else 'auto'})...")
    graph = Graph(db_dir, options=GraphOptions(mode="strict") if strict else None)

    if strict:
        print("Defining schema explicitly before bulk load...")
        schema = graph.open_schema()
        schema.add_vertex_label("person")
        schema.add_edge_label("knows")
        # Staged changes are NOT applied until commit() is called explicitly —
        # letting the session go out of scope (or `del`) silently discards them.
        schema.commit()
    else:
        print("Mode: AUTO. Relying on BulkLoader auto-schema discovery...")
    
    loader = graph.open_bulk_loader()
    
    # Configure for higher throughput by increasing the sort buffer
    loader.with_max_memory(1024 * 1024 * memory_mb)
    
    t0 = time.time()
    
    # 1. Prepare and stream vertices
    vertices = collect_vertex_ids(dataset_path)
    loader.load_vertices(vertices)
    t1 = time.time()
    print(f"Finished Phase 1 (vertices) in {t1 - t0:.2f}s")
    
    # 2. Prepare and stream edges
    edges = stream_edges_from_list(dataset_path)
    loader.load_edges(edges)
    t2 = time.time()
    print(f"Finished Phase 2 (edges) in {t2 - t1:.2f}s")
    
    # 3. Finalize SST generation and atomically ingest into database
    print("\nPhase 3: Committing bulk load to database (sorting & generating SST files)...")
    loader.commit()
    t3 = time.time()
    print(f"Finished Phase 3 (commit) in {t3 - t2:.2f}s")
    
    print(f"\nBulk load completed successfully! Total time: {t3 - t0:.2f}s")

if __name__ == "__main__":
    main()
