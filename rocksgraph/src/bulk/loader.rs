// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bulk load via SST ingestion.
//!
//! Provides [`BulkLoader`] which streams vertices and edges through
//! [`ExternalSorter`], writes sorted SST files, and ingests them atomically
//! into an open graph database — bypassing WAL, memtable pressure, and OCC
//! overhead entirely.
//!
//! ## Memory model
//!
//! All sorters spill to `work_dir` when their buffer exceeds
//! `max_memory_bytes / N` (N depends on how many sorters are active
//! concurrently in each phase). Peak RAM is bounded by `max_memory_bytes`
//! regardless of dataset size. There are no in-memory per-vertex or
//! per-edge maps.
//!
//! ## Internal helpers
//!
//! | Type / fn | Role |
//! |---|---|
//! | [`SortedLabelFile`] | Compact on-disk `(VertexKey → LabelId)` file, readable multiple times |
//! | [`DegreeCounter`] | Streams a sorted `(vertex_id, [])` iterator, counting consecutive equal keys |
//! | [`annotate_edges`] | Sort-merge join: attaches `end_vertex_label` to each edge record |
//! | [`write_degree_sst`] | Three-way merge of label file + out-degree + in-degree → degree CF SST |

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Instant;

use rocksdb::{IngestExternalFileOptions, Options, SstFileWriter, WriteBatchWithTransaction};

use crate::{
    schema::{
        definition::{EdgeMode, PropKeyConfig, SchemaMode},
        DataType, GraphOptions, Schema,
    },
    store::rocks::cf_options,
    store::RocksOptions,
    types::{
        gvalue::Primitive,
        keys::{CanonicalEdgeKey, LabelId, Rank, VertexKey},
        kv_codec::{
            self, encode_schema_key, encode_schema_label_value, encode_schema_meta, encode_schema_prop_value,
            SCHEMA_KIND_EDGE_LABEL, SCHEMA_KIND_PROP_KEY, SCHEMA_KIND_VERTEX_LABEL, SCHEMA_META_KEY,
        },
        prop_codec, StoreError,
    },
    vector::VectorRuntimeOptions,
};

use super::sort::ExternalSorter;
use crate::store::rocks::{CF_EDGES_IN, CF_EDGES_OUT, CF_SCHEMA, CF_VERTEX_DEGREE, CF_VERTICES};

// ── Constants ──────────────────────────────────────────────────────────────────

pub(crate) const BULK_LOAD_IN_PROGRESS_KEY: &[u8] = b"_bulk_load_in_progress";

/// Crash marker states written to CF_SCHEMA under `BULK_LOAD_IN_PROGRESS_KEY`.
/// `store.rs` reads these during `recover_bulk_load_crash()` to decide recovery action.
pub(crate) const MARKER_PRE_INGEST: u8 = 1; // SSTs written, ingest not yet started → data unsafe, require retry
pub(crate) const MARKER_POST_INGEST: u8 = 2; // Ingest done, schema/index sync may be incomplete → data safe, clear marker
pub(crate) const MARKER_POST_SNAPSHOT: u8 = 3; // Index snapshots written → data safe, clear marker

// Default SST split threshold: 90% of RocksDB's default target_file_size_base (64 MiB).
const DEFAULT_MAX_SST_SIZE: usize = 58 * 1024 * 1024;
const DEFAULT_MAX_MEMORY_BYTES: usize = 512 * 1024 * 1024;

// ── Public types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BulkSchema {
    pub vertex_labels: Vec<String>,
    pub edge_labels: Vec<String>,
    /// (name, DataType). IDs 1–3 are reserved (id/label/rank); user keys start at 4.
    pub prop_keys: Vec<(String, DataType)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BulkVertex {
    pub id: VertexKey,
    pub label: String,
    pub props: HashMap<String, Primitive>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BulkEdge {
    pub src: VertexKey,
    pub dst: VertexKey,
    pub label: String,
    pub props: HashMap<String, Primitive>,
    /// `None` = auto-assign (Multi mode). Ignored in Single mode (always rank 0).
    pub rank: Option<Rank>,
}

#[derive(Debug)]
pub struct BulkLoadStats {
    pub vertices_written: u64,
    pub edges_written: u64,
    pub sst_files: usize,
    pub duration_secs: f64,
}

/// Trait for types that can be converted into a [`BulkVertex`] or error.
pub trait IntoBulkVertex {
    fn into_bulk_vertex(self) -> Result<BulkVertex, StoreError>;
}

impl IntoBulkVertex for BulkVertex {
    fn into_bulk_vertex(self) -> Result<BulkVertex, StoreError> {
        Ok(self)
    }
}

impl IntoBulkVertex for Result<BulkVertex, StoreError> {
    fn into_bulk_vertex(self) -> Result<BulkVertex, StoreError> {
        self
    }
}

/// Trait for types that can be converted into a [`BulkEdge`] or error.
pub trait IntoBulkEdge {
    fn into_bulk_edge(self) -> Result<BulkEdge, StoreError>;
}

impl IntoBulkEdge for BulkEdge {
    fn into_bulk_edge(self) -> Result<BulkEdge, StoreError> {
        Ok(self)
    }
}

impl IntoBulkEdge for Result<BulkEdge, StoreError> {
    fn into_bulk_edge(self) -> Result<BulkEdge, StoreError> {
        self
    }
}

// ── BulkLoader Session ─────────────────────────────────────────────────────────

/// A session for bulk loading vertices and edges into an open [`Graph`](crate::Graph).
///
/// Created via [`Graph::open_bulk_loader`](crate::Graph::open_bulk_loader).
/// Streams vertices and edges through external sorters, builds sorted SST files offline,
/// and atomically ingests them via `IngestExternalFile`.
pub struct BulkLoader<'a> {
    graph: &'a crate::Graph,
    work_dir: PathBuf,
    max_sst_size: usize,
    max_memory_bytes: usize,
    rocks_opts: RocksOptions,
    committed: bool,

    // Schema handling
    staging_schema: Schema,

    // Phase 1 state (load_vertices / load_edges)
    vertex_sorter: Option<ExternalSorter>,
    label_file: Option<SortedLabelFile>,
    vcount: u64,
    vertices_loaded: bool,

    // Edge state
    dst_annot: Option<ExternalSorter>,
    src_annot: Option<ExternalSorter>,
    out_deg: Option<ExternalSorter>,
    in_deg: Option<ExternalSorter>,
    ecount: u64,
    edges_loaded: bool,

    start_time: Instant,
}

impl<'a> BulkLoader<'a> {
    pub(crate) fn new(graph: &'a crate::Graph) -> Result<Self, StoreError> {
        let db_path = graph.store.db.path();
        let work_dir = db_path.join("_bulk_work");
        let staging_schema = graph.schema.read().map_err(|_| StoreError::LockError)?.clone();
        Ok(Self {
            graph,
            work_dir,
            max_sst_size: DEFAULT_MAX_SST_SIZE,
            max_memory_bytes: DEFAULT_MAX_MEMORY_BYTES,
            rocks_opts: RocksOptions::default(),
            committed: false,
            staging_schema,
            vertex_sorter: None,
            label_file: None,
            vcount: 0,
            vertices_loaded: false,
            dst_annot: None,
            src_annot: None,
            out_deg: None,
            in_deg: None,
            ecount: 0,
            edges_loaded: false,
            start_time: Instant::now(),
        })
    }

