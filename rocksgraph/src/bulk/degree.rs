// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! On-disk sorted vertex label file and vertex degree SST generation.
//!
//! [`SortedLabelFile`] is written once from an [`ExternalSorter`](super::sort::ExternalSorter)
//! and read back multiple times — twice by [`annotate_edges`](super::edge_annotator::annotate_edges)
//! and once by [`write_degree_sst`] — to join edge streams against vertex labels without holding
//! the full vertex set in memory.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use rocksdb::{Options, SstFileWriter};

use crate::types::{
    keys::{LabelId, VertexKey},
    kv_codec, StoreError,
};

use super::sort::ExternalSorter;

// ── SortedLabelFile ────────────────────────────────────────────────────────────

/// Compact on-disk sorted file of (VertexKey, LabelId) pairs (12 bytes each).
/// Written once from an ExternalSorter with deduplication; readable independently
/// multiple times (two annotation passes + one degree pass). Deleted on drop.
#[derive(Debug)]
pub(crate) struct SortedLabelFile {
    path: PathBuf,
    pub(crate) count: u64,
}

impl SortedLabelFile {
    pub(crate) fn write_from(sorter: ExternalSorter, path: &Path) -> Result<Self, StoreError> {
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

    pub(crate) fn reader(&self) -> Result<LabelFileIter, StoreError> {
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
pub(crate) struct LabelFileIter {
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
pub(crate) struct DegreeCounter<I: Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>>> {
    iter: I,
    head: Option<VertexKey>,
}

impl<I: Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>>> DegreeCounter<I> {
    pub(crate) fn new(mut iter: I) -> Result<Self, StoreError> {
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

    pub(crate) fn count_for(&mut self, vid: VertexKey) -> Result<u32, StoreError> {
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

// ── write_degree_sst ───────────────────────────────────────────────────────────

pub(crate) fn write_degree_sst<I1, I2>(
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
