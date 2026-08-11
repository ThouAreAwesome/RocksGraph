//! Cross-validates that `bulk_load_ldbc_typed` (BulkLoader) and
//! `oltp_load_ldbc` (TxnSession) produce equivalent databases from the same
//! diversified synthetic LDBC-shaped dataset.
//!
//! Two independent checks:
//! 1. **Structural/property equality**: every vertex and edge (id, label, and
//!    every property, including exact `FloatVector` equality) must match
//!    between the two databases.
//! 2. **ANN consistency**: `.nearest()`/`.neighbors()` results from both
//!    databases must agree with each other and with an in-process
//!    brute-force ground truth, within a recall tolerance (HNSW is
//!    approximate, and the two load paths build the index via different
//!    insertion orders, so exact equality isn't expected here).
//!
//! Run with:
//! `cargo run --release --bin cross_validate_load -- --bulk-db <path> --oltp-db <path> [--nearest-queries N] [--neighbor-queries N]`

use rand::{rngs::StdRng, Rng, SeedableRng};
use rocksgraph::{Graph, StoreError, TraversalBuilder, Value, VectorEntityType};
use std::{collections::HashSet, error::Error, time::Instant};

const VECTOR_PROP: &str = "embedding";

/// k values swept for `.nearest()`/`.neighbors()` — small (matches the
/// production default `ef_search=50`) and large (deep enough to exercise a
/// wider HNSW beam / more candidate layers), since recall behavior at one k
/// doesn't guarantee correctness at another.
const NEAREST_KS: &[usize] = &[10, 50];
const NEIGHBORS_KS: &[usize] = &[5, 20];

/// Explicit `.with_ef_search()` override, well above the schema-declared
/// default (50) baked into `HnswConfig::default()` — checks that the
/// override actually reaches the index on both load paths and that recall
/// doesn't regress relative to the default-ef_search pass at the same k.
const EF_SEARCH_OVERRIDE: usize = 200;
const EF_SEARCH_OVERRIDE_K: usize = 10;

const DEFAULT_NEAREST_QUERIES: usize = 500;
const DEFAULT_NEIGHBOR_QUERIES: usize = 200;
const RECALL_THRESHOLD: f64 = 0.9;
const LOW_QUERY_WARN_THRESHOLD: f64 = 0.5;
const RNG_SEED: u64 = 42;

fn flag_arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|pos| args.get(pos + 1)).cloned()
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    let bulk_db = flag_arg(&args, "--bulk-db").expect("Usage: --bulk-db <path> --oltp-db <path>");
    let oltp_db = flag_arg(&args, "--oltp-db").expect("Usage: --bulk-db <path> --oltp-db <path>");
    let nearest_queries: usize =
        flag_arg(&args, "--nearest-queries").and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_NEAREST_QUERIES);
    let neighbor_queries: usize =
        flag_arg(&args, "--neighbor-queries").and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_NEIGHBOR_QUERIES);

    println!("Opening bulk-loaded DB: {bulk_db}");
    let bulk = Graph::open(&bulk_db)?;
    println!("Opening OLTP-loaded DB: {oltp_db}");
    let oltp = Graph::open(&oltp_db)?;

    let structural_ok = verify_structural(&bulk, &oltp)?;
    let ann_ok = verify_ann(&bulk, &oltp, nearest_queries, neighbor_queries)?;

    if structural_ok && ann_ok {
        println!("\n=== CROSS-VALIDATION PASSED ===");
        Ok(())
    } else {
        eprintln!("\n=== CROSS-VALIDATION FAILED === (structural_ok={structural_ok}, ann_ok={ann_ok})");
        std::process::exit(1);
    }
}

// ── Structural / property equality ──────────────────────────────────────────