    /// Sets the temporary working directory for spilling sort chunks and writing SST files.
    pub fn with_work_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.work_dir = path.into();
        self
    }

    /// Sets the target size in bytes for each generated SST file (defaults to 58 MiB).
    pub fn with_max_sst_size(mut self, bytes: usize) -> Self {
        self.max_sst_size = bytes;
        self
    }

    /// Sets the total memory budget in bytes allocated for sorting passes (defaults to 512 MiB).
    pub fn with_max_memory(mut self, bytes: usize) -> Self {
        self.max_memory_bytes = bytes;
        self
    }

    /// Sets RocksDB options used when constructing SST files.
    pub fn with_rocks_options(mut self, opts: RocksOptions) -> Self {
        self.rocks_opts = opts;
        self
    }

    /// Loads vertices from an iterator. Must be called before [`load_edges`](Self::load_edges).
    pub fn load_vertices<I, V>(&mut self, vertices: I) -> Result<&mut Self, StoreError>
    where
        I: IntoIterator<Item = V>,
        V: IntoBulkVertex,
    {
        if self.vertices_loaded {
            return Err(StoreError::UnsupportedOperation("load_vertices has already been called".into()));
        }

        if self.work_dir.exists() {
            std::fs::remove_dir_all(&self.work_dir).map_err(StoreError::Io)?;
        }
        std::fs::create_dir_all(&self.work_dir).map_err(StoreError::Io)?;

        let budget_v = self.max_memory_bytes / 4;
        let mut vertex_sorter = ExternalSorter::new(self.work_dir.join("sv"), budget_v);
        let mut label_sorter = ExternalSorter::new(self.work_dir.join("sl"), budget_v);
        let mut vcount = 0u64;

        bulk_log(self.start_time, "phase 1 — streaming vertices");

        let is_strict = self.staging_schema.mode == SchemaMode::Strict;

        for v in vertices {
            let v = v.into_bulk_vertex()?;
            let lid = if is_strict {
                self.staging_schema
                    .vertex_label_id(&v.label)
                    .ok_or_else(|| StoreError::SchemaViolation(format!("unknown vertex label '{}'", v.label)))?
            } else {
                self.staging_schema
                    .register_vertex_label(&v.label)
                    .ok_or_else(|| StoreError::SchemaExhausted("vertex label capacity exhausted".into()))?
            };

            let mut id_props = HashMap::with_capacity(v.props.len());
            for (k, val) in &v.props {
                let pkid = if is_strict {
                    self.staging_schema
                        .prop_key_id(k)
                        .ok_or_else(|| StoreError::SchemaViolation(format!("unknown property key '{k}'")))?
                } else {
                    let id = self
                        .staging_schema
                        .register_prop_key(k)
                        .ok_or_else(|| StoreError::SchemaExhausted("property key capacity exhausted".into()))?;
                    self.staging_schema
                        .prop_key_types
                        .entry(id)
                        .or_insert(PropKeyConfig { data_type: DataType::from_primitive(val) });
                    id
                };
                id_props.insert(pkid, val.clone());
            }

            let blob = prop_codec::encode_props(&id_props);
            vertex_sorter.push(
                kv_codec::encode_vertex_key(v.id).to_vec(),
                kv_codec::VertexValue { label_id: lid, property_blob: blob }.encode(),
            )?;
            label_sorter.push(v.id.to_be_bytes().to_vec(), lid.to_be_bytes().to_vec())?;
            vcount += 1;
        }

        let label_file_path = self.work_dir.join("vertex_labels.bin");
        let label_file = SortedLabelFile::write_from(label_sorter, &label_file_path)?;
        bulk_log(
            self.start_time,
            &format!("phase 1 — {vcount} vertices; label file written ({} unique)", label_file.count),
        );

        self.vertex_sorter = Some(vertex_sorter);
        self.label_file = Some(label_file);
        self.vcount = vcount;
        self.vertices_loaded = true;

        Ok(self)
    }

    /// Loads edges from an iterator. Must be called after [`load_vertices`](Self::load_vertices).
    pub fn load_edges<I, E>(&mut self, edges: I) -> Result<&mut Self, StoreError>
    where
        I: IntoIterator<Item = E>,
        E: IntoBulkEdge,
    {
        if !self.vertices_loaded {
            return Err(StoreError::VerticesNotLoaded);
        }
        if self.edges_loaded {
            return Err(StoreError::UnsupportedOperation("load_edges has already been called".into()));
        }

        let budget_a = self.max_memory_bytes / 4;
        let budget_d = self.max_memory_bytes / 8;
        let mut dst_annot = ExternalSorter::new(self.work_dir.join("ea_dst"), budget_a);
        let mut src_annot = ExternalSorter::new(self.work_dir.join("ea_src"), budget_a);
        let mut out_deg = ExternalSorter::new(self.work_dir.join("deg_out"), budget_d);
        let mut in_deg = ExternalSorter::new(self.work_dir.join("deg_in"), budget_d);
        let is_strict = self.staging_schema.mode == SchemaMode::Strict;
        let edge_mode = self.staging_schema.edge_mode;

        let ecount;
        match edge_mode {
            EdgeMode::Single => {
                bulk_log(self.start_time, "phase 1 — streaming edges (Single mode)");
                let mut n = 0u64;
                let mut last_report = Instant::now();
                for edge in edges {
                    let edge = edge.into_bulk_edge()?;
                    let lid = if is_strict {
                        self.staging_schema.edge_label_id(&edge.label).ok_or_else(|| {
                            StoreError::SchemaViolation(format!("unknown edge label '{}'", edge.label))
                        })?
                    } else {
                        self.staging_schema
                            .register_edge_label(&edge.label)
                            .ok_or_else(|| StoreError::SchemaExhausted("edge label capacity exhausted".into()))?
                    };

                    let mut id_props = HashMap::with_capacity(edge.props.len());
                    for (k, val) in &edge.props {
                        let pkid = if is_strict {
                            self.staging_schema
                                .prop_key_id(k)
                                .ok_or_else(|| StoreError::SchemaViolation(format!("unknown property key '{k}'")))?
                        } else {
                            let id = self
                                .staging_schema
                                .register_prop_key(k)
                                .ok_or_else(|| StoreError::SchemaExhausted("property key capacity exhausted".into()))?;
                            self.staging_schema
                                .prop_key_types
                                .entry(id)
                                .or_insert(PropKeyConfig { data_type: DataType::from_primitive(val) });
                            id
                        };
                        id_props.insert(pkid, val.clone());
                    }

                    let blob = prop_codec::encode_props(&id_props);
                    let cek = CanonicalEdgeKey { src_id: edge.src, label_id: lid, dst_id: edge.dst, rank: 0 };

                    let mut dk = [0u8; 30];
                    dk[0..8].copy_from_slice(&edge.dst.to_be_bytes());
                    dk[8..30].copy_from_slice(&kv_codec::encode_edge_key(&cek.out_key()));
                    dst_annot.push(dk.to_vec(), blob.clone())?;

                    let mut sk = [0u8; 30];
                    sk[0..8].copy_from_slice(&edge.src.to_be_bytes());
                    sk[8..30].copy_from_slice(&kv_codec::encode_edge_key(&cek.in_key()));
                    src_annot.push(sk.to_vec(), blob)?;

                    out_deg.push(edge.src.to_be_bytes().to_vec(), vec![])?;
                    in_deg.push(edge.dst.to_be_bytes().to_vec(), vec![])?;

                    n += 1;
                    if n % 1_000_000 == 0 && last_report.elapsed().as_secs_f64() >= 5.0 {
                        bulk_log(self.start_time, &format!("  {n} edges streamed"));
                        last_report = Instant::now();
                    }
                }
                ecount = n;
            }
            EdgeMode::Multi => {
                let pre_budget = self.max_memory_bytes / 4;
                let mut pre_sorter = ExternalSorter::new(self.work_dir.join("sm_pre"), pre_budget);
                let mut n = 0u64;
                let mut last_report = Instant::now();

                bulk_log(self.start_time, "phase 1 — streaming edges into pre-sorter (Multi mode)");
                for edge in edges {
                    let edge = edge.into_bulk_edge()?;
                    let lid = if is_strict {
                        self.staging_schema.edge_label_id(&edge.label).ok_or_else(|| {
                            StoreError::SchemaViolation(format!("unknown edge label '{}'", edge.label))
                        })?
                    } else {
                        self.staging_schema
                            .register_edge_label(&edge.label)
                            .ok_or_else(|| StoreError::SchemaExhausted("edge label capacity exhausted".into()))?
                    };

                    let mut id_props = HashMap::with_capacity(edge.props.len());
                    for (k, val) in &edge.props {
                        let pkid = if is_strict {
                            self.staging_schema
                                .prop_key_id(k)
                                .ok_or_else(|| StoreError::SchemaViolation(format!("unknown property key '{k}'")))?
                        } else {
                            let id = self
                                .staging_schema
                                .register_prop_key(k)
                                .ok_or_else(|| StoreError::SchemaExhausted("property key capacity exhausted".into()))?;
                            self.staging_schema
                                .prop_key_types
                                .entry(id)
                                .or_insert(PropKeyConfig { data_type: DataType::from_primitive(val) });
                            id
                        };
                        id_props.insert(pkid, val.clone());
                    }

                    let blob = prop_codec::encode_props(&id_props);
                    let rank_for_sort = edge.rank.unwrap_or(Rank::MAX);
                    let mut sort_key = [0u8; 22];
                    sort_key[0..8].copy_from_slice(&edge.src.to_be_bytes());
                    sort_key[8..12].copy_from_slice(&lid.to_be_bytes());
                    sort_key[12..20].copy_from_slice(&edge.dst.to_be_bytes());
                    sort_key[20..22].copy_from_slice(&rank_for_sort.to_be_bytes());
                    pre_sorter.push(sort_key.to_vec(), blob)?;
                    n += 1;
                    if n % 1_000_000 == 0 && last_report.elapsed().as_secs_f64() >= 5.0 {
                        bulk_log(self.start_time, &format!("  {n} edges streamed"));
                        last_report = Instant::now();
                    }
                }
                bulk_log(self.start_time, &format!("phase 1 — {n} edges pre-sorted; assigning ranks"));

                let mut last_prefix: Option<[u8; 20]> = None;
                let mut next_rank: Rank = 0;
                let mut last_explicit: Option<Rank> = None;

                for item in pre_sorter.finish()? {
                    let (sort_key, blob) = item?;
                    let key22: [u8; 22] = sort_key
                        .try_into()
                        .map_err(|_| StoreError::CorruptData("corrupt pre-sort key in Multi mode"))?;
                    let prefix: [u8; 20] = key22[0..20].try_into().unwrap();
                    let rank_from_key = Rank::from_be_bytes(key22[20..22].try_into().unwrap());
                    let src = VertexKey::from_be_bytes(prefix[0..8].try_into().unwrap());
                    let lid = LabelId::from_be_bytes(prefix[8..12].try_into().unwrap());
                    let dst = VertexKey::from_be_bytes(prefix[12..20].try_into().unwrap());

                    if Some(prefix) != last_prefix {
                        next_rank = 0;
                        last_explicit = None;
                        last_prefix = Some(prefix);
                    }
                    let rank = if rank_from_key == Rank::MAX {
                        let r = next_rank;
                        next_rank = next_rank.checked_add(1).ok_or_else(|| {
                            StoreError::SchemaViolation(format!("rank overflow for edge ({src}->{dst})"))
                        })?;
                        r
                    } else {
                        if let Some(prev) = last_explicit {
                            if prev == rank_from_key {
                                return Err(StoreError::DuplicateEdge(CanonicalEdgeKey {
                                    src_id: src,
                                    label_id: lid,
                                    dst_id: dst,
                                    rank: rank_from_key,
                                }));
                            }
                        }
                        last_explicit = Some(rank_from_key);
                        next_rank = rank_from_key.checked_add(1).ok_or_else(|| {
                            StoreError::SchemaViolation(format!("rank overflow for edge ({src}->{dst})"))
                        })?;
                        rank_from_key
                    };

                    let cek = CanonicalEdgeKey { src_id: src, label_id: lid, dst_id: dst, rank };
                    let mut dk = [0u8; 30];
                    dk[0..8].copy_from_slice(&dst.to_be_bytes());
                    dk[8..30].copy_from_slice(&kv_codec::encode_edge_key(&cek.out_key()));
                    dst_annot.push(dk.to_vec(), blob.clone())?;
                    let mut sk = [0u8; 30];
                    sk[0..8].copy_from_slice(&src.to_be_bytes());
                    sk[8..30].copy_from_slice(&kv_codec::encode_edge_key(&cek.in_key()));
                    src_annot.push(sk.to_vec(), blob)?;
                    out_deg.push(src.to_be_bytes().to_vec(), vec![])?;
                    in_deg.push(dst.to_be_bytes().to_vec(), vec![])?;
                }
                ecount = n;
            }
        }

        bulk_log(self.start_time, &format!("phase 1 — {ecount} edges streamed and sorted"));

        self.dst_annot = Some(dst_annot);
        self.src_annot = Some(src_annot);
        self.out_deg = Some(out_deg);
        self.in_deg = Some(in_deg);
        self.ecount = ecount;
        self.edges_loaded = true;

        Ok(self)
    }

    /// Atomically ingests all generated SST files and synchronizes schema changes.
    pub fn commit(mut self) -> Result<BulkLoadStats, StoreError> {
        if !self.vertices_loaded {
            return Err(StoreError::VerticesNotLoaded);
        }

        let vertex_sorter = self.vertex_sorter.take().unwrap();
        let label_file = self.label_file.take().unwrap();

        let budget_d = self.max_memory_bytes / 8;
        let dst_annot =
            self.dst_annot.take().unwrap_or_else(|| ExternalSorter::new(self.work_dir.join("ea_dst_empty"), budget_d));
        let src_annot =
            self.src_annot.take().unwrap_or_else(|| ExternalSorter::new(self.work_dir.join("ea_src_empty"), budget_d));
        let out_deg =
            self.out_deg.take().unwrap_or_else(|| ExternalSorter::new(self.work_dir.join("deg_out_empty"), budget_d));
        let in_deg =
            self.in_deg.take().unwrap_or_else(|| ExternalSorter::new(self.work_dir.join("deg_in_empty"), budget_d));

        let v_bo = cf_options::vertex_block_opts(&self.rocks_opts);
        let v_opts = cf_options::vertex_cf_opts(&self.rocks_opts, &v_bo);
        let e_bo = cf_options::edge_block_opts(&self.rocks_opts);
        let e_opts = cf_options::edge_cf_opts(&self.rocks_opts, &e_bo);

        // SST finalization — generates sorted SST files before the crash-safe window opens.
        // A crash here leaves no SSTs ingested; restart is safe.
        bulk_log(self.start_time, &format!("commit/sst — writing vertex SSTs ({} vertices)", self.vcount));
        let vert_files =
            write_sst_from_iter("vertices", &mut vertex_sorter.finish()?, &self.work_dir, self.max_sst_size, &v_opts)?;

        bulk_log(self.start_time, "commit/sst — writing degree SSTs");
        let deg_files = write_degree_sst(
            &label_file,
            out_deg.finish()?,
            in_deg.finish()?,
            &self.work_dir,
            self.max_sst_size,
            &v_opts,
        )?;

        bulk_log(self.start_time, "commit/sst — annotating + writing edges_out SSTs");
        let mut out_edge_sorter = ExternalSorter::new(self.work_dir.join("eo"), self.max_memory_bytes);
        annotate_edges(dst_annot.finish()?, &label_file, &mut out_edge_sorter)?;
        let out_files = write_sst_from_iter_dedup(
            "edges_out",
            out_edge_sorter.finish()?,
            &self.work_dir,
            self.max_sst_size,
            &e_opts,
        )?;

        bulk_log(self.start_time, "commit/sst — annotating + writing edges_in SSTs");
        let mut in_edge_sorter = ExternalSorter::new(self.work_dir.join("ei"), self.max_memory_bytes);
        annotate_edges(src_annot.finish()?, &label_file, &mut in_edge_sorter)?;
        let in_files =
            write_sst_from_iter("edges_in", &mut in_edge_sorter.finish()?, &self.work_dir, self.max_sst_size, &e_opts)?;

        let sst_paths = SstPaths { vertices: vert_files, degree: deg_files, edges_out: out_files, edges_in: in_files };

        let db = &self.graph.store.db;

        // Phase 1 (commit): Write crash marker (§5 design_bulk_loader.md)
        let cf_sch = db.cf_handle(CF_SCHEMA).ok_or(StoreError::MissingColumnFamily(CF_SCHEMA))?;
        let mut marker_batch = WriteBatchWithTransaction::<true>::default();
        marker_batch.put_cf(&cf_sch, BULK_LOAD_IN_PROGRESS_KEY, [MARKER_PRE_INGEST]);
        db.write(marker_batch).map_err(StoreError::RocksDb)?;

        // Phase 2 (commit): IngestExternalFile (atomic, all CFs)
        bulk_log(self.start_time, &format!("phase 2 — ingesting {} SST files (atomic)", sst_paths.total_files()));
        let mut ingest_opts = IngestExternalFileOptions::default();
        ingest_opts.set_move_files(true);

        macro_rules! ingest {
            ($paths:expr, $cf_name:expr) => {
                if !$paths.is_empty() {
                    let cf = db.cf_handle($cf_name).ok_or(StoreError::MissingColumnFamily($cf_name))?;
                    db.ingest_external_file_cf_opts(&cf, &ingest_opts, $paths.to_vec()).map_err(StoreError::RocksDb)?;
                }
            };
        }
        ingest!(&sst_paths.vertices, CF_VERTICES);
        ingest!(&sst_paths.degree, CF_VERTEX_DEGREE);
        ingest!(&sst_paths.edges_out, CF_EDGES_OUT);
        ingest!(&sst_paths.edges_in, CF_EDGES_IN);

        // Update crash marker: data is now durable, schema/index build may
        // still be in progress. If we crash here, recovery sees POST_INGEST
        // and knows the data is safe.
        {
            let mut post_batch = WriteBatchWithTransaction::<true>::default();
            post_batch.put_cf(&cf_sch, BULK_LOAD_IN_PROGRESS_KEY, [MARKER_POST_INGEST]);
            db.write(post_batch).map_err(StoreError::RocksDb)?;
        }

        // Phase 3 (commit): Schema sync
        if self.staging_schema.mode == SchemaMode::Auto {
            let mut schema_batch = WriteBatchWithTransaction::<true>::default();
            let mut changed = false;

            // Sync vertex labels
            for (&id, name) in &self.staging_schema.vertex_labels {
                if !self.staging_schema.persisted_vertex_labels.contains(&id) {
                    let key = encode_schema_key(SCHEMA_KIND_VERTEX_LABEL, name);
                    let val = encode_schema_label_value(id);
                    schema_batch.put_cf(&cf_sch, key, val);
                    self.staging_schema.persisted_vertex_labels.insert(id);
                    changed = true;
                }
            }
            // Sync edge labels
            for (&id, name) in &self.staging_schema.edge_labels {
                if !self.staging_schema.persisted_edge_labels.contains(&id) {
                    let key = encode_schema_key(SCHEMA_KIND_EDGE_LABEL, name);
                    let val = encode_schema_label_value(id);
                    schema_batch.put_cf(&cf_sch, key, val);
                    self.staging_schema.persisted_edge_labels.insert(id);
                    changed = true;
                }
            }
            // Sync property keys
            for (&id, name) in &self.staging_schema.prop_keys {
                if !self.staging_schema.persisted_prop_keys.contains(&id) {
                    let dt = self.staging_schema.prop_key_types.get(&id).map(|c| c.data_type).unwrap_or(DataType::Null);
                    let key = encode_schema_key(SCHEMA_KIND_PROP_KEY, name);
                    let val = encode_schema_prop_value(id, dt.to_u8());
                    schema_batch.put_cf(&cf_sch, key, val);
                    self.staging_schema.persisted_prop_keys.insert(id);
                    changed = true;
                }
            }

            if changed {
                self.staging_schema.version += 1;
                let meta = encode_schema_meta(
                    self.staging_schema.version,
                    self.staging_schema.edge_mode.to_u8(),
                    self.staging_schema.mode.to_u8(),
                );
                schema_batch.put_cf(&cf_sch, SCHEMA_META_KEY, meta);
                db.write(schema_batch).map_err(StoreError::RocksDb)?;

                let mut live_schema = self.graph.schema.write().map_err(|_| StoreError::LockError)?;
                *live_schema = self.staging_schema.clone();
            }
        }

        // TODO(v0.2): Phase 4 (commit): Build HNSW indexes for declared VectorIndexConfigs.
        // Scan CF_VERTICES for FloatVector values matching each (entity_type, prop_key_id),
        // batch-insert into HNSW, update marker to MARKER_POST_INDEX.
        //
        // TODO(v0.2): Phase 5 (commit): Write HNSW snapshots.
        // For each built index, write snapshot atomically, update marker to MARKER_POST_SNAPSHOT.

        // Phase 6 (commit): Clear crash marker
        let mut cleanup_batch = WriteBatchWithTransaction::<true>::default();
        cleanup_batch.delete_cf(&cf_sch, BULK_LOAD_IN_PROGRESS_KEY);
        db.write(cleanup_batch).map_err(StoreError::RocksDb)?;

        // Post-commit: compact all data CFs (not crash-critical; work_dir already cleaned by Drop if we crash here)
        let n_files = sst_paths.total_files();
        bulk_log(
            self.start_time,
            &format!("done — {} vertices, {} edges, {n_files} SST files", self.vcount, self.ecount),
        );
        bulk_log(self.start_time, "compacting all CFs (moves L0 SSTs into deeper levels for fast scans)");
        for cf_name in [CF_VERTICES, CF_VERTEX_DEGREE, CF_EDGES_OUT, CF_EDGES_IN] {
            if let Some(cf) = db.cf_handle(cf_name) {
                db.compact_range_cf(&cf, None::<&[u8]>, None::<&[u8]>);
            }
        }
        bulk_log(self.start_time, "compaction done");

        // Clean up work directory
        if self.work_dir.exists() {
            let _ = std::fs::remove_dir_all(&self.work_dir);
        }

        self.committed = true;
        self.graph.bulk_load_in_progress.store(false, Ordering::Release);
        let duration_secs = self.start_time.elapsed().as_secs_f64();

        Ok(BulkLoadStats {
            vertices_written: self.vcount,
            edges_written: self.ecount,
            sst_files: n_files,
            duration_secs,
        })
    }
}

