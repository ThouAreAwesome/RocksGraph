// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of RocksGraph.
//
// RocksGraph is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 2 of the License, or
// (at your option) any later version.
//
// RocksGraph is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with RocksGraph.  If not, see <https://www.gnu.org/licenses/>.

//! Bulk load via SST ingestion.
//!
//! Provides [`SstBulkLoader`] which streams vertices and edges through
//! [`ExternalSorter`], writes sorted SST files, and ingests them atomically
//! into a new RocksDB database — bypassing WAL, memtable pressure, and OCC
//! overhead entirely.
//!
//! ## Memory model
//!
//! All sorters spill to `work_dir` when their buffer exceeds
//! `max_memory_bytes / N` (N depends on how many sorters are active
//! concurrently in each phase).  Peak RAM is bounded by `max_memory_bytes`
//! regardless of dataset size.  There are no in-memory per-vertex or
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
use std::time::Instant;

use rocksdb::{
    ColumnFamilyDescriptor, IngestExternalFileOptions, OptimisticTransactionDB, Options, SstFileWriter,
    WriteBatchWithTransaction,
};

use crate::{
    schema::{
        definition::{EdgeMode, SchemaMode},
        DataType, GraphOptions,
    },
    store::rocks::cf_options,
    types::{
        gvalue::Primitive,
        keys::{CanonicalEdgeKey, LabelId, Rank, VertexKey},
        kv_codec::{
            self, encode_schema_key, encode_schema_label_value, encode_schema_meta, encode_schema_prop_value,
            SCHEMA_KIND_EDGE_LABEL, SCHEMA_KIND_PROP_KEY, SCHEMA_KIND_VERTEX_LABEL, SCHEMA_META_KEY,
        },
        prop_codec, StoreError,
    },
};

use super::{bulk_sort::ExternalSorter, CF_EDGES_IN, CF_EDGES_OUT, CF_SCHEMA, CF_VERTEX_DEGREE, CF_VERTICES};

// ── Constants ──────────────────────────────────────────────────────────────────

pub(crate) const BULK_LOAD_IN_PROGRESS_KEY: &[u8] = b"_bulk_load_in_progress";

// Default SST split threshold: 90% of RocksDB's default target_file_size_base (64 MiB).
const DEFAULT_MAX_SST_SIZE: usize = 58 * 1024 * 1024;
const DEFAULT_MAX_MEMORY_BYTES: usize = 512 * 1024 * 1024;

// ── Public types ───────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct BulkSchema {
    pub vertex_labels: Vec<String>,
    pub edge_labels: Vec<String>,
    /// (name, DataType). IDs 1–3 are reserved (id/label/rank); user keys start at 4.
    pub prop_keys: Vec<(String, DataType)>,
}

pub struct BulkVertex {
    pub id: VertexKey,
    pub label: String,
    pub props: HashMap<String, Primitive>,
}

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
}

/// Loads a graph dataset into RocksDB at disk-write speed via SST ingestion.
///
/// Generates sorted SST files offline and ingests them atomically, bypassing
/// the transactional write path entirely (no WAL, no memtable pressure).
///
/// ## Edge mode note
///
/// `EdgeMode::Single` (the default) uses a fully streaming edge path and handles
/// arbitrarily large datasets bounded only by disk space.
///
/// Both `EdgeMode::Single` and `EdgeMode::Multi` stream vertices and edges through
/// `ExternalSorter` — peak memory is bounded by `max_memory_bytes` regardless of
/// dataset size.  There is no per-vertex or per-edge in-memory map.
///
/// **Multi-mode rank semantics**: `BulkEdge::rank = None` auto-assigns ranks in
/// encounter order.  `BulkEdge::rank = Some(r)` uses the explicit rank `r`.
/// Explicit ranks are positioned before auto-ranks within each `(src, label, dst)`
/// group.  `Rank::MAX` (`u16::MAX`, 65535) is reserved as the auto-assign sentinel
/// and must not be used as an explicit rank.
pub struct SstBulkLoader {
    /// Destination directory where the final RocksDB database will be created.
    db_path: PathBuf,
    /// Temporary working directory where spilled sort chunks and SST files are written.
    /// Recommended to be placed on a fast local NVMe SSD.
    work_dir: PathBuf,
    /// Target size in bytes for each individual generated SST file (defaults to 64 MiB).
    max_sst_size: usize,
    /// Total memory budget in bytes allocated for sorting passes (defaults to 512 MiB).
    max_memory_bytes: usize,
}

