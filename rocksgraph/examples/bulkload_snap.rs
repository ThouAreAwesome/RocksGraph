use rocksgraph::{
    bulk::{BulkEdge, BulkVertex},
    schema::{GraphOptions, SchemaMode},
    Graph, StoreError,
};
use std::{
    collections::{BTreeSet, HashMap},
    fs::File,
    io::{BufRead, BufReader},
    time::Instant,
};

/// Pass 1: collect every distinct vertex ID mentioned by an edge.
/// BTreeSet gives sorted, de-duplicated IDs — the whole set is buffered in memory
/// (bounded by unique vertex count, not file size), but each vertex is only ~8
/// bytes here, so this stays cheap even at tens of millions of vertices.
fn collect_vertex_ids(path: &str) -> std::io::Result<BTreeSet<i64>> {
    let mut ids = BTreeSet::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        let mut cols = line.split_whitespace();
        if let (Some(src), Some(dst)) = (cols.next(), cols.next()) {
            if let (Ok(src), Ok(dst)) = (src.parse::<i64>(), dst.parse::<i64>()) {
                ids.insert(src);
                ids.insert(dst);
            }
        }
    }
    Ok(ids)
}

/// Pass 2: stream the same file again, this time as edges.
fn stream_edges(path: &str) -> impl Iterator<Item = Result<BulkEdge, StoreError>> {
    let reader = BufReader::new(File::open(path).expect("failed to open file"));
    reader.lines().filter_map(|line| {
        let line = line.ok()?;
        let mut cols = line.split_whitespace();
        let (Some(src), Some(dst)) = (cols.next(), cols.next()) else { return None };
        let (Ok(src), Ok(dst)) = (src.parse::<i64>(), dst.parse::<i64>()) else { return None };
        Some(Ok(BulkEdge { src, dst, label: "knows".into(), props: HashMap::new(), rank: None }))
    })
}

fn main() -> Result<(), StoreError> {
    let args: Vec<String> = std::env::args().collect();
    let dataset_path = args.get(1).map(String::as_str).unwrap_or("bench_data/snap/soc-LiveJournal1-1000.txt");
    let db_path = args.get(2).map(String::as_str).unwrap_or("/tmp/rocks_db_bulk_example");
    let mode = args.get(3).map(String::as_str).unwrap_or("auto");
    let memory_mb: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(128);

    let strict = mode == "strict";
    println!("Opening graph database at {} (mode: {})", db_path, if strict { "strict" } else { "auto" });
    let graph = if strict {
        Graph::open_with_options(db_path, GraphOptions::default().with_mode(SchemaMode::Strict))?
    } else {
        Graph::open(db_path)?
    };

    if strict {
        println!("Defining schema explicitly before bulk load...");
        let mut schema = graph.open_schema();
        schema.add_vertex_label("person");
        schema.add_edge_label("knows");
        schema.commit()?;
    } else {
        println!("Mode: AUTO. Relying on BulkLoader auto-schema discovery...");
    }

    let loader = graph.open_bulk_loader()?;

    // Configure for higher throughput by increasing the sort buffer
    let mut loader = loader.with_max_memory(1024 * 1024 * memory_mb);

    let t0 = Instant::now();

    println!("Phase 1: Collecting unique vertex IDs from {}", dataset_path);
    let vertex_ids = collect_vertex_ids(dataset_path)?;
    let vertices =
        vertex_ids.into_iter().map(|id| Ok(BulkVertex { id, label: "person".into(), props: HashMap::new() }));
    loader.load_vertices(vertices)?;
    let t1 = Instant::now();
    println!("Finished Phase 1 in {:.2?}", t1 - t0);

    println!("\nPhase 2: Streaming edges from {}", dataset_path);
    loader.load_edges(stream_edges(dataset_path))?;
    let t2 = Instant::now();
    println!("Finished Phase 2 in {:.2?}", t2 - t1);

    println!("\nPhase 3: Committing bulk load to database...");
    loader.commit()?;
    let t3 = Instant::now();
    println!("Finished Phase 3 in {:.2?}", t3 - t2);

    println!("\nBulk load completed successfully! Total time: {:.2?}", t3 - t0);

    Ok(())
}