impl<'a> Drop for BulkLoader<'a> {
    fn drop(&mut self) {
        if !self.committed {
            self.graph.bulk_load_in_progress.store(false, Ordering::Release);
            if self.work_dir.exists() {
                let _ = std::fs::remove_dir_all(&self.work_dir);
            }
        }
    }
}

// ── Deprecated Standalone SstBulkLoader ────────────────────────────────────────

/// Standalone bulk loader (deprecated in favor of [`Graph::open_bulk_loader`](crate::Graph::open_bulk_loader)).
#[deprecated(since = "0.3.0", note = "use `Graph::open_bulk_loader()` instead")]
pub struct SstBulkLoader {
    db_path: PathBuf,
    work_dir: PathBuf,
    max_sst_size: usize,
    max_memory_bytes: usize,
}

#[allow(deprecated)]
impl SstBulkLoader {
    /// Creates a new bulk loader instance.
    pub fn new(db_path: impl Into<PathBuf>, work_dir: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            work_dir: work_dir.into(),
            max_sst_size: DEFAULT_MAX_SST_SIZE,
            max_memory_bytes: DEFAULT_MAX_MEMORY_BYTES,
        }
    }

    pub fn with_max_sst_size(mut self, bytes: usize) -> Self {
        self.max_sst_size = bytes;
        self
    }

    pub fn with_max_memory(mut self, bytes: usize) -> Self {
        self.max_memory_bytes = bytes;
        self
    }

    /// Load an initial dataset into an empty database at `db_path`.
    pub fn load_initial(
        self,
        schema: BulkSchema,
        vertices: impl Iterator<Item = BulkVertex>,
        edges: impl Iterator<Item = BulkEdge>,
        graph_opts: GraphOptions,
        rocks_opts: &crate::RocksOptions,
    ) -> Result<BulkLoadStats, StoreError> {
        let graph = crate::Graph::open_with_rocksdb_options(
            &self.db_path,
            graph_opts,
            rocks_opts.clone(),
            VectorRuntimeOptions::default(),
        )?;
        if graph_opts.mode == SchemaMode::Strict {
            let mut session = graph.open_schema();
            for vl in &schema.vertex_labels {
                session.add_vertex_label(vl);
            }
            for el in &schema.edge_labels {
                session.add_edge_label(el);
            }
            for (pk, dt) in &schema.prop_keys {
                session.add_property_key(pk, *dt);
            }
            session.commit()?;
        }
        let mut loader = graph
            .open_bulk_loader()?
            .with_work_dir(self.work_dir)
            .with_max_sst_size(self.max_sst_size)
            .with_max_memory(self.max_memory_bytes)
            .with_rocks_options(rocks_opts.clone());
        loader.load_vertices(vertices)?;
        loader.load_edges(edges)?;
        loader.commit()
    }
}