impl SstBulkLoader {
    /// Creates a new bulk loader instance.
    ///
    /// * `db_path`: The directory where the new graph database will be loaded. Must be empty.
    /// * `work_dir`: The directory for writing temporary spill/SST files (cleaned up automatically on completion).
    pub fn new(db_path: impl Into<PathBuf>, work_dir: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            work_dir: work_dir.into(),
            max_sst_size: DEFAULT_MAX_SST_SIZE,
            max_memory_bytes: DEFAULT_MAX_MEMORY_BYTES,
        }
    }

    /// Sets the target size in bytes for each generated SST file.
    ///
    /// Larger SST files reduce the overall file count but should be sized to align
    /// with RocksDB's block cache and compaction specifications. Defaults to 64 MiB.
    pub fn with_max_sst_size(mut self, bytes: usize) -> Self {
        self.max_sst_size = bytes;
        self
    }

    /// Sets the total memory budget in bytes allocated for sorting passes.
    ///
    /// The loader partitions this memory to buffer edge/vertex data in memory.
    /// Larger budgets (e.g. 1-2 GiB) decrease disk-spill frequency and speed up sorting.
    /// Defaults to 512 MiB.
    pub fn with_max_memory(mut self, bytes: usize) -> Self {
        self.max_memory_bytes = bytes;
        self
    }
}

// ── Internal types ─────────────────────────────────────────────────────────────

struct ResolvedSchema {
    vertex_label_ids: HashMap<String, LabelId>,
    edge_label_ids: HashMap<String, LabelId>,
    prop_key_ids: HashMap<String, u16>,
}

impl ResolvedSchema {
    fn resolve_vertex_label(&self, name: &str) -> Result<LabelId, StoreError> {
        self.vertex_label_ids
            .get(name)
            .copied()
            .ok_or_else(|| StoreError::SchemaViolation(format!("unknown vertex label '{name}'")))
    }

    fn resolve_edge_label(&self, name: &str) -> Result<LabelId, StoreError> {
        self.edge_label_ids
            .get(name)
            .copied()
            .ok_or_else(|| StoreError::SchemaViolation(format!("unknown edge label '{name}'")))
    }

    fn resolve_prop_key(&self, name: &str) -> Result<u16, StoreError> {
        self.prop_key_ids
            .get(name)
            .copied()
            .ok_or_else(|| StoreError::SchemaViolation(format!("unknown property key '{name}'")))
    }

    fn encode_props(&self, props: &HashMap<String, Primitive>) -> Vec<u8> {
        let id_props: HashMap<u16, Primitive> =
            props.iter().filter_map(|(name, val)| self.prop_key_ids.get(name).map(|&id| (id, val.clone()))).collect();
        prop_codec::encode_props(&id_props)
    }
}

// ── Schema resolution + write_schema_cf ───────────────────────────────────────

fn resolve_schema(schema: &BulkSchema) -> ResolvedSchema {
    let vertex_label_ids: HashMap<String, LabelId> =
        schema.vertex_labels.iter().enumerate().map(|(i, name)| (name.clone(), (i + 1) as LabelId)).collect();
    let edge_label_ids: HashMap<String, LabelId> =
        schema.edge_labels.iter().enumerate().map(|(i, name)| (name.clone(), (i + 1) as LabelId)).collect();
    let prop_key_ids: HashMap<String, u16> =
        schema.prop_keys.iter().enumerate().map(|(i, (name, _))| (name.clone(), (i + 4) as u16)).collect();
    ResolvedSchema { vertex_label_ids, edge_label_ids, prop_key_ids }
}

