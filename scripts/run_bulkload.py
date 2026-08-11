#!/usr/bin/env python3
import argparse
import os
import subprocess
import sys
import shutil

def main():
    parser = argparse.ArgumentParser(description="Run RocksGraph BulkLoad Benchmarks")
    parser.add_argument("--dataset", choices=["snap", "ldbc"], required=True, help="Which dataset parser to use")
    parser.add_argument("--lang", choices=["rust", "python"], required=True, help="Which implementation to run")
    parser.add_argument("--mode", choices=["auto", "strict"], default="auto", help="Schema inference mode")
    parser.add_argument("--data-dir", required=True, help="Path to the dataset directory or file")
    parser.add_argument("--db-dir", default="/tmp/rocks_db_bulk_bench", help="Where to output the database")
    parser.add_argument("--memory-mb", type=int, default=128, help="Max memory for the external sorter (MB)")
    
    args = parser.parse_args()

    repo_root = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
    args.data_dir = os.path.abspath(args.data_dir)
    args.db_dir = os.path.abspath(args.db_dir)
    
    # Ensure a clean database directory
    if os.path.exists(args.db_dir):
        shutil.rmtree(args.db_dir)
        
    print(f"============================================================")
    print(f"Running BulkLoad Benchmark")
    print(f"Dataset:  {args.dataset}")
    print(f"Language: {args.lang}")
    print(f"Mode:     {args.mode}")
    print(f"Data Dir: {args.data_dir}")
    print(f"DB Dir:   {args.db_dir}")
    print(f"Memory:   {args.memory_mb} MB")
    print(f"============================================================\n")

    cmd = []
    cwd = repo_root
    env = os.environ.copy()
    
    if args.lang == "rust":
        # cargo run --release --example bulkload_{dataset} -- {data_dir} {db_dir} {mode} {memory}
        cwd = os.path.join(repo_root, "rocksgraph")
        example_name = f"bulkload_{args.dataset}"
        cmd = [
            "cargo", "run", "--release", "--example", example_name, "--",
            args.data_dir, args.db_dir, args.mode, str(args.memory_mb)
        ]
    else:
        # python bindings/python/examples/bulkload_{dataset}.py {data_dir} {db_dir} {mode} {memory}
        script_path = os.path.join(repo_root, "bindings", "python", "examples", f"bulkload_{args.dataset}.py")
        cmd = [
            sys.executable, script_path,
            args.data_dir, args.db_dir, args.mode, str(args.memory_mb)
        ]
        # Ensure PYTHONPATH includes the python bindings, without discarding
        # whatever the caller's environment (e.g. a virtualenv) already has set.
        bindings_path = os.path.join(repo_root, "bindings", "python")
        existing = env.get("PYTHONPATH", "")
        env["PYTHONPATH"] = bindings_path + os.pathsep + existing if existing else bindings_path

    print(f"Executing: {' '.join(cmd)}")
    result = subprocess.run(cmd, cwd=cwd, env=env)
    
    if result.returncode != 0:
        print(f"\nBenchmark failed with exit code {result.returncode}")
        sys.exit(result.returncode)
    else:
        print(f"\nBenchmark completed successfully.")

if __name__ == "__main__":
    main()
