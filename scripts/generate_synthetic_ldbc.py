#!/usr/bin/env python3
"""
Generates an LDBC-SNB-shaped (pipe-delimited CSV) synthetic dataset with a
diversified property schema, covering every RocksGraph DataType including
FloatVector, for use by the bulk-load-vs-OLTP cross-validation harness
(see docs on `rocksgraph/src/bin/cross_validate_load.rs`).

This is NOT a substitute for LDBC's real Datagen tool — it has only two entity
types (Person, knows) versus LDBC's dozen-plus, and no realistic social
structure. Its purpose is narrower: exercising every scalar DataType plus
FloatVector across varied per-record data, so that a `BulkLoader`-built
database and a `TxnSession`-built database can be diffed against each other
property-by-property, and their HNSW vector indexes compared via `.nearest()`.

person_0_0.csv columns (pipe-delimited), and the DataType each maps to:
  id            (vertex id, not a property)
  firstName     String
  lastName      String
  gender        String
  birthday      String
  creationDate  String
  locationIP    String
  browserUsed   String
  age           Int32
  friendCount   Int64
  rating        Float32
  balance       Float64
  isVerified    Bool
  sessionId     Uuid   — canonical hyphenated UUID4 text; decode by stripping
                         hyphens and parsing as hex into a u128 (RocksGraph's
                         Uuid primitive is a plain u128, not a formatted string)
  signupCode    UInt16
  avatarHash    Bytes  — hex-encoded; decode with a hex-to-bytes routine
  embedding     FloatVector — VECTOR_DIM comma-joined floats

person_knows_person_0_0.csv columns:
  Person.id | Person.id | creationDate (String) | weight (Float32)
"""
import os
import sys
import random
import time
import uuid

FIRST_NAMES = ["Alice", "Bob", "Carol", "David", "Elena", "Farid", "Grace", "Hiroshi", "Ivy", "Jamal"]
LAST_NAMES = ["Smith", "Johnson", "Garcia", "Müller", "Kim", "Chen", "Ivanova", "Diallo", "Nguyen", "Rossi"]
GENDERS = ["male", "female"]
BROWSERS = ["Firefox", "Chrome", "Safari", "Edge"]

VECTOR_DIM = 16


def random_date(start_year=1960, end_year=2005):
    year = random.randint(start_year, end_year)
    month = random.randint(1, 12)
    day = random.randint(1, 28)
    return f"{year:04d}-{month:02d}-{day:02d}"


def random_ip():
    return f"{random.randint(1, 223)}.{random.randint(0, 255)}.{random.randint(0, 255)}.{random.randint(1, 254)}"


def random_uuid_field():
    return str(uuid.uuid4())


def random_bytes_field(n=16):
    return os.urandom(n).hex()


def random_embedding(dim=VECTOR_DIM):
    # Roughly unit-scale components so cosine/L2 distances behave sensibly.
    return ",".join(f"{random.uniform(-1.0, 1.0):.6f}" for _ in range(dim))


def generate(out_dir, num_vertices, num_edges):
    os.makedirs(out_dir, exist_ok=True)

    person_file = os.path.join(out_dir, "person_0_0.csv")
    knows_file = os.path.join(out_dir, "person_knows_person_0_0.csv")

    print(f"Generating {num_vertices:,} Person vertices with diversified properties (incl. {VECTOR_DIM}-dim embedding)...")
    t0 = time.time()
    report_every = max(num_vertices // 20, 1)
    with open(person_file, "w") as f:
        f.write(
            "id|firstName|lastName|gender|birthday|creationDate|locationIP|browserUsed|"
            "age|friendCount|rating|balance|isVerified|sessionId|signupCode|avatarHash|embedding\n"
        )
        buf = []
        for i in range(1, num_vertices + 1):
            buf.append(
                f"{i}|{random.choice(FIRST_NAMES)}|{random.choice(LAST_NAMES)}|{random.choice(GENDERS)}|"
                f"{random_date()}|{random_date(2018, 2025)}T00:00:00Z|{random_ip()}|{random.choice(BROWSERS)}|"
                f"{random.randint(18, 80)}|{random.randint(0, 5_000_000)}|{random.uniform(0.0, 5.0):.4f}|"
                f"{random.uniform(-10_000.0, 10_000.0):.6f}|{random.choice(['true', 'false'])}|"
                f"{random_uuid_field()}|{random.randint(0, 65535)}|{random_bytes_field()}|{random_embedding()}\n"
            )
            if len(buf) >= 100_000:
                f.writelines(buf)
                buf.clear()
            if i % report_every == 0:
                print(f"  {i:,}/{num_vertices:,} vertices ({time.time()-t0:.1f}s elapsed)")
        f.writelines(buf)
    print(f"Done in {time.time()-t0:.2f}s")

    # Per-vertex sampling instead of a global (src, dst) dedup set: each vertex
    # independently draws `degree` DISTINCT targets via random.sample, so an exact
    # duplicate (src, dst) pair is structurally impossible without ever tracking
    # billions-of-bytes of "seen pairs" state — that global set is what made the
    # naive approach infeasible at large scale (tens of millions of edges would mean
    # tens of millions of Python tuples resident in memory, several GB by itself,
    # with an increasingly expensive membership check as it grows).
    avg_degree = max(1, round(num_edges / num_vertices))
    print(f"Generating ~{num_edges:,} knows edges (avg out-degree {avg_degree}, self-loop-free, no duplicates)...")
    t0 = time.time()
    written = 0
    report_every_v = max(num_vertices // 20, 1)
    with open(knows_file, "w") as f:
        f.write("Person.id|Person.id|creationDate|weight\n")
        buf = []
        for src in range(1, num_vertices + 1):
            degree = max(1, avg_degree + random.randint(-4, 4))
            degree = min(degree, num_vertices - 1)  # can't exceed available non-self targets
            # Sample degree+1 candidates from the full range and drop src if present,
            # rather than sampling from range(1, num_vertices+1) minus {src} directly
            # (building that exclusion set per vertex would reintroduce the same
            # per-call overhead we're trying to avoid).
            candidates = random.sample(range(1, num_vertices + 1), min(degree + 1, num_vertices))
            targets = [c for c in candidates if c != src][:degree]
            for dst in targets:
                buf.append(f"{src}|{dst}|{random_date(2018, 2025)}T00:00:00Z|{random.uniform(0.0, 1.0):.4f}\n")
                written += 1
            if len(buf) >= 100_000:
                f.writelines(buf)
                buf.clear()
            if src % report_every_v == 0:
                print(f"  {src:,}/{num_vertices:,} vertices processed, {written:,} edges written ({time.time()-t0:.1f}s elapsed)")
        f.writelines(buf)
    print(f"Done in {time.time()-t0:.2f}s — wrote {written:,} edges")

    print(f"\nSynthetic LDBC-shaped dataset generated successfully at: {out_dir}")
    print(f"You can now run: scripts/run_cross_validate.sh {out_dir}")


if __name__ == "__main__":
    if len(sys.argv) != 4:
        print("Usage: python3 generate_synthetic_ldbc.py <out_dir> <num_vertices> <num_edges>")
        sys.exit(1)

    generate(sys.argv[1], int(sys.argv[2]), int(sys.argv[3]))