fn write_schema_cf(
    db: &OptimisticTransactionDB,
    schema: &BulkSchema,
    graph_opts: &GraphOptions,
) -> Result<ResolvedSchema, StoreError> {
    let resolved = resolve_schema(schema);
    let cf = db.cf_handle(CF_SCHEMA).ok_or(StoreError::MissingColumnFamily(CF_SCHEMA))?;
    let mut batch = WriteBatchWithTransaction::<true>::default();

    // Meta + crash marker in the same atomic WriteBatch.
    let meta = encode_schema_meta(1, graph_opts.edge_mode.to_u8(), graph_opts.mode.to_u8());
    batch.put_cf(&cf, SCHEMA_META_KEY, meta);
    batch.put_cf(&cf, BULK_LOAD_IN_PROGRESS_KEY, [1u8]);

    for (name, &id) in &resolved.vertex_label_ids {
        batch.put_cf(&cf, encode_schema_key(SCHEMA_KIND_VERTEX_LABEL, name), encode_schema_label_value(id));
    }
    for (name, &id) in &resolved.edge_label_ids {
        batch.put_cf(&cf, encode_schema_key(SCHEMA_KIND_EDGE_LABEL, name), encode_schema_label_value(id));
    }

    // Reserved prop keys (id/label/rank) + user keys.
    // enumerate() over the BulkSchema slice guarantees id == index+4 matches
    // what resolve_schema() assigned — HashMap::values() must NOT be used here.
    let reserved: [(&str, u16, DataType); 3] =
        [("id", 1, DataType::Int64), ("label", 2, DataType::Int32), ("rank", 3, DataType::UInt16)];
    for (name, id, dt) in &reserved {
        batch.put_cf(&cf, encode_schema_key(SCHEMA_KIND_PROP_KEY, name), encode_schema_prop_value(*id, dt.to_u8()));
    }
    for (i, (name, dt)) in schema.prop_keys.iter().enumerate() {
        let id = (i + 4) as u16;
        batch.put_cf(&cf, encode_schema_key(SCHEMA_KIND_PROP_KEY, name), encode_schema_prop_value(id, dt.to_u8()));
    }

    db.write(batch).map_err(StoreError::RocksDb)?;
    Ok(resolved)
}

// ── Progress reporting ─────────────────────────────────────────────────────────

fn bulk_log(start: Instant, msg: &str) {
    eprintln!("[bulk {:>7.1}s] {}", start.elapsed().as_secs_f64(), msg);
}

/// RAII guard that removes `work_dir` on drop unless explicitly disarmed.
/// Ensures temporary SST/chunk files are cleaned up on error paths and panics.
struct WorkDirGuard {
    path: PathBuf,
    active: bool,
}

impl Drop for WorkDirGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

// ── SortedLabelFile ────────────────────────────────────────────────────────────

/// Compact on-disk sorted file of (VertexKey, LabelId) pairs (12 bytes each).
/// Written once from an ExternalSorter with deduplication; readable independently
/// multiple times (two annotation passes + one degree pass). Deleted on drop.
#[derive(Debug)]
struct SortedLabelFile {
    path: PathBuf,
    /// Number of unique (deduplicated) vertex records in the file.
    count: u64,
}