// ── Progress reporting ─────────────────────────────────────────────────────────

fn bulk_log(start: Instant, msg: &str) {
    eprintln!("[bulk {:>7.1}s] {}", start.elapsed().as_secs_f64(), msg);
}

// ── SortedLabelFile ────────────────────────────────────────────────────────────

/// Compact on-disk sorted file of (VertexKey, LabelId) pairs (12 bytes each).
/// Written once from an ExternalSorter with deduplication; readable independently
/// multiple times (two annotation passes + one degree pass). Deleted on drop.
#[derive(Debug)]
struct SortedLabelFile {
    path: PathBuf,
    count: u64,
}

impl SortedLabelFile {
    fn write_from(sorter: ExternalSorter, path: &Path) -> Result<Self, StoreError> {
        let file = File::create(path).map_err(StoreError::Io)?;
        let mut w = BufWriter::new(file);
        w.write_all(&0u64.to_le_bytes()).map_err(StoreError::Io)?;
        let mut count = 0u64;
        let mut last: Option<(VertexKey, LabelId)> = None;
        for item in sorter.finish()? {
            let (key, val) = item?;
            let vid = VertexKey::from_be_bytes(
                key.try_into().map_err(|_| StoreError::CorruptData("label sorter: key must be 8 bytes"))?,
            );
            let lid = LabelId::from_be_bytes(
                val.try_into().map_err(|_| StoreError::CorruptData("label sorter: value must be 4 bytes"))?,
            );
            if let Some((lv, ll)) = last {
                if lv == vid {
                    if ll != lid {
                        return Err(StoreError::SchemaViolation(format!(
                            "vertex {vid} appears with conflicting labels in input"
                        )));
                    }
                    continue;
                }
            }
            last = Some((vid, lid));
            w.write_all(&vid.to_be_bytes()).map_err(StoreError::Io)?;
            w.write_all(&lid.to_be_bytes()).map_err(StoreError::Io)?;
            count += 1;
        }
        w.flush().map_err(StoreError::Io)?;
        let mut file = w.into_inner().map_err(|e| StoreError::Io(e.into_error()))?;
        file.seek(SeekFrom::Start(0)).map_err(StoreError::Io)?;
        file.write_all(&count.to_le_bytes()).map_err(StoreError::Io)?;
        Ok(Self { path: path.to_owned(), count })
    }

    fn reader(&self) -> Result<LabelFileIter, StoreError> {
        let file = File::open(&self.path).map_err(StoreError::Io)?;
        let mut reader = BufReader::new(file);
        let mut buf = [0u8; 8];
        reader.read_exact(&mut buf).map_err(StoreError::Io)?;
        Ok(LabelFileIter { reader, remaining: u64::from_le_bytes(buf) })
    }
}

impl Drop for SortedLabelFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Sequential reader over a [`SortedLabelFile`].
struct LabelFileIter {
    reader: BufReader<File>,
    remaining: u64,
}

impl Iterator for LabelFileIter {
    type Item = Result<(VertexKey, LabelId), StoreError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let mut buf = [0u8; 12];
        if let Err(e) = self.reader.read_exact(&mut buf) {
            return Some(Err(StoreError::Io(e)));
        }
        Some(Ok((
            VertexKey::from_be_bytes(buf[0..8].try_into().unwrap()),
            LabelId::from_be_bytes(buf[8..12].try_into().unwrap()),
        )))
    }
}

// ── DegreeCounter ──────────────────────────────────────────────────────────────

/// Wraps a sorted iterator of (vertex_id:8, []) keys.
/// Counts consecutive equal keys for a given vertex_id — O(1) memory.
struct DegreeCounter<I: Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>>> {
    iter: I,
    head: Option<VertexKey>,
}

impl<I: Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>>> DegreeCounter<I> {
    fn new(mut iter: I) -> Result<Self, StoreError> {
        let head = Self::advance(&mut iter)?;
        Ok(Self { iter, head })
    }

    fn advance(iter: &mut I) -> Result<Option<VertexKey>, StoreError> {
        match iter.next() {
            None => Ok(None),
            Some(Err(e)) => Err(e),
            Some(Ok((key, _))) => Ok(Some(VertexKey::from_be_bytes(
                key.try_into().map_err(|_| StoreError::CorruptData("degree sorter: key must be 8 bytes"))?,
            ))),
        }
    }

    fn count_for(&mut self, vid: VertexKey) -> Result<u32, StoreError> {
        let mut count = 0u32;
        loop {
            match self.head {
                None => return Ok(count),
                Some(cur) if cur < vid => {
                    self.head = Self::advance(&mut self.iter)?;
                }
                Some(cur) if cur == vid => {
                    count += 1;
                    self.head = Self::advance(&mut self.iter)?;
                }
                _ => return Ok(count),
            }
        }
    }
}

// ── annotate_edges ─────────────────────────────────────────────────────────────

