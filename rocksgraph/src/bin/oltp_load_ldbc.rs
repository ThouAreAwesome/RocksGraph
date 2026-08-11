//! Loads the diversified synthetic LDBC-shaped dataset (see
//! `scripts/generate_synthetic_ldbc.py`) via the transactional (`TxnSession`)
//! OLTP write path — one `addV`/`addE` + `.property(...)` chain per record,
//! batched into periodic commits for throughput.
//!
//! Paired with `bulk_load_ldbc_typed` (same CSV input, loaded via `BulkLoader`
//! instead) and `cross_validate_load` (compares the two resulting databases)
//! to cross-validate that `BulkLoader` and the transactional write path
//! produce identical graphs. Reuses the exact same CSV parser as the bulk
//! loader (`ldbc_common::parse_person_vertices`/`parse_knows_edges`) so any
//! divergence found by `cross_validate_load` reflects a real loader
//! discrepancy, not a difference in how the two binaries read the input.
//!
//! Run with: `cargo run --release --bin oltp_load_ldbc -- <dataset_dir> <db_path> [batch_size]`

#[path = "ldbc_common/mod.rs"]
mod ldbc_common;

use ldbc_common::{declare_schema, parse_knows_edges, parse_person_vertices, primitive_to_value};
use rocksgraph::{
    schema::{GraphOptions, SchemaMode},
    Graph, StoreError,
};
use std::{path::Path, time::Instant};

const DEFAULT_BATCH_SIZE: usize = 5_000;

fn main() -> Result<(), StoreError> {
    let args: Vec<String> = std::env::args().collect();
    let ldbc_dir = args.get(1).map(String::as_str).unwrap_or("/tmp/rocksgraph_cross_validate/dataset");
    let db_path = args.get(2).map(String::as_str).unwrap_or("/tmp/rocksgraph_cross_validate/oltp_db");
    let batch_size: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_BATCH_SIZE);

    println!("Opening graph database at {} (strict mode, typed schema + vector index)", db_path);
    let graph = Graph::open_with_options(db_path, GraphOptions::default().with_mode(SchemaMode::Strict))?;
    declare_schema(&graph)?;

    let t0 = Instant::now();
    let person_file = Path::new(ldbc_dir).join("person_0_0.csv");
    println!("Loading vertices from {:?} via TxnSession (batch_size={})", person_file, batch_size);
    let mut n_vertices = 0u64;
    let mut txn = graph.begin();
    for result in parse_person_vertices(&person_file) {
        let v = result?;
        let mut step = txn.g().addV(v.label.clone()).property("id", v.id);
        for (key, value) in v.props {
            step = step.property(key, primitive_to_value(value));
        }
        step.next()?;
        n_vertices += 1;
        if n_vertices as usize % batch_size == 0 {
            txn.commit()?;
            txn = graph.begin();
        }
    }
    if n_vertices as usize % batch_size != 0 {
        txn.commit()?;
    }
    println!("Finished {} vertices in {:.2?}", n_vertices, t0.elapsed());

    let t1 = Instant::now();
    let knows_file = Path::new(ldbc_dir).join("person_knows_person_0_0.csv");
    println!("Loading edges from {:?} via TxnSession (batch_size={})", knows_file, batch_size);
    let mut n_edges = 0u64;
    let mut txn = graph.begin();
    for result in parse_knows_edges(&knows_file) {
        let e = result?;
        let mut step = txn.g().addE(e.label.clone()).from(e.src).to(e.dst);
        for (key, value) in e.props {
            step = step.property(key, primitive_to_value(value));
        }
        step.next()?;
        n_edges += 1;
        if n_edges as usize % batch_size == 0 {
            txn.commit()?;
            txn = graph.begin();
        }
    }
    if n_edges as usize % batch_size != 0 {
        txn.commit()?;
    }
    println!("Finished {} edges in {:.2?}", n_edges, t1.elapsed());

    println!("OLTP load completed successfully! Total time: {:.2?}", t0.elapsed());
    Ok(())
}