impl SortedLabelFile {
    /// Consume `sorter`, deduplicate adjacent equal VertexKeys (advice A), and write
    /// a flat binary file: `[count: u64 le][(vertex_id: i64 be, label_id: i32 be)...]`.
    ///
    /// Returns `SchemaViolation` if the same VertexKey appears with two different labels.
    fn write_from(sorter: ExternalSorter, path: &Path) -> Result<Self, StoreError> {
        let file = File::create(path).map_err(StoreError::Io)?;
        let mut w = BufWriter::new(file);
        w.write_all(&0u64.to_le_bytes()).map_err(StoreError::Io)?; // placeholder count
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
                    continue; // deduplicate same-key same-label
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
    /// The buffered next vertex_id (already consumed from iter).
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

    /// Count and consume all head entries where vertex_id == `vid`.
    /// Returns 0 if head is already past `vid`.
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

/// Sort-merge join: for each (annot_key, props) from `annot_iter` (sorted by
/// (lookup_id:8, edge_key:22)), look up `lookup_id` in `label_file` to obtain
/// `end_vertex_label`, then push (edge_key, EdgeValue{label, props}) to `out_sorter`.
///
/// Both streams must be sorted by their vertex ID fields.
/// Advice C: if the label file has no entry for a lookup_id → SchemaViolation.
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

        // Cache: consecutive edges often share the same lookup_id (sorted input).
        let label = if cached.map(|(v, _)| v) == Some(lookup_id) {
            cached.unwrap().1
        } else {
            loop {
                match cur {
                    None => {
                        // Advice C: label file exhausted before finding lookup_id.
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
                        // Advice C: vid > lookup_id → vertex missing from file.
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

/// Three-way merge of `label_file`, `out_deg_iter`, and `in_deg_iter` (all sorted
/// by vertex_id) to produce `VertexDegree` records.  Writes directly to
/// `SstFileWriter` in O(1) memory — no intermediate Vec collection.
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

    // label_file is sorted by vertex_id → records emerge in ascending key order.
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
    let mut path = work_dir.join(format!("bulk_{cf_name}_{chunk}.sst"));
    let mut writer = SstFileWriter::create(cf_opts);
    writer.open(&path).map_err(StoreError::RocksDb)?;
    let mut count = 0usize;
    #[allow(clippy::while_let_loop)]
    loop {
        let result = match iter.next() {
            Some(r) => r,
            None => break,
        };
        let (key, val) = result?;
        count += 1;
        if writer.file_size() >= max_sst_size as u64 {
            writer.finish().map_err(StoreError::RocksDb)?;
            files.push(path);
            chunk += 1;
            path = work_dir.join(format!("bulk_{cf_name}_{chunk}.sst"));
            writer = SstFileWriter::create(cf_opts);
            writer.open(&path).map_err(StoreError::RocksDb)?;
        }
        writer.put(key, val).map_err(StoreError::RocksDb)?;
    }
    if count > 0 {
        writer.finish().map_err(StoreError::RocksDb)?;
        files.push(path);
    }
    Ok(files)
}

/// Like `write_sst_from_iter` but returns `DuplicateEdge` if two consecutive
/// records share the same key (used for Single-mode duplicate detection in the
/// streaming path, where duplicates surface during sorted SST write).
///
/// # Correctness
/// The consecutive-key check is sufficient because `ExternalSorter::finish()`
/// produces globally-sorted output via K-way merge: identical edge keys from any
/// chunk file are always adjacent in the merged stream.
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

// ── load_initial ───────────────────────────────────────────────────────────────

impl SstBulkLoader {
    /// Load an initial dataset into an empty database at `db_path`.
    ///
    /// The entire load is atomic: data becomes visible all-at-once via
    /// `IngestExternalFile`, or not at all if the process is killed mid-load.
    pub fn load_initial(
        self,
        schema: BulkSchema,
        vertices: impl Iterator<Item = BulkVertex>,
        edges: impl Iterator<Item = BulkEdge>,
        graph_opts: GraphOptions,
        rocks_opts: &crate::store::rocks::store::RocksOptions,
    ) -> Result<BulkLoadStats, StoreError> {
        // Prepare work_dir — clean up any leftover from a prior attempt.
        let t0 = Instant::now();
        bulk_log(t0, "starting bulk load");

        if self.work_dir.exists() {
            std::fs::remove_dir_all(&self.work_dir).map_err(StoreError::Io)?;
        }
        std::fs::create_dir_all(&self.work_dir).map_err(StoreError::Io)?;
        // Guard deletes work_dir on any error return or panic; disarmed on success.
        let mut work_dir_guard = WorkDirGuard { path: self.work_dir.clone(), active: true };

        // Open (or create) the database with the correct CF descriptors.
        let mut db_opts = Options::default();
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);
        db_opts.set_max_background_jobs(rocks_opts.max_background_jobs);
        let v_bo = cf_options::vertex_block_opts(rocks_opts);
        let e_bo = cf_options::edge_block_opts(rocks_opts);
        let cfs = vec![
            ColumnFamilyDescriptor::new(CF_VERTICES, cf_options::vertex_cf_opts(rocks_opts, &v_bo)),
            ColumnFamilyDescriptor::new(CF_VERTEX_DEGREE, cf_options::vertex_cf_opts(rocks_opts, &v_bo)),
            ColumnFamilyDescriptor::new(CF_EDGES_OUT, cf_options::edge_cf_opts(rocks_opts, &e_bo)),
            ColumnFamilyDescriptor::new(CF_EDGES_IN, cf_options::edge_cf_opts(rocks_opts, &e_bo)),
            ColumnFamilyDescriptor::new(CF_SCHEMA, Options::default()),
        ];
        let db =
            OptimisticTransactionDB::open_cf_descriptors(&db_opts, &self.db_path, cfs).map_err(StoreError::RocksDb)?;

        bulk_log(t0, "writing schema CF");
        // Write schema CF + crash marker atomically before any SST is generated.
        let resolved = write_schema_cf(&db, &schema, &graph_opts)?;

        // ── Phase 1a: stream vertices ───────────────────────────────────────────
        // vertex_sorter  → vertex CF SST (sorted by vertex key)
        // label_sorter   → SortedLabelFile (sorted by vertex_id, deduplicated)
        // Budget: max_memory_bytes/4 each — both run concurrently during this phase.
        let budget_v = self.max_memory_bytes / 4;
        let mut vertex_sorter = ExternalSorter::new(self.work_dir.join("sv"), budget_v);
        let mut label_sorter = ExternalSorter::new(self.work_dir.join("sl"), budget_v);
        let mut vcount = 0u64;

        bulk_log(t0, "phase 1 — streaming vertices");
        for v in vertices {
            let lid = resolved.resolve_vertex_label(&v.label)?;
            if graph_opts.mode == SchemaMode::Strict {
                for k in v.props.keys() {
                    resolved.resolve_prop_key(k)?;
                }
            }
            let blob = resolved.encode_props(&v.props);
            vertex_sorter.push(
                kv_codec::encode_vertex_key(v.id).to_vec(),
                kv_codec::VertexValue { label_id: lid, property_blob: blob }.encode(),
            )?;
            label_sorter.push(v.id.to_be_bytes().to_vec(), lid.to_be_bytes().to_vec())?;
            vcount += 1;
        }
        // Materialise label_sorter into an on-disk file readable multiple times.
        let label_file_path = self.work_dir.join("vertex_labels.bin");
        let label_file = SortedLabelFile::write_from(label_sorter, &label_file_path)?;
        bulk_log(t0, &format!("phase 1 — {vcount} vertices; label file written ({} unique)", label_file.count));

        // ── Phase 1b: stream edges into annotation + degree sorters ────────────
        // Annotation sorters (budget/4 each — advice B: concurrent during edge streaming):
        //   dst_annot: key = (dst_id:8, out_edge_key:22) → annotate with dst_label for edges_out
        //   src_annot: key = (src_id:8, in_edge_key:22)  → annotate with src_label for edges_in
        // Degree sorters (budget/8 each — 8-byte keys only, small):
        //   out_deg: key = src_id:8 → count consecutive = out-degree per vertex
        //   in_deg:  key = dst_id:8 → count consecutive = in-degree per vertex
        //
        // Referential integrity (edge src/dst in vertex set) is checked during
        // Phase 2 annotation (advice C) rather than here — SchemaViolation if missing.
        let budget_a = self.max_memory_bytes / 4;
        let budget_d = self.max_memory_bytes / 8;
        let mut dst_annot = ExternalSorter::new(self.work_dir.join("ea_dst"), budget_a);
        let mut src_annot = ExternalSorter::new(self.work_dir.join("ea_src"), budget_a);
        let mut out_deg = ExternalSorter::new(self.work_dir.join("deg_out"), budget_d);
        let mut in_deg = ExternalSorter::new(self.work_dir.join("deg_in"), budget_d);
        let ecount;

        match graph_opts.edge_mode {
            EdgeMode::Single => {
                bulk_log(t0, "phase 1 — streaming edges (Single mode)");
                let mut n = 0u64;
                let mut last_report = Instant::now();
                for edge in edges {
                    let lid = resolved.resolve_edge_label(&edge.label)?;
                    if graph_opts.mode == SchemaMode::Strict {
                        for k in edge.props.keys() {
                            resolved.resolve_prop_key(k)?;
                        }
                    }
                    let blob = resolved.encode_props(&edge.props);
                    let cek = CanonicalEdgeKey { src_id: edge.src, label_id: lid, dst_id: edge.dst, rank: 0 };
                    // dst_annot: sorted by (dst_id, out_edge_key) for dst_label join
                    let mut dk = [0u8; 30];
                    dk[0..8].copy_from_slice(&edge.dst.to_be_bytes());
                    dk[8..30].copy_from_slice(&kv_codec::encode_edge_key(&cek.out_key()));
                    dst_annot.push(dk.to_vec(), blob.clone())?;
                    // src_annot: sorted by (src_id, in_edge_key) for src_label join
                    let mut sk = [0u8; 30];
                    sk[0..8].copy_from_slice(&edge.src.to_be_bytes());
                    sk[8..30].copy_from_slice(&kv_codec::encode_edge_key(&cek.in_key()));
                    src_annot.push(sk.to_vec(), blob)?;
                    out_deg.push(edge.src.to_be_bytes().to_vec(), vec![])?;
                    in_deg.push(edge.dst.to_be_bytes().to_vec(), vec![])?;
                    n += 1;
                    if n % 1_000_000 == 0 && last_report.elapsed().as_secs_f64() >= 5.0 {
                        bulk_log(t0, &format!("  {n} edges streamed"));
                        last_report = Instant::now();
                    }
                }
                ecount = n;
            }
            EdgeMode::Multi => {
                // Pre-sort edges by (src:8, label:4, dst:8, rank_or_MAX:2) to assign ranks,
                // then push with final edge keys to the annotation+degree sorters.
                // Only pre_sorter is active during edge streaming (budget/4); the
                // annotation+degree sorters are populated sequentially after — advice B.
                let pre_budget = self.max_memory_bytes / 4;
                let mut pre_sorter = ExternalSorter::new(self.work_dir.join("sm_pre"), pre_budget);
                let mut n = 0u64;
                let mut last_report = Instant::now();

                bulk_log(t0, "phase 1 — streaming edges into pre-sorter (Multi mode)");
                for edge in edges {
                    let lid = resolved.resolve_edge_label(&edge.label)?;
                    if graph_opts.mode == SchemaMode::Strict {
                        for k in edge.props.keys() {
                            resolved.resolve_prop_key(k)?;
                        }
                    }
                    let rank_for_sort = edge.rank.unwrap_or(Rank::MAX);
                    let mut sort_key = [0u8; 22];
                    sort_key[0..8].copy_from_slice(&edge.src.to_be_bytes());
                    sort_key[8..12].copy_from_slice(&lid.to_be_bytes());
                    sort_key[12..20].copy_from_slice(&edge.dst.to_be_bytes());
                    sort_key[20..22].copy_from_slice(&rank_for_sort.to_be_bytes());
                    pre_sorter.push(sort_key.to_vec(), resolved.encode_props(&edge.props))?;
                    n += 1;
                    if n % 1_000_000 == 0 && last_report.elapsed().as_secs_f64() >= 5.0 {
                        bulk_log(t0, &format!("  {n} edges streamed"));
                        last_report = Instant::now();
                    }
                }
                bulk_log(t0, &format!("phase 1 — {n} edges pre-sorted; assigning ranks"));

                // Rank assignment: same sentinel logic as single-mode pre-sort.
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
                        if last_explicit == Some(rank_from_key) {
                            return Err(StoreError::DuplicateEdge(CanonicalEdgeKey {
                                src_id: src,
                                label_id: lid,
                                dst_id: dst,
                                rank: rank_from_key,
                            }));
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
        bulk_log(t0, &format!("phase 1 done — {ecount} edges"));

        let v_bo = cf_options::vertex_block_opts(rocks_opts);
        let v_opts = cf_options::vertex_cf_opts(rocks_opts, &v_bo);
        let e_bo = cf_options::edge_block_opts(rocks_opts);
        let e_opts = cf_options::edge_cf_opts(rocks_opts, &e_bo);

        // ── Phase 2: write all SSTs (sequentially — full budget per step, advice B) ──

        // Vertex SST: vertex_sorter output is already sorted by vertex key.
        bulk_log(t0, &format!("phase 2 — writing vertex SSTs ({vcount} vertices)"));
        let vert_files =
            write_sst_from_iter("vertices", &mut vertex_sorter.finish()?, &self.work_dir, self.max_sst_size, &v_opts)?;

        // Degree SST: three-way merge of label_file + out_deg + in_deg, O(1) memory.
        bulk_log(t0, "phase 2 — writing degree SSTs");
        let deg_files = write_degree_sst(
            &label_file,
            out_deg.finish()?,
            in_deg.finish()?,
            &self.work_dir,
            self.max_sst_size,
            &v_opts,
        )?;

        // Edges_out SST: annotate with dst_label, sort by out-edge key order.
        // out_edge_sorter gets full max_memory_bytes — sequential, no other sorter active.
        bulk_log(t0, "phase 2 — annotating + writing edges_out SSTs");
        let mut out_edge_sorter = ExternalSorter::new(self.work_dir.join("eo"), self.max_memory_bytes);
        annotate_edges(dst_annot.finish()?, &label_file, &mut out_edge_sorter)?;
        let out_files = write_sst_from_iter_dedup(
            "edges_out",
            out_edge_sorter.finish()?,
            &self.work_dir,
            self.max_sst_size,
            &e_opts,
        )?;

        // Edges_in SST: annotate with src_label, sort by in-edge key order.
        bulk_log(t0, "phase 2 — annotating + writing edges_in SSTs");
        let mut in_edge_sorter = ExternalSorter::new(self.work_dir.join("ei"), self.max_memory_bytes);
        annotate_edges(src_annot.finish()?, &label_file, &mut in_edge_sorter)?;
        let in_files =
            write_sst_from_iter("edges_in", &mut in_edge_sorter.finish()?, &self.work_dir, self.max_sst_size, &e_opts)?;

        let sst_paths = SstPaths { vertices: vert_files, degree: deg_files, edges_out: out_files, edges_in: in_files };

        bulk_log(t0, &format!("phase 3 — ingesting {} SST files (atomic)", sst_paths.total_files()));
        // Phase 3: atomic ingest — all CFs link in or none do.
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

        // Delete crash marker now that ingest succeeded.
        let cf_sch = db.cf_handle(CF_SCHEMA).ok_or(StoreError::MissingColumnFamily(CF_SCHEMA))?;
        let mut cleanup = WriteBatchWithTransaction::<true>::default();
        cleanup.delete_cf(&cf_sch, BULK_LOAD_IN_PROGRESS_KEY);
        db.write(cleanup).map_err(StoreError::RocksDb)?;

        // Compact all data CFs so ingested SST files are merged from L0 into
        // deeper levels.  Without compaction the L0 files cause multi-file merges
        // on every full scan (g.V().count(), g.E([]).count(), etc.), which can be
        // 5–30× slower than a single sequential read over compacted data.
        let n_files = sst_paths.total_files();
        bulk_log(t0, &format!("done — {vcount} vertices, {ecount} edges, {n_files} SST files"));
        bulk_log(t0, "compacting all CFs (moves L0 SSTs into deeper levels for fast scans)");
        for cf_name in [CF_VERTICES, CF_VERTEX_DEGREE, CF_EDGES_OUT, CF_EDGES_IN] {
            if let Some(cf) = db.cf_handle(cf_name) {
                db.compact_range_cf(&cf, None::<&[u8]>, None::<&[u8]>);
            }
        }
        bulk_log(t0, "compaction done");
        work_dir_guard.active = false; // disarm: drop will skip removal
        Ok(BulkLoadStats { vertices_written: vcount, edges_written: ecount, sst_files: n_files })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tempfile::tempdir;

    use crate::{
        api::Graph,
        gremlin::{traversal::TraversalBuilder, value::Value},
        schema::{
            definition::{EdgeMode, SchemaMode},
            GraphOptions,
        },
        store::rocks::store::RocksOptions,
    };

    use super::*;

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

        // Re-open via the public Graph API and verify counts via Gremlin.
        let graph = Graph::open(&db_path).unwrap();
        let mut snap = graph.read();
        let v_count = snap.g().V([]).count().next().unwrap().unwrap();
        assert_eq!(v_count, Value::Int64(5));
        // Vertex 1 has out-edges to 2 and 3.
        let out_count = snap.g().V([1_i64]).out(["Knows"]).count().next().unwrap().unwrap();
        assert_eq!(out_count, Value::Int64(2));
        graph.close().unwrap();
    }

    #[test]
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
    fn test_crash_marker_detection() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db");

        // Run a full load to create the DB with correct schema and data.
        SstBulkLoader::new(&db_path, dir.path().join("_bulk_work"))
            .load_initial(
                small_schema(),
                small_vertices().into_iter(),
                small_edges().into_iter(),
                GraphOptions::default(),
                &RocksOptions::default(),
            )
            .unwrap();

        // Simulate a crash that left the marker behind by writing it back.
        {
            use super::super::{CF_EDGES_IN, CF_EDGES_OUT, CF_SCHEMA, CF_VERTEX_DEGREE, CF_VERTICES};
            use crate::store::rocks::cf_options;
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
            batch.put_cf(&cf, BULK_LOAD_IN_PROGRESS_KEY, [1u8]);
            db.write(batch).unwrap();
        }

        // Graph::open should detect data is present and auto-clear the marker.
        let graph = Graph::open(&db_path).unwrap();
        let mut snap = graph.read();
        let v_count = snap.g().V([]).count().next().unwrap().unwrap();
        assert_eq!(v_count, Value::Int64(5));
        graph.close().unwrap();
    }

    #[test]
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

        // Re-open and verify both edges exist with different ranks
        let graph = Graph::open(dir.path().join("db")).unwrap();
        let mut snap = graph.read();
        let edges_count = snap.g().V([1_i64]).outE(["Knows"]).count().next().unwrap().unwrap();
        assert_eq!(edges_count, Value::Int64(2));
        graph.close().unwrap();
    }

    #[test]
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
    fn test_load_initial_external_sort() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db");
        // max_memory_bytes=1 forces the external sort path for every CF.
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

        // Round-trip verification: same counts as the in-memory path.
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

        // Build a valid out-edge key for (src=10, label=1, dst=20, rank=0).
        let cek = CanonicalEdgeKey { src_id: 10, label_id: 1, dst_id: 20, rank: 0 };
        let key = kv_codec::encode_edge_key(&cek.out_key()).to_vec();
        let val = kv_codec::EdgeValue { end_vertex_label: 1, property_blob: vec![] }.encode();

        // Feed two identical (key, val) pairs — dedup should fire on the second.
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

        // Feed out-of-order labels, some duplicates
        sorter.push(20i64.to_be_bytes().to_vec(), 2i32.to_be_bytes().to_vec()).unwrap();
        sorter.push(10i64.to_be_bytes().to_vec(), 1i32.to_be_bytes().to_vec()).unwrap();
        sorter.push(10i64.to_be_bytes().to_vec(), 1i32.to_be_bytes().to_vec()).unwrap(); // duplicate
        sorter.push(30i64.to_be_bytes().to_vec(), 3i32.to_be_bytes().to_vec()).unwrap();

        let file = SortedLabelFile::write_from(sorter, &path).unwrap();
        assert_eq!(file.count, 3); // 10, 20, 30 (deduplicated)

        let reader: Vec<_> = file.reader().unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(reader, vec![(10, 1), (20, 2), (30, 3)]);
    }

    #[test]
    fn test_sorted_label_file_conflicting_labels() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("labels.bin");
        let mut sorter = ExternalSorter::new(dir.path().join("sort"), 1024 * 1024);

        // Same vertex ID, conflicting labels
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
        assert_eq!(counter.count_for(15).unwrap(), 0); // past, but doesn't exist
        assert_eq!(counter.count_for(20).unwrap(), 1);
        assert_eq!(counter.count_for(30).unwrap(), 2);
        assert_eq!(counter.count_for(40).unwrap(), 0); // stream exhausted
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn test_annotate_edges_mismatched_vertex() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("labels.bin");
        let mut sorter = ExternalSorter::new(dir.path().join("sort"), 1024 * 1024);
        sorter.push(10i64.to_be_bytes().to_vec(), 1i32.to_be_bytes().to_vec()).unwrap();
        let file = SortedLabelFile::write_from(sorter, &path).unwrap();

        // Edge references vertex 20, which is missing from labels
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

    // ── New tests (1–10) ───────────────────────────────────────────────────────

    // 1. Empty input — 0 vertices, 0 edges
    #[test]
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

    // 2. Vertices only, no edges
    #[test]
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

    // 3. Properties roundtrip — vertex age and edge score survive load_initial
    #[test]
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

    // 4. Multi mode explicit ranks — both stored, retrievable
    #[test]
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

    // 5. Multi mode explicit rank duplicate → DuplicateEdge
    #[test]
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

    // 6. Multi mode rank overflow — explicit rank 65534 + auto-rank exhausts u16 → SchemaViolation
    #[test]
    fn test_multi_mode_rank_overflow() {
        let dir = tempdir().unwrap();
        let vertices: Vec<BulkVertex> =
            (1..=2).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
        // After explicit rank 65534, next_rank = 65535 (the sentinel u16::MAX).
        // The auto-rank edge then tries to use 65535 as its rank, but incrementing
        // next_rank to 65536 overflows u16 → SchemaViolation.
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

    // 7. Non-default max_sst_size — load succeeds and produces correct results
    #[test]
    fn test_sst_file_splitting() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("db");
        // Note: SstFileWriter::file_size() returns 0 until its internal buffer
        // flushes, so splitting only triggers on large datasets. For this tiny
        // graph we get exactly one file per CF (4 total). The test verifies
        // correctness with a non-default max_sst_size setting.
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

    // 8. WorkDirGuard cleanup on error — work_dir removed after a failing load
    #[test]
    fn test_work_dir_cleaned_up_on_error() {
        let dir = tempdir().unwrap();
        let work_dir = dir.path().join("_w");
        // Unknown edge label triggers SchemaViolation during edge streaming (Phase 1b),
        // after work_dir and several sorter subdirectories have been created.
        let vertices: Vec<BulkVertex> =
            (1..=2).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
        let edges = vec![BulkEdge { src: 1, dst: 2, label: "Unknown".into(), props: HashMap::new(), rank: None }];
        let err = SstBulkLoader::new(dir.path().join("db"), work_dir.clone())
            .load_initial(
                small_schema(),
                vertices.into_iter(),
                edges.into_iter(),
                GraphOptions::default(),
                &RocksOptions::default(),
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::SchemaViolation(_)));
        assert!(!work_dir.exists(), "WorkDirGuard should have removed work_dir on error");
    }

    // 9. Empty SortedLabelFile — 0 vertices produces valid file with count=0
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

    // 10. Strict mode rejects undeclared edge property key
    #[test]
    fn test_strict_mode_undeclared_edge_property() {
        use crate::{schema::DataType, Primitive};
        let dir = tempdir().unwrap();
        let schema = BulkSchema {
            vertex_labels: vec!["Person".into()],
            edge_labels: vec!["Knows".into()],
            prop_keys: vec![("age".into(), DataType::Int64)], // "weight" not declared
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
}