fn annotate_edges(
    annot_iter: impl Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>>,
    label_file: &SortedLabelFile,
    out_sorter: &mut ExternalSorter,
) -> Result<(), StoreError> {
    let mut label_iter = label_file.reader()?;
    let mut cur: Option<(VertexKey, LabelId)> = label_iter.next().transpose()?;
    let mut cached: Option<(VertexKey, LabelId)> = None;

    for item in annot_iter {
        let (key, props) = item?;
        if key.len() != 30 {
            return Err(StoreError::CorruptData("annotation key must be 30 bytes"));
        }
        let lookup_id = VertexKey::from_be_bytes(key[0..8].try_into().unwrap());
        let edge_key = key[8..30].to_vec();

        let label = if cached.map(|(v, _)| v) == Some(lookup_id) {
            cached.unwrap().1
        } else {
            loop {
                match cur {
                    None => {
                        return Err(StoreError::SchemaViolation(format!(
                            "edge references vertex {lookup_id} not in vertex set"
                        )));
                    }
                    Some((vid, _)) if vid < lookup_id => {
                        cur = label_iter.next().transpose()?;
                    }
                    Some((vid, lid)) if vid == lookup_id => {
                        cached = Some((vid, lid));
                        break lid;
                    }
                    Some((vid, _)) => {
                        return Err(StoreError::SchemaViolation(format!(
                            "edge references vertex {lookup_id} not in vertex set (next in file: {vid})"
                        )));
                    }
                }
            }
        };

        out_sorter.push(edge_key, kv_codec::EdgeValue { end_vertex_label: label, property_blob: props }.encode())?;
    }
    Ok(())
}

// ── write_degree_sst ───────────────────────────────────────────────────────────

fn write_degree_sst<I1, I2>(
    label_file: &SortedLabelFile,
    out_deg_iter: I1,
    in_deg_iter: I2,
    work_dir: &Path,
    max_sst_size: usize,
    cf_opts: &Options,
) -> Result<Vec<PathBuf>, StoreError>
where
    I1: Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>>,
    I2: Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>>,
{
    if label_file.count == 0 {
        return Ok(Vec::new());
    }
    let mut out_ctr = DegreeCounter::new(out_deg_iter)?;
    let mut in_ctr = DegreeCounter::new(in_deg_iter)?;

    let mut files = Vec::new();
    let mut chunk = 0usize;
    let mut path = work_dir.join(format!("bulk_vertex_degree_{chunk}.sst"));
    let mut writer = SstFileWriter::create(cf_opts);
    writer.open(&path).map_err(StoreError::RocksDb)?;
    let mut written = 0usize;

    for label_item in label_file.reader()? {
        let (vid, lid) = label_item?;
        let out_cnt = out_ctr.count_for(vid)?;
        let in_cnt = in_ctr.count_for(vid)?;
        let key = kv_codec::encode_vertex_key(vid);
        let val = kv_codec::VertexDegree { vertex_label_id: lid, out_e_cnt: out_cnt, in_e_cnt: in_cnt }.encode();
        if writer.file_size() >= max_sst_size as u64 {
            writer.finish().map_err(StoreError::RocksDb)?;
            files.push(path);
            chunk += 1;
            path = work_dir.join(format!("bulk_vertex_degree_{chunk}.sst"));
            writer = SstFileWriter::create(cf_opts);
            writer.open(&path).map_err(StoreError::RocksDb)?;
        }
        writer.put(key, val).map_err(StoreError::RocksDb)?;
        written += 1;
    }
    if written > 0 {
        writer.finish().map_err(StoreError::RocksDb)?;
        files.push(path);
    }
    Ok(files)
}

// ── Phase 2: sort + SST write ──────────────────────────────────────────────────

struct SstPaths {
    vertices: Vec<PathBuf>,
    degree: Vec<PathBuf>,
    edges_out: Vec<PathBuf>,
    edges_in: Vec<PathBuf>,
}

impl SstPaths {
    fn total_files(&self) -> usize {
        self.vertices.len() + self.degree.len() + self.edges_out.len() + self.edges_in.len()
    }
}

#[allow(clippy::type_complexity)]
fn write_sst_from_iter(
    cf_name: &str,
    iter: &mut dyn Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>>,
    work_dir: &Path,
    max_sst_size: usize,
    cf_opts: &Options,
) -> Result<Vec<PathBuf>, StoreError> {
    let mut files = Vec::new();
    let mut chunk = 0usize;
    // Open the SST writer lazily so an empty iterator leaves no file on disk.
    let mut writer: Option<SstFileWriter> = None;
    let mut current_path: Option<PathBuf> = None;

    for result in iter {
        let (key, val) = result?;

        // Roll over to a new file if the current one has reached the size limit.
        if let Some(w) = writer.as_ref() {
            if w.file_size() >= max_sst_size as u64 {
                writer.take().unwrap().finish().map_err(StoreError::RocksDb)?;
                files.push(current_path.take().unwrap());
                chunk += 1;
            }
        }

        // Open a file on demand (first item overall, or after a rollover).
        if writer.is_none() {
            let path = work_dir.join(format!("bulk_{cf_name}_{chunk}.sst"));
            let w = SstFileWriter::create(cf_opts);
            w.open(&path).map_err(StoreError::RocksDb)?;
            current_path = Some(path);
            writer = Some(w);
        }

        writer.as_mut().unwrap().put(key, val).map_err(StoreError::RocksDb)?;
    }

    if let Some(mut w) = writer {
        w.finish().map_err(StoreError::RocksDb)?;
        files.push(current_path.unwrap());
    }
    Ok(files)
}

