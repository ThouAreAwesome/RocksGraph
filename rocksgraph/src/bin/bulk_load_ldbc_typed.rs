//! Loads the diversified synthetic LDBC-shaped dataset (see
//! `scripts/generate_synthetic_ldbc.py`) via `BulkLoader`, declaring every
//! `DataType` plus a `FloatVector` vector index on `embedding`.
//!
//! Paired with `oltp_load_ldbc` (same CSV input, loaded via `TxnSession`
//! instead) and `cross_validate_load` (compares the two resulting databases)
//! to cross-validate that `BulkLoader` and the transactional write path
//! produce identical graphs.
//!
//! Run with: `cargo run --release --bin bulk_load_ldbc_typed -- <dataset_dir> <db_path> [max_sst_size_bytes] [sort_memory_bytes]`

#[path = "ldbc_common/mod.rs"]
mod ldbc_common;

use ldbc_common::{declare_schema, parse_knows_edges, parse_person_vertices, DEFAULT_MAX_SST_SIZE};
use rocksgraph::{
    schema::{GraphOptions, SchemaMode},
    Graph, StoreError,
};
use std::{path::Path, time::Instant};

/// Small enough (relative to the ~30MB dataset at the harness's default
/// 50K/250K scale) to force the `ExternalSorter` to spill multiple runs and
/// exercise its cascaded k-way merge, without degenerating into a one-record-
/// per-run pathological case (that extreme — `with_max_memory(1)` — is
/// already covered at tiny scale by `test_load_initial_external_sort` in
/// `rocksgraph/src/bulk/tests.rs`).
const DEFAULT_SORT_MEMORY_BYTES: usize = 256 * 1024;

fn main() -> Result<(), StoreError> {
    let args: Vec<String> = std::env::args().collect();
    let ldbc_dir = args.get(1).map(String::as_str).unwrap_or("/tmp/rocksgraph_cross_validate/dataset");
    let db_path = args.get(2).map(String::as_str).unwrap_or("/tmp/rocksgraph_cross_validate/bulk_db");
    let max_sst_size: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_MAX_SST_SIZE);
    let sort_memory_bytes: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_SORT_MEMORY_BYTES);

    println!("Opening graph database at {} (strict mode, typed schema + vector index)", db_path);
    let graph = Graph::open_with_options(db_path, GraphOptions::default().with_mode(SchemaMode::Strict))?;
    declare_schema(&graph)?;

    let loader = graph.open_bulk_loader()?;
    let mut loader = loader.with_max_memory(sort_memory_bytes).with_max_sst_size(max_sst_size);
    println!("Sort buffer: {sort_memory_bytes} bytes (forces multi-run external sort at this dataset size)");

    let t0 = Instant::now();
    let person_file = Path::new(ldbc_dir).join("person_0_0.csv");
    println!("Streaming vertices from {:?}", person_file);
    loader.load_vertices(parse_person_vertices(&person_file))?;
    println!("Finished vertices in {:.2?}", t0.elapsed());

    let t1 = Instant::now();
    let knows_file = Path::new(ldbc_dir).join("person_knows_person_0_0.csv");
    println!("Streaming edges from {:?}", knows_file);
    loader.load_edges(parse_knows_edges(&knows_file))?;
    println!("Finished edges in {:.2?}", t1.elapsed());

    println!("Committing bulk load (max_sst_size={} bytes)...", max_sst_size);
    let t2 = Instant::now();
    let stats = loader.commit()?;
    println!(
        "Commit finished in {:.2?} — {} vertices, {} edges, {} SST files",
        t2.elapsed(),
        stats.vertices_written,
        stats.edges_written,
        stats.sst_files
    );
    println!("Bulk load completed successfully! Total time: {:.2?}", t0.elapsed());

    Ok(())
}