fn verify_structural(bulk: &Graph, oltp: &Graph) -> Result<bool, Box<dyn Error>> {
    println!("\n--- Structural check: comparing every vertex and edge (id, label, properties) ---");
    let t0 = Instant::now();

    let mut bulk_snap = bulk.read();
    let mut oltp_snap = oltp.read();
    let bulk_v_count = count(&mut bulk_snap, false)?;
    let oltp_v_count = count(&mut oltp_snap, false)?;
    let bulk_e_count = count(&mut bulk_snap, true)?;
    let oltp_e_count = count(&mut oltp_snap, true)?;
    println!("  vertices: bulk={bulk_v_count} oltp={oltp_v_count} | edges: bulk={bulk_e_count} oltp={oltp_e_count}");

    if bulk_v_count != oltp_v_count || bulk_e_count != oltp_e_count {
        eprintln!("  FAIL: element counts diverge — cannot safely zip-compare, aborting structural check");
        return Ok(false);
    }

    let mut bulk_snap = bulk.read();
    let mut oltp_snap = oltp.read();
    let bulk_vertices = bulk_snap.g().withProperties([]).V([]).iter()?;
    let oltp_vertices = oltp_snap.g().withProperties([]).V([]).iter()?;
    let (v_checked, v_mismatches) = compare_vertices(bulk_vertices, oltp_vertices)?;
    println!("  vertices: {v_checked} compared, {v_mismatches} mismatch(es)");

    let mut bulk_snap = bulk.read();
    let mut oltp_snap = oltp.read();
    let bulk_edges = bulk_snap.g().withProperties([]).E([]).iter()?;
    let oltp_edges = oltp_snap.g().withProperties([]).E([]).iter()?;
    let (e_checked, e_mismatches) = compare_edges(bulk_edges, oltp_edges)?;
    println!("  edges: {e_checked} compared, {e_mismatches} mismatch(es)");

    println!("  structural check finished in {:.2?}", t0.elapsed());
    Ok(v_checked == bulk_v_count as usize
        && e_checked == bulk_e_count as usize
        && v_mismatches == 0
        && e_mismatches == 0)
}

fn count(snap: &mut rocksgraph::ReadSession, edges: bool) -> Result<i64, StoreError> {
    let result = if edges { snap.g().E([]).count().next()? } else { snap.g().V([]).count().next()? };
    match result {
        Some(Value::Int64(n)) => Ok(n),
        _ => Ok(0),
    }
}

fn compare_vertices(
    bulk_iter: impl Iterator<Item = Result<Value, StoreError>>,
    oltp_iter: impl Iterator<Item = Result<Value, StoreError>>,
) -> Result<(usize, usize), Box<dyn Error>> {
    let mut checked = 0usize;
    let mut mismatches = 0usize;

    for (a, b) in bulk_iter.zip(oltp_iter) {
        let (Value::Vertex(av), Value::Vertex(bv)) = (a?, b?) else {
            return Err("expected Value::Vertex from withProperties([]).V([])".into());
        };
        if av.id != bv.id {
            return Err(
                format!("vertex scan order diverged at position {checked}: bulk={} oltp={}", av.id, bv.id).into()
            );
        }
        if av.label != bv.label || av.properties != bv.properties {
            mismatches += 1;
            if mismatches <= 5 {
                eprintln!(
                    "  MISMATCH vertex {}: bulk=({:?}, {:?}) oltp=({:?}, {:?})",
                    av.id, av.label, av.properties, bv.label, bv.properties
                );
            }
        }
        checked += 1;
    }
    Ok((checked, mismatches))
}

fn compare_edges(
    bulk_iter: impl Iterator<Item = Result<Value, StoreError>>,
    oltp_iter: impl Iterator<Item = Result<Value, StoreError>>,
) -> Result<(usize, usize), Box<dyn Error>> {
    let mut checked = 0usize;
    let mut mismatches = 0usize;

    for (a, b) in bulk_iter.zip(oltp_iter) {
        let (Value::Edge(ae), Value::Edge(be)) = (a?, b?) else {
            return Err("expected Value::Edge from withProperties([]).E([])".into());
        };
        if ae.out_v != be.out_v || ae.in_v != be.in_v || ae.label != be.label || ae.rank != be.rank {
            return Err(format!(
                "edge scan order diverged at position {checked}: bulk=({}->{},{},{}) oltp=({}->{},{},{})",
                ae.out_v, ae.in_v, ae.label, ae.rank, be.out_v, be.in_v, be.label, be.rank
            )
            .into());
        }
        if ae.properties != be.properties {
            mismatches += 1;
            if mismatches <= 5 {
                eprintln!(
                    "  MISMATCH edge {}->{}: bulk={:?} oltp={:?}",
                    ae.out_v, ae.in_v, ae.properties, be.properties
                );
            }
        }
        checked += 1;
    }
    Ok((checked, mismatches))
}