fn write_sst_from_iter_dedup(
    cf_name: &str,
    iter: impl Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>>,
    work_dir: &Path,
    max_sst_size: usize,
    cf_opts: &Options,
) -> Result<Vec<PathBuf>, StoreError> {
    let mut last_key: Option<Vec<u8>> = None;
    let mut deduped = iter.map(move |r| {
        let (key, val) = r?;
        if last_key.as_deref() == Some(&key[..]) {
            let cek = kv_codec::decode_edge_key(&key, crate::types::keys::Direction::OUT)
                .map(|ek| CanonicalEdgeKey {
                    src_id: ek.primary_id,
                    label_id: ek.label_id,
                    dst_id: ek.secondary_id,
                    rank: ek.rank,
                })
                .ok_or(StoreError::CorruptData("duplicate edge key could not be decoded"))?;
            return Err(StoreError::DuplicateEdge(cek));
        }
        last_key = Some(key.clone());
        Ok((key, val))
    });
    write_sst_from_iter(cf_name, &mut deduped, work_dir, max_sst_size, cf_opts)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{schema::definition::EdgeMode, Graph, TraversalBuilder, Value};
    use tempfile::tempdir;

    fn small_schema() -> BulkSchema {
        BulkSchema { vertex_labels: vec!["Person".into()], edge_labels: vec!["Knows".into()], prop_keys: vec![] }
    }

    fn small_vertices() -> Vec<BulkVertex> {
        (1..=5).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect()
    }

    fn small_edges() -> Vec<BulkEdge> {
        vec![
            BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None },
            BulkEdge { src: 1, dst: 3, label: "Knows".into(), props: HashMap::new(), rank: None },
            BulkEdge { src: 2, dst: 3, label: "Knows".into(), props: HashMap::new(), rank: None },
            BulkEdge { src: 3, dst: 4, label: "Knows".into(), props: HashMap::new(), rank: None },
            BulkEdge { src: 4, dst: 5, label: "Knows".into(), props: HashMap::new(), rank: None },
            BulkEdge { src: 5, dst: 1, label: "Knows".into(), props: HashMap::new(), rank: None },
        ]
    }

    #[test]
    fn test_bulk_loader_fluent_api() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db");
        let graph = Graph::open(&db_path).unwrap();

        let mut loader = graph.open_bulk_loader().unwrap();
        loader.load_vertices(small_vertices()).unwrap();
        loader.load_edges(small_edges()).unwrap();
        let stats = loader.commit().unwrap();

        assert_eq!(stats.vertices_written, 5);
        assert_eq!(stats.edges_written, 6);
        assert!(stats.sst_files >= 4);

        let mut snap = graph.read();
        let v_count = snap.g().V([]).count().next().unwrap().unwrap();
        assert_eq!(v_count, Value::Int64(5));

        let out_count = snap.g().V([1_i64]).out(["Knows"]).count().next().unwrap().unwrap();
        assert_eq!(out_count, Value::Int64(2));
        graph.close().unwrap();
    }

    #[test]
    fn test_bulk_loader_auto_schema_discovery() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db");
        let graph = Graph::open(&db_path).unwrap();

        let vertices = vec![
            BulkVertex {
                id: 1,
                label: "User".into(),
                props: [("name".into(), Primitive::String("Alice".into()))].into(),
            },
            BulkVertex {
                id: 2,
                label: "User".into(),
                props: [("name".into(), Primitive::String("Bob".into()))].into(),
            },
        ];
        let edges = vec![BulkEdge {
            src: 1,
            dst: 2,
            label: "Follows".into(),
            props: [("since".into(), Primitive::Int64(2026))].into(),
            rank: None,
        }];

        let mut loader = graph.open_bulk_loader().unwrap();
        loader.load_vertices(vertices).unwrap();
        loader.load_edges(edges).unwrap();
        loader.commit().unwrap();

        let mut snap = graph.read();
        assert_eq!(snap.g().V([1_i64]).values(["name"]).next().unwrap().unwrap(), Value::String("Alice".into()));
        assert_eq!(
            snap.g().V([1_i64]).outE(["Follows"]).values(["since"]).next().unwrap().unwrap(),
            Value::Int64(2026)
        );
        graph.close().unwrap();
    }

    #[test]
    #[allow(deprecated)]
    fn test_load_initial_small() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db");
        let loader = SstBulkLoader::new(&db_path, dir.path().join("_bulk_work"));
        let stats = loader
            .load_initial(
                small_schema(),
                small_vertices().into_iter(),
                small_edges().into_iter(),
                GraphOptions::default(),
                &RocksOptions::default(),
            )
            .unwrap();
        assert_eq!(stats.vertices_written, 5);
        assert_eq!(stats.edges_written, 6);
        assert!(stats.sst_files >= 4);

        let graph = Graph::open(&db_path).unwrap();
        let mut snap = graph.read();
        let v_count = snap.g().V([]).count().next().unwrap().unwrap();
        assert_eq!(v_count, Value::Int64(5));
        let out_count = snap.g().V([1_i64]).out(["Knows"]).count().next().unwrap().unwrap();
        assert_eq!(out_count, Value::Int64(2));
        graph.close().unwrap();
    }

    #[test]
    #[allow(deprecated)]
    fn test_duplicate_edge_single_mode() {
        let dir = tempdir().unwrap();
        let vertices: Vec<BulkVertex> =
            (1..=3).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
        let edges = vec![
            BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None },
            BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None },
        ];
        let err = SstBulkLoader::new(dir.path().join("db"), dir.path().join("_bulk_work"))
            .load_initial(
                small_schema(),
                vertices.into_iter(),
                edges.into_iter(),
                GraphOptions { edge_mode: EdgeMode::Single, ..Default::default() },
                &RocksOptions::default(),
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::DuplicateEdge(_)));
    }

    #[test]
    #[allow(deprecated)]
    fn test_strict_mode_unknown_label() {
        let dir = tempdir().unwrap();
        let vertices = vec![BulkVertex { id: 1, label: "Unknown".into(), props: HashMap::new() }];
        let err = SstBulkLoader::new(dir.path().join("db"), dir.path().join("_bulk_work"))
            .load_initial(
                small_schema(),
                vertices.into_iter(),
                std::iter::empty(),
                GraphOptions { mode: SchemaMode::Strict, ..Default::default() },
                &RocksOptions::default(),
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::SchemaViolation(_)));
    }

    #[test]
    #[allow(deprecated)]
    fn test_crash_marker_detection() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db");

        SstBulkLoader::new(&db_path, dir.path().join("_bulk_work"))
            .load_initial(
                small_schema(),
                small_vertices().into_iter(),
                small_edges().into_iter(),
                GraphOptions::default(),
                &RocksOptions::default(),
            )
            .unwrap();

        {
            use crate::store::rocks::cf_options;
            use crate::store::rocks::{CF_EDGES_IN, CF_EDGES_OUT, CF_SCHEMA, CF_VERTEX_DEGREE, CF_VERTICES};
            use rocksdb::{
                ColumnFamilyDescriptor, MultiThreaded, OptimisticTransactionDB, Options, WriteBatchWithTransaction,
            };
            let rocks_opts = RocksOptions::default();
            let v_bo = cf_options::vertex_block_opts(&rocks_opts);
            let e_bo = cf_options::edge_block_opts(&rocks_opts);
            let mut dbo = Options::default();
            dbo.create_if_missing(false);
            let cfs = vec![
                ColumnFamilyDescriptor::new(CF_VERTICES, cf_options::vertex_cf_opts(&rocks_opts, &v_bo)),
                ColumnFamilyDescriptor::new(CF_VERTEX_DEGREE, cf_options::vertex_cf_opts(&rocks_opts, &v_bo)),
                ColumnFamilyDescriptor::new(CF_EDGES_OUT, cf_options::edge_cf_opts(&rocks_opts, &e_bo)),
                ColumnFamilyDescriptor::new(CF_EDGES_IN, cf_options::edge_cf_opts(&rocks_opts, &e_bo)),
                ColumnFamilyDescriptor::new(CF_SCHEMA, Options::default()),
            ];
            let db: OptimisticTransactionDB<MultiThreaded> =
                OptimisticTransactionDB::open_cf_descriptors(&dbo, &db_path, cfs).unwrap();
            let cf = db.cf_handle(CF_SCHEMA).unwrap();
            let mut batch = WriteBatchWithTransaction::<true>::default();
            batch.put_cf(&cf, BULK_LOAD_IN_PROGRESS_KEY, [MARKER_POST_INGEST]);
            db.write(batch).unwrap();
        }

        let graph = Graph::open(&db_path).unwrap();
        let mut snap = graph.read();
        let v_count = snap.g().V([]).count().next().unwrap().unwrap();
        assert_eq!(v_count, Value::Int64(5));
        graph.close().unwrap();
    }

    #[test]
    #[allow(deprecated)]
    fn test_multiple_edges_multi_mode_auto_rank() {
        let dir = tempdir().unwrap();
        let vertices: Vec<BulkVertex> =
            (1..=2).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
        let edges = vec![
            BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None },
            BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None },
        ];
        let stats = SstBulkLoader::new(dir.path().join("db"), dir.path().join("_bulk_work"))
            .load_initial(
                small_schema(),
                vertices.into_iter(),
                edges.into_iter(),
                GraphOptions { edge_mode: EdgeMode::Multi, ..Default::default() },
                &RocksOptions::default(),
            )
            .unwrap();
        assert_eq!(stats.edges_written, 2);

        let graph = Graph::open(dir.path().join("db")).unwrap();
        let mut snap = graph.read();
        let edges_count = snap.g().V([1_i64]).outE(["Knows"]).count().next().unwrap().unwrap();
        assert_eq!(edges_count, Value::Int64(2));
        graph.close().unwrap();
    }

    #[test]
    #[allow(deprecated)]
    fn test_edge_referencing_unknown_vertex() {
        let dir = tempdir().unwrap();
        let vertices = vec![BulkVertex { id: 1, label: "Person".into(), props: HashMap::new() }];
        let edges = vec![BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None }];
        let err = SstBulkLoader::new(dir.path().join("db"), dir.path().join("_bulk_work"))
            .load_initial(
                small_schema(),
                vertices.into_iter(),
                edges.into_iter(),
                GraphOptions::default(),
                &RocksOptions::default(),
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::SchemaViolation(_)));
    }

    #[test]
    #[allow(deprecated)]
    fn test_load_initial_external_sort() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db");
        let loader = SstBulkLoader::new(&db_path, dir.path().join("_bulk_work")).with_max_memory(1);
        let stats = loader
            .load_initial(
                small_schema(),
                small_vertices().into_iter(),
                small_edges().into_iter(),
                GraphOptions::default(),
                &RocksOptions::default(),
            )
            .unwrap();
        assert_eq!(stats.vertices_written, 5);
        assert_eq!(stats.edges_written, 6);

        let graph = Graph::open(&db_path).unwrap();
        let mut snap = graph.read();
        let v_count = snap.g().V([]).count().next().unwrap().unwrap();
        assert_eq!(v_count, Value::Int64(5));
        let out_count = snap.g().V([1_i64]).out(["Knows"]).count().next().unwrap().unwrap();
        assert_eq!(out_count, Value::Int64(2));
        graph.close().unwrap();
    }

    #[test]
    fn test_dedup_iter_returns_correct_duplicate_edge() {
        use crate::{
            store::rocks::cf_options,
            types::{keys::CanonicalEdgeKey, kv_codec},
        };

        let dir = tempdir().unwrap();
        let rocks_opts = RocksOptions::default();
        let e_bo = cf_options::edge_block_opts(&rocks_opts);
        let e_opts = cf_options::edge_cf_opts(&rocks_opts, &e_bo);

        let cek = CanonicalEdgeKey { src_id: 10, label_id: 1, dst_id: 20, rank: 0 };
        let key = kv_codec::encode_edge_key(&cek.out_key()).to_vec();
        let val = kv_codec::EdgeValue { end_vertex_label: 1, property_blob: vec![] }.encode();

        let pairs = vec![Ok((key.clone(), val.clone())), Ok((key.clone(), val.clone()))];

        let err = write_sst_from_iter_dedup("test_edges", pairs.into_iter(), dir.path(), 64 * 1024 * 1024, &e_opts)
            .unwrap_err();

        match err {
            StoreError::DuplicateEdge(detected) => {
                assert_eq!(detected.src_id, 10);
                assert_eq!(detected.label_id, 1);
                assert_eq!(detected.dst_id, 20);
                assert_eq!(detected.rank, 0);
            }
            other => panic!("expected DuplicateEdge, got: {other}"),
        }
    }

    #[test]
    fn test_sorted_label_file_basic_and_dedup() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("labels.bin");
        let mut sorter = ExternalSorter::new(dir.path().join("sort"), 1024 * 1024);

        sorter.push(20i64.to_be_bytes().to_vec(), 2i32.to_be_bytes().to_vec()).unwrap();
        sorter.push(10i64.to_be_bytes().to_vec(), 1i32.to_be_bytes().to_vec()).unwrap();
        sorter.push(10i64.to_be_bytes().to_vec(), 1i32.to_be_bytes().to_vec()).unwrap();
        sorter.push(30i64.to_be_bytes().to_vec(), 3i32.to_be_bytes().to_vec()).unwrap();

        let file = SortedLabelFile::write_from(sorter, &path).unwrap();
        assert_eq!(file.count, 3);

        let reader: Vec<_> = file.reader().unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(reader, vec![(10, 1), (20, 2), (30, 3)]);
    }

    #[test]
    fn test_sorted_label_file_conflicting_labels() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("labels.bin");
        let mut sorter = ExternalSorter::new(dir.path().join("sort"), 1024 * 1024);

        sorter.push(10i64.to_be_bytes().to_vec(), 1i32.to_be_bytes().to_vec()).unwrap();
        sorter.push(10i64.to_be_bytes().to_vec(), 2i32.to_be_bytes().to_vec()).unwrap();

        let err = SortedLabelFile::write_from(sorter, &path).unwrap_err();
        assert!(matches!(err, StoreError::SchemaViolation(_)));
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn test_degree_counter() {
        let items: Vec<Result<(Vec<u8>, Vec<u8>), StoreError>> = vec![
            Ok((10i64.to_be_bytes().to_vec(), vec![])),
            Ok((10i64.to_be_bytes().to_vec(), vec![])),
            Ok((10i64.to_be_bytes().to_vec(), vec![])),
            Ok((20i64.to_be_bytes().to_vec(), vec![])),
            Ok((30i64.to_be_bytes().to_vec(), vec![])),
            Ok((30i64.to_be_bytes().to_vec(), vec![])),
        ];

        let mut counter = DegreeCounter::new(items.into_iter()).unwrap();
        assert_eq!(counter.count_for(10).unwrap(), 3);
        assert_eq!(counter.count_for(15).unwrap(), 0);
        assert_eq!(counter.count_for(20).unwrap(), 1);
        assert_eq!(counter.count_for(30).unwrap(), 2);
        assert_eq!(counter.count_for(40).unwrap(), 0);
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn test_annotate_edges_mismatched_vertex() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("labels.bin");
        let mut sorter = ExternalSorter::new(dir.path().join("sort"), 1024 * 1024);
        sorter.push(10i64.to_be_bytes().to_vec(), 1i32.to_be_bytes().to_vec()).unwrap();
        let file = SortedLabelFile::write_from(sorter, &path).unwrap();

        let annot_item: Vec<Result<(Vec<u8>, Vec<u8>), StoreError>> = vec![Ok((
            {
                let mut k = vec![0u8; 30];
                k[0..8].copy_from_slice(&20i64.to_be_bytes());
                k
            },
            vec![],
        ))];

        let mut out_sorter = ExternalSorter::new(dir.path().join("out"), 1024 * 1024);
        let err = annotate_edges(annot_item.into_iter(), &file, &mut out_sorter).unwrap_err();
        assert!(matches!(err, StoreError::SchemaViolation(_)));
    }

    #[test]
    #[allow(deprecated)]
    fn test_empty_input() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db");
        let stats = SstBulkLoader::new(&db_path, dir.path().join("_w"))
            .load_initial(
                small_schema(),
                std::iter::empty(),
                std::iter::empty(),
                GraphOptions::default(),
                &RocksOptions::default(),
            )
            .unwrap();
        assert_eq!(stats.vertices_written, 0);
        assert_eq!(stats.edges_written, 0);
        let graph = Graph::open(&db_path).unwrap();
        let mut snap = graph.read();
        assert_eq!(snap.g().V([]).count().next().unwrap().unwrap(), Value::Int64(0));
        graph.close().unwrap();
    }

    #[test]
    #[allow(deprecated)]
    fn test_vertices_only_no_edges() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db");
        let vertices: Vec<BulkVertex> =
            (1..=3).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
        let stats = SstBulkLoader::new(&db_path, dir.path().join("_w"))
            .load_initial(
                small_schema(),
                vertices.into_iter(),
                std::iter::empty(),
                GraphOptions::default(),
                &RocksOptions::default(),
            )
            .unwrap();
        assert_eq!(stats.vertices_written, 3);
        assert_eq!(stats.edges_written, 0);
        let graph = Graph::open(&db_path).unwrap();
        let mut snap = graph.read();
        assert_eq!(snap.g().V([]).count().next().unwrap().unwrap(), Value::Int64(3));
        assert_eq!(snap.g().V([1_i64]).out([]).count().next().unwrap().unwrap(), Value::Int64(0));
        graph.close().unwrap();
    }

    #[test]
    #[allow(deprecated)]
    fn test_properties_roundtrip() {
        use crate::{schema::DataType, Primitive};
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db");
        let schema = BulkSchema {
            vertex_labels: vec!["Person".into()],
            edge_labels: vec!["Knows".into()],
            prop_keys: vec![("age".into(), DataType::Int64), ("score".into(), DataType::Int64)],
        };
        let vertices = vec![
            BulkVertex { id: 1, label: "Person".into(), props: [("age".into(), Primitive::Int64(42))].into() },
            BulkVertex { id: 2, label: "Person".into(), props: HashMap::new() },
        ];
        let edges = vec![BulkEdge {
            src: 1,
            dst: 2,
            label: "Knows".into(),
            props: [("score".into(), Primitive::Int64(100))].into(),
            rank: None,
        }];
        SstBulkLoader::new(&db_path, dir.path().join("_w"))
            .load_initial(
                schema,
                vertices.into_iter(),
                edges.into_iter(),
                GraphOptions::default(),
                &RocksOptions::default(),
            )
            .unwrap();
        let graph = Graph::open(&db_path).unwrap();
        let mut snap = graph.read();
        assert_eq!(snap.g().V([1_i64]).values(["age"]).next().unwrap().unwrap(), Value::Int64(42));
        assert_eq!(snap.g().V([1_i64]).outE(["Knows"]).values(["score"]).next().unwrap().unwrap(), Value::Int64(100));
        graph.close().unwrap();
    }

    #[test]
    #[allow(deprecated)]
    fn test_multi_mode_explicit_ranks() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db");
        let vertices: Vec<BulkVertex> =
            (1..=2).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
        let edges = vec![
            BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: Some(5) },
            BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: Some(10) },
        ];
        let stats = SstBulkLoader::new(&db_path, dir.path().join("_w"))
            .load_initial(
                small_schema(),
                vertices.into_iter(),
                edges.into_iter(),
                GraphOptions { edge_mode: EdgeMode::Multi, ..Default::default() },
                &RocksOptions::default(),
            )
            .unwrap();
        assert_eq!(stats.edges_written, 2);
        let graph = Graph::open(&db_path).unwrap();
        let mut snap = graph.read();
        assert_eq!(snap.g().V([1_i64]).outE(["Knows"]).count().next().unwrap().unwrap(), Value::Int64(2));
        graph.close().unwrap();
    }

    #[test]
    #[allow(deprecated)]
    fn test_multi_mode_explicit_rank_duplicate() {
        let dir = tempdir().unwrap();
        let vertices: Vec<BulkVertex> =
            (1..=2).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
        let edges = vec![
            BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: Some(3) },
            BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: Some(3) },
        ];
        let err = SstBulkLoader::new(dir.path().join("db"), dir.path().join("_w"))
            .load_initial(
                small_schema(),
                vertices.into_iter(),
                edges.into_iter(),
                GraphOptions { edge_mode: EdgeMode::Multi, ..Default::default() },
                &RocksOptions::default(),
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::DuplicateEdge(_)));
    }

    #[test]
    #[allow(deprecated)]
    fn test_multi_mode_rank_overflow() {
        let dir = tempdir().unwrap();
        let vertices: Vec<BulkVertex> =
            (1..=2).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
        let edges = vec![
            BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: Some(65534) },
            BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None },
        ];
        let err = SstBulkLoader::new(dir.path().join("db"), dir.path().join("_w"))
            .load_initial(
                small_schema(),
                vertices.into_iter(),
                edges.into_iter(),
                GraphOptions { edge_mode: EdgeMode::Multi, ..Default::default() },
                &RocksOptions::default(),
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::SchemaViolation(_)));
    }

    #[test]
    #[allow(deprecated)]
    fn test_sst_file_splitting() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db");
        let stats = SstBulkLoader::new(&db_path, dir.path().join("_w"))
            .with_max_sst_size(1)
            .load_initial(
                small_schema(),
                small_vertices().into_iter(),
                small_edges().into_iter(),
                GraphOptions::default(),
                &RocksOptions::default(),
            )
            .unwrap();
        assert!(stats.sst_files >= 4, "expected at least one file per CF, got {}", stats.sst_files);
        let graph = Graph::open(&db_path).unwrap();
        let mut snap = graph.read();
        assert_eq!(snap.g().V([]).count().next().unwrap().unwrap(), Value::Int64(5));
        assert_eq!(snap.g().V([1_i64]).out(["Knows"]).count().next().unwrap().unwrap(), Value::Int64(2));
        graph.close().unwrap();
    }

    #[test]
    #[allow(deprecated)]
    fn test_work_dir_cleaned_up_on_error() {
        let dir = tempdir().unwrap();
        let work_dir = dir.path().join("_w");
        let vertices: Vec<BulkVertex> =
            (1..=2).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
        let edges = vec![BulkEdge { src: 1, dst: 2, label: "Unknown".into(), props: HashMap::new(), rank: None }];
        let err = SstBulkLoader::new(dir.path().join("db"), work_dir.clone())
            .load_initial(
                small_schema(),
                vertices.into_iter(),
                edges.into_iter(),
                GraphOptions { mode: SchemaMode::Strict, ..Default::default() },
                &RocksOptions::default(),
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::SchemaViolation(_)));
        assert!(!work_dir.exists(), "WorkDirGuard should have removed work_dir on error");
    }

    #[test]
    fn test_empty_sorted_label_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("labels.bin");
        let sorter = ExternalSorter::new(dir.path().join("sort"), 1024 * 1024);
        let file = SortedLabelFile::write_from(sorter, &path).unwrap();
        assert_eq!(file.count, 0);
        let items: Vec<_> = file.reader().unwrap().collect();
        assert!(items.is_empty());
    }

    #[test]
    #[allow(deprecated)]
    fn test_strict_mode_undeclared_edge_property() {
        use crate::{schema::DataType, Primitive};
        let dir = tempdir().unwrap();
        let schema = BulkSchema {
            vertex_labels: vec!["Person".into()],
            edge_labels: vec!["Knows".into()],
            prop_keys: vec![("age".into(), DataType::Int64)],
        };
        let vertices: Vec<BulkVertex> =
            (1..=2).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
        let edges = vec![BulkEdge {
            src: 1,
            dst: 2,
            label: "Knows".into(),
            props: [("weight".into(), Primitive::Float64(1.5))].into(),
            rank: None,
        }];
        let err = SstBulkLoader::new(dir.path().join("db"), dir.path().join("_w"))
            .load_initial(
                schema,
                vertices.into_iter(),
                edges.into_iter(),
                GraphOptions { mode: SchemaMode::Strict, ..Default::default() },
                &RocksOptions::default(),
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::SchemaViolation(_)));
    }

    #[test]
    fn test_bulk_loader_ordering_enforcement() {
        let dir = tempdir().unwrap();
        let graph = Graph::open(dir.path()).unwrap();
        let mut loader = graph.open_bulk_loader().unwrap();

        // 1. Calling load_edges before load_vertices -> VerticesNotLoaded
        let edges = vec![BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None }];
        assert!(matches!(loader.load_edges(edges), Err(StoreError::VerticesNotLoaded)));

        // 2. Calling commit before load_vertices -> VerticesNotLoaded
        drop(loader); // release AtomicBool lock
        let empty_loader = graph.open_bulk_loader().unwrap();
        assert!(matches!(empty_loader.commit(), Err(StoreError::VerticesNotLoaded)));

        // Re-open for tests 3-4
        let mut loader = graph.open_bulk_loader().unwrap();

        // 3. Calling load_vertices twice -> UnsupportedOperation
        let vertices = vec![BulkVertex { id: 1, label: "Person".into(), props: HashMap::new() }];
        loader.load_vertices(vertices.clone()).unwrap();
        assert!(matches!(loader.load_vertices(vertices), Err(StoreError::UnsupportedOperation(_))));

        // 4. Calling load_edges twice -> UnsupportedOperation
        let edges = vec![BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None }];
        loader.load_edges(edges.clone()).unwrap();
        assert!(matches!(loader.load_edges(edges), Err(StoreError::UnsupportedOperation(_))));
    }

    #[test]
    fn test_bulk_loader_commit_without_edges() {
        let dir = tempdir().unwrap();
        let graph = Graph::open(dir.path().join("db")).unwrap();
        let mut loader = graph.open_bulk_loader().unwrap();

        let vertices = vec![
            BulkVertex { id: 10, label: "Node".into(), props: HashMap::new() },
            BulkVertex { id: 20, label: "Node".into(), props: HashMap::new() },
        ];

        loader.load_vertices(vertices).unwrap();
        // Skip load_edges entirely
        let stats = loader.commit().unwrap();

        assert_eq!(stats.vertices_written, 2);
        assert_eq!(stats.edges_written, 0);

        let mut snap = graph.read();
        assert_eq!(snap.g().V([]).count().next().unwrap().unwrap(), Value::Int64(2));
        assert_eq!(snap.g().V([10_i64]).out([]).count().next().unwrap().unwrap(), Value::Int64(0));
    }

    #[test]
    fn test_bulk_loader_auto_schema_sync_back() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db");
        let graph = Graph::open(&db_path).unwrap();

        let vertices = vec![BulkVertex {
            id: 1,
            label: "Account".into(),
            props: [("balance".into(), Primitive::Float64(1250.50))].into(),
        }];
        let edges = vec![BulkEdge {
            src: 1,
            dst: 1,
            label: "TransfersTo".into(),
            props: [("amount".into(), Primitive::Float64(50.0))].into(),
            rank: None,
        }];

        let mut loader = graph.open_bulk_loader().unwrap();
        loader.load_vertices(vertices).unwrap();
        loader.load_edges(edges).unwrap();
        loader.commit().unwrap();

        // Verify in-memory schema on Graph was updated
        {
            let schema_lock = graph.schema();
            let schema = schema_lock.read().unwrap();
            assert!(schema.vertex_label_id("Account").is_some());
            assert!(schema.edge_label_id("TransfersTo").is_some());
            assert!(schema.prop_key_id("balance").is_some());
            assert!(schema.prop_key_id("amount").is_some());
        }

        // Close and re-open graph to verify schema persisted to CF_SCHEMA
        drop(graph);
        let reopened = Graph::open(&db_path).unwrap();
        let mut snap = reopened.read();
        assert_eq!(snap.g().V([]).hasLabel(["Account"]).count().next().unwrap().unwrap(), Value::Int64(1));
        assert_eq!(snap.g().V([1_i64]).values(["balance"]).next().unwrap().unwrap(), Value::Float64(1250.50));
        assert_eq!(
            snap.g().V([1_i64]).outE(["TransfersTo"]).values(["amount"]).next().unwrap().unwrap(),
            Value::Float64(50.0)
        );
    }

    #[test]
    fn test_bulk_loader_strict_schema_enforcement() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db");
        let graph = Graph::open_with_options(&db_path, GraphOptions { mode: SchemaMode::Strict, ..Default::default() })
            .unwrap();

        // Declare schema beforehand
        {
            let mut s = graph.open_schema();
            s.add_vertex_label("User");
            s.add_edge_label("Follows");
            s.add_property_key("name", DataType::String);
            s.commit().unwrap();
        }

        // 1. Undeclared vertex label -> SchemaViolation
        {
            let mut loader = graph.open_bulk_loader().unwrap();
            let res =
                loader.load_vertices(vec![BulkVertex { id: 1, label: "UnknownLabel".into(), props: HashMap::new() }]);
            assert!(matches!(res, Err(StoreError::SchemaViolation(_))));
        }

        // 2. Undeclared edge label -> SchemaViolation
        {
            let mut loader = graph.open_bulk_loader().unwrap();
            loader
                .load_vertices(vec![
                    BulkVertex { id: 1, label: "User".into(), props: HashMap::new() },
                    BulkVertex { id: 2, label: "User".into(), props: HashMap::new() },
                ])
                .unwrap();
            let res = loader.load_edges(vec![BulkEdge {
                src: 1,
                dst: 2,
                label: "UnknownEdge".into(),
                props: HashMap::new(),
                rank: None,
            }]);
            assert!(matches!(res, Err(StoreError::SchemaViolation(_))));
        }

        // 3. Undeclared property key -> SchemaViolation
        {
            let mut loader = graph.open_bulk_loader().unwrap();
            loader
                .load_vertices(vec![
                    BulkVertex { id: 1, label: "User".into(), props: HashMap::new() },
                    BulkVertex { id: 2, label: "User".into(), props: HashMap::new() },
                ])
                .unwrap();
            let res = loader.load_edges(vec![BulkEdge {
                src: 1,
                dst: 2,
                label: "Follows".into(),
                props: [("undeclared_prop".into(), Primitive::Int64(1))].into(),
                rank: None,
            }]);
            assert!(matches!(res, Err(StoreError::SchemaViolation(_))));
        }
    }

    #[test]
    fn test_bulk_loader_drop_and_custom_work_dir_cleanup() {
        let dir = tempdir().unwrap();
        let graph = Graph::open(dir.path().join("db")).unwrap();
        let custom_work_dir = dir.path().join("my_custom_scratch");

        {
            let mut loader = graph
                .open_bulk_loader()
                .unwrap()
                .with_work_dir(&custom_work_dir)
                .with_max_memory(1024 * 1024)
                .with_max_sst_size(1024 * 1024)
                .with_rocks_options(RocksOptions::default());

            loader.load_vertices(vec![BulkVertex { id: 1, label: "V".into(), props: HashMap::new() }]).unwrap();
            assert!(custom_work_dir.exists());
            // Drop without calling commit()
        }

        // Work directory should be cleaned up by Drop
        assert!(!custom_work_dir.exists(), "work directory should be cleaned up when BulkLoader is dropped");
    }

    #[test]
    fn test_bulk_loader_iterator_workflow() {
        let dir = tempdir().unwrap();
        let graph = Graph::open(dir.path().join("db")).unwrap();
        let mut loader = graph.open_bulk_loader().unwrap();

        let vertices = vec![
            BulkVertex { id: 1, label: "Person".into(), props: HashMap::new() },
            BulkVertex { id: 2, label: "Person".into(), props: HashMap::new() },
            BulkVertex { id: 3, label: "Person".into(), props: HashMap::new() },
        ];
        let edges = vec![
            BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None },
            BulkEdge { src: 2, dst: 3, label: "Knows".into(), props: HashMap::new(), rank: None },
            BulkEdge { src: 3, dst: 1, label: "Knows".into(), props: HashMap::new(), rank: None },
        ];

        loader.load_vertices(vertices).unwrap();
        loader.load_edges(edges).unwrap();
        let stats = loader.commit().unwrap();

        assert_eq!(stats.vertices_written, 3);
        assert_eq!(stats.edges_written, 3);

        let mut snap = graph.read();
        assert_eq!(snap.g().V([]).count().next().unwrap().unwrap(), Value::Int64(3));
        assert_eq!(snap.g().V([1_i64]).out([]).count().next().unwrap().unwrap(), Value::Int64(1));
    }
}
