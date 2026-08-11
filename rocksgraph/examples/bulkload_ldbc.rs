use rocksgraph::{
    bulk::{BulkEdge, BulkVertex},
    schema::{DataType, GraphOptions, SchemaMode},
    Graph, Primitive, StoreError,
};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
    time::Instant,
};

// Naive `split('|')` — assumes no field value itself contains a literal '|'
// (true for LDBC SNB's generated fields, but not a safe assumption for
// arbitrary pipe-delimited input). A real CSV/TSV parser with quoting
// support is required if adapting this pattern to less controlled data.
fn parse_person_vertices(path: &Path) -> impl Iterator<Item = Result<BulkVertex, StoreError>> {
    let reader = BufReader::new(File::open(path).expect("failed to open person file"));
    let mut lines = reader.lines();

    // Skip header: id|firstName|lastName|gender|birthday|creationDate|locationIP|browserUsed
    let _ = lines.next();

    lines.filter_map(|line| {
        let line = line.ok()?;
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 8 {
            return None;
        }
        let id: i64 = parts[0].parse().ok()?;

        let mut props = HashMap::new();
        props.insert("firstName".into(), Primitive::String(parts[1].into()));
        props.insert("lastName".into(), Primitive::String(parts[2].into()));
        props.insert("gender".into(), Primitive::String(parts[3].into()));
        props.insert("birthday".into(), Primitive::String(parts[4].into()));
        props.insert("creationDate".into(), Primitive::String(parts[5].into()));
        props.insert("locationIP".into(), Primitive::String(parts[6].into()));
        props.insert("browserUsed".into(), Primitive::String(parts[7].into()));

        Some(Ok(BulkVertex { id, label: "Person".into(), props }))
    })
}

fn parse_knows_edges(path: &Path) -> impl Iterator<Item = Result<BulkEdge, StoreError>> {
    let reader = BufReader::new(File::open(path).expect("failed to open knows file"));
    let mut lines = reader.lines();

    // Skip header: Person.id|Person.id|creationDate
    let _ = lines.next();

    lines.filter_map(|line| {
        let line = line.ok()?;
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 3 {
            return None;
        }
        let src: i64 = parts[0].parse().ok()?;
        let dst: i64 = parts[1].parse().ok()?;

        let mut props = HashMap::new();
        props.insert("creationDate".into(), Primitive::String(parts[2].into()));

        Some(Ok(BulkEdge { src, dst, label: "knows".into(), props, rank: None }))
    })
}

fn main() -> Result<(), StoreError> {
    let args: Vec<String> = std::env::args().collect();
    let ldbc_dir = args.get(1).map(String::as_str).unwrap_or("bench_data/ldbc");
    let db_path = args.get(2).map(String::as_str).unwrap_or("/tmp/rocks_db_ldbc_example");
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
        schema.add_vertex_label("Person");
        schema.add_edge_label("knows");

        schema.add_property_key("firstName", DataType::String);
        schema.add_property_key("lastName", DataType::String);
        schema.add_property_key("gender", DataType::String);
        schema.add_property_key("birthday", DataType::String);
        schema.add_property_key("creationDate", DataType::String);
        schema.add_property_key("locationIP", DataType::String);
        schema.add_property_key("browserUsed", DataType::String);

        schema.commit()?;
    } else {
        println!("Mode: AUTO. Relying on BulkLoader auto-schema discovery...");
    }

    let loader = graph.open_bulk_loader()?;
    let mut loader = loader.with_max_memory(1024 * 1024 * memory_mb);

    // Process Vertices
    let t0 = Instant::now();
    let person_file = Path::new(ldbc_dir).join("person_0_0.csv");
    println!("Streaming vertices from {:?}", person_file);
    loader.load_vertices(parse_person_vertices(&person_file))?;
    println!("Finished vertices in {:.2?}", t0.elapsed());

    // Process Edges
    let t1 = Instant::now();
    let knows_file = Path::new(ldbc_dir).join("person_knows_person_0_0.csv");
    println!("Streaming edges from {:?}", knows_file);
    loader.load_edges(parse_knows_edges(&knows_file))?;
    println!("Finished edges in {:.2?}", t1.elapsed());

    println!("Committing bulk load...");
    let t2 = Instant::now();
    loader.commit()?;
    println!("Commit finished in {:.2?}", t2.elapsed());
    println!("Bulk load completed successfully! Total time: {:.2?}", t0.elapsed());

    Ok(())
}