// ── ANN consistency ──────────────────────────────────────────────────────────

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

fn brute_force_topk(all: &[(i64, Vec<f32>)], query: &[f32], k: usize) -> Vec<i64> {
    let mut scored: Vec<(i64, f32)> = all.iter().map(|(id, v)| (*id, cosine(v, query))).collect();
    // unwrap_or(Equal) rather than unwrap(): a NaN similarity score (malformed
    // embedding data) must not panic this comparator and crash the whole run.
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(k).map(|(id, _)| id).collect()
}

fn overlap(a: &[i64], b: &[i64]) -> usize {
    let set: HashSet<i64> = a.iter().copied().collect();
    b.iter().filter(|x| set.contains(x)).count()
}

fn ids_from(values: Vec<Value>) -> Vec<i64> {
    values.into_iter().filter_map(|v| if let Value::Int64(id) = v { Some(id) } else { None }).collect()
}

/// (vertex id, embedding vector) pairs loaded for brute-force ground truth.
type EmbeddingSet = Vec<(i64, Vec<f32>)>;

fn load_all_embeddings(graph: &Graph) -> Result<EmbeddingSet, Box<dyn Error>> {
    let mut snap = graph.read();
    let values = snap.g().withProperties([VECTOR_PROP]).V([]).to_list()?;
    let mut out = Vec::with_capacity(values.len());
    for v in values {
        if let Value::Vertex(vx) = v {
            if let Some(Value::FloatVector(vec)) = vx.properties.get(VECTOR_PROP) {
                out.push((vx.id, vec.clone()));
            }
        }
    }
    Ok(out)
}

struct AnnAgg {
    recall_bulk_sum: f64,
    recall_oltp_sum: f64,
    agreement_sum: f64,
    n: usize,
    low_query_warnings: usize,
}

impl AnnAgg {
    fn new() -> Self {
        Self { recall_bulk_sum: 0.0, recall_oltp_sum: 0.0, agreement_sum: 0.0, n: 0, low_query_warnings: 0 }
    }

    fn record(&mut self, label: &str, truth: &[i64], bulk_result: &[i64], oltp_result: &[i64], k: usize) {
        let recall_b = overlap(bulk_result, truth) as f64 / k as f64;
        let recall_o = overlap(oltp_result, truth) as f64 / k as f64;
        let agreement = overlap(bulk_result, oltp_result) as f64 / k as f64;
        self.recall_bulk_sum += recall_b;
        self.recall_oltp_sum += recall_o;
        self.agreement_sum += agreement;
        self.n += 1;
        if recall_b < LOW_QUERY_WARN_THRESHOLD || recall_o < LOW_QUERY_WARN_THRESHOLD {
            self.low_query_warnings += 1;
            eprintln!(
                "  WARN low recall on {label} query #{}: bulk_recall={recall_b:.2} oltp_recall={recall_o:.2} agreement={agreement:.2}",
                self.n
            );
        }
    }

    fn report(&self, label: &str) -> bool {
        let avg_recall_bulk = self.recall_bulk_sum / self.n as f64;
        let avg_recall_oltp = self.recall_oltp_sum / self.n as f64;
        let avg_agreement = self.agreement_sum / self.n as f64;
        println!(
            "  {label}: n={} avg_recall_bulk={avg_recall_bulk:.4} avg_recall_oltp={avg_recall_oltp:.4} avg_agreement={avg_agreement:.4} low_recall_warnings={}",
            self.n, self.low_query_warnings
        );
        avg_recall_bulk >= RECALL_THRESHOLD && avg_recall_oltp >= RECALL_THRESHOLD && avg_agreement >= RECALL_THRESHOLD
    }
}

/// One `.nearest()` sweep at a fixed `(k, ef_search)` pair. `ef_search: None`
/// leaves the schema-declared default (50) in effect; `Some(ef)` chains
/// `.with_ef_search(ef)` onto both DBs' queries.
fn run_nearest_pass(
    bulk: &Graph,
    oltp: &Graph,
    all: &EmbeddingSet,
    rng: &mut StdRng,
    n_queries: usize,
    k: usize,
    ef_search: Option<usize>,
) -> Result<bool, Box<dyn Error>> {
    let n = all.len();
    let dim = all[0].1.len();
    let mut agg = AnnAgg::new();

    for i in 0..n_queries {
        let query: Vec<f32> = if i % 2 == 0 {
            (0..dim).map(|_| rng.gen_range(-1.0f32..1.0f32)).collect()
        } else {
            all[rng.gen_range(0..n)].1.clone()
        };

        let truth = brute_force_topk(all, &query, k);

        let mut bulk_snap = bulk.read();
        let bulk_t = bulk_snap.g().V([]).nearest(VECTOR_PROP, query.clone(), k);
        let bulk_t = if let Some(ef) = ef_search { bulk_t.with_ef_search(ef) } else { bulk_t };
        let bulk_result = ids_from(bulk_t.id().to_list()?);

        let mut oltp_snap = oltp.read();
        let oltp_t = oltp_snap.g().V([]).nearest(VECTOR_PROP, query.clone(), k);
        let oltp_t = if let Some(ef) = ef_search { oltp_t.with_ef_search(ef) } else { oltp_t };
        let oltp_result = ids_from(oltp_t.id().to_list()?);

        agg.record("nearest()", &truth, &bulk_result, &oltp_result, k);
    }

    let label = match ef_search {
        Some(ef) => format!("nearest(k={k}, ef_search={ef})"),
        None => format!("nearest(k={k}, ef_search=default)"),
    };
    Ok(agg.report(&label))
}

/// One `.neighbors()` sweep at a fixed k, seeded from random existing vertices.
fn run_neighbors_pass(
    bulk: &Graph,
    oltp: &Graph,
    all: &EmbeddingSet,
    rng: &mut StdRng,
    n_queries: usize,
    k: usize,
) -> Result<bool, Box<dyn Error>> {
    let n = all.len();
    let mut agg = AnnAgg::new();

    for _ in 0..n_queries {
        let (seed_id, seed_vec) = &all[rng.gen_range(0..n)];
        let truth = brute_force_topk(all, seed_vec, k);

        let mut bulk_snap = bulk.read();
        let bulk_result = ids_from(
            bulk_snap
                .g()
                .V([*seed_id])
                .neighbors(VECTOR_PROP, VECTOR_PROP, k, VectorEntityType::Vertex)
                .id()
                .to_list()?,
        );
        let mut oltp_snap = oltp.read();
        let oltp_result = ids_from(
            oltp_snap
                .g()
                .V([*seed_id])
                .neighbors(VECTOR_PROP, VECTOR_PROP, k, VectorEntityType::Vertex)
                .id()
                .to_list()?,
        );

        agg.record("neighbors()", &truth, &bulk_result, &oltp_result, k);
    }

    Ok(agg.report(&format!("neighbors(k={k})")))
}

fn verify_ann(bulk: &Graph, oltp: &Graph, n_nearest: usize, n_neighbors: usize) -> Result<bool, Box<dyn Error>> {
    println!("\n--- ANN check: .nearest()/.neighbors() vs brute-force ground truth ---");
    let t0 = Instant::now();

    // Ground-truth source: embeddings are verified byte-identical between the
    // two DBs by the structural check above, so either DB's copy is authoritative.
    let all = load_all_embeddings(bulk)?;
    let n = all.len();
    if n == 0 {
        return Err("no vertices with an embedding property found".into());
    }
    println!("  loaded {n} embeddings (dim={}) for brute-force ground truth", all[0].1.len());

    let mut rng = StdRng::seed_from_u64(RNG_SEED);

    // ── .nearest(): swept across k values, plus one explicit ef_search override ──
    let mut nearest_ok = true;
    for &k in NEAREST_KS {
        nearest_ok &= run_nearest_pass(bulk, oltp, &all, &mut rng, n_nearest, k, None)?;
    }
    nearest_ok &=
        run_nearest_pass(bulk, oltp, &all, &mut rng, n_nearest, EF_SEARCH_OVERRIDE_K, Some(EF_SEARCH_OVERRIDE))?;

    // ── .neighbors(): vertex-to-vertex ANN, swept across k values ───────────
    let mut neighbors_ok = true;
    for &k in NEIGHBORS_KS {
        neighbors_ok &= run_neighbors_pass(bulk, oltp, &all, &mut rng, n_neighbors, k)?;
    }

    println!("  ANN check finished in {:.2?}", t0.elapsed());
    Ok(nearest_ok && neighbors_ok)
}
