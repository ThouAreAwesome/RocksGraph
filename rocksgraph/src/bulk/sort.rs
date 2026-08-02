// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! External merge sorter — byte-ordered key-value pairs with disk spill.
//!
//! ## Design
//!
//! ### Flat buffer (in-memory phase)
//! All key and value bytes are packed into a single `raw_data: Vec<u8>`.
//! An `offsets` vec records `(key_start, key_len, val_start, val_len)` per entry.
//! Sorting is done on `offsets` using a comparator that references `raw_data` —
//! zero additional allocations during the sort phase.
//!
//! ### Cascaded merge (spill phase)
//! If the number of chunk files exceeds `MAX_OPEN_CHUNKS`, intermediate merge
//! passes reduce the fan-in before the final K-way merge. This prevents the OS
//! from hitting its open-file-descriptor limit on very large datasets.

use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use crate::types::StoreError;

type KvResult = Result<(Vec<u8>, Vec<u8>), StoreError>;
type KvPair = (Vec<u8>, Vec<u8>);

/// Maximum chunk files opened simultaneously during K-way merge.
/// Prevents "Too many open files" errors on datasets that produce hundreds of spill files.
/// 128 leaves comfortable headroom below the typical OS default of 256.
const MAX_OPEN_CHUNKS: usize = 128;

// ── Heap entry for min-heap K-way merge ────────────────────────────────────────

#[derive(Eq, PartialEq)]
struct HeapEntry {
    key: Vec<u8>,
    idx: usize,
    val: Vec<u8>,
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.key.cmp(&self.key).then(other.idx.cmp(&self.idx))
    }
}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// ── ExternalSorter ─────────────────────────────────────────────────────────────

pub(crate) struct ExternalSorter {
    work_dir: PathBuf,
    max_memory_bytes: usize,

    // Flat buffer: all key/value bytes packed into one contiguous allocation.
    raw_data: Vec<u8>,
    // Index into raw_data: (key_start, key_len, val_start, val_len).
    offsets: Vec<(usize, usize, usize, usize)>,
    buffer_bytes: usize,

    chunk_files: Vec<PathBuf>,
    chunk_counter: usize,
}

impl ExternalSorter {
    pub(crate) fn new(work_dir: PathBuf, max_memory_bytes: usize) -> Self {
        let _ = std::fs::create_dir_all(&work_dir);
        Self {
            work_dir,
            max_memory_bytes,
            raw_data: Vec::new(),
            offsets: Vec::new(),
            buffer_bytes: 0,
            chunk_files: Vec::new(),
            chunk_counter: 0,
        }
    }

    /// Append one (key, value) pair. Spills to disk when the buffer is full.
    pub(crate) fn push(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<(), StoreError> {
        self.buffer_bytes += key.len() + value.len();
        let key_start = self.raw_data.len();
        let key_len = key.len();
        self.raw_data.extend_from_slice(&key);
        let val_start = self.raw_data.len();
        let val_len = value.len();
        self.raw_data.extend_from_slice(&value);
        self.offsets.push((key_start, key_len, val_start, val_len));
        if self.buffer_bytes >= self.max_memory_bytes {
            self.spill()?;
        }
        Ok(())
    }

    /// Finalize and return all records in strictly ascending key byte order.
    pub(crate) fn finish(mut self) -> Result<impl Iterator<Item = KvResult>, StoreError> {
        if self.chunk_files.is_empty() {
            // Everything fits in memory — sort offsets in-place, then extract.
            let raw = &self.raw_data;
            self.offsets
                .sort_unstable_by(|&(ks1, kl1, _, _), &(ks2, kl2, _, _)| raw[ks1..ks1 + kl1].cmp(&raw[ks2..ks2 + kl2]));
            let raw_data = std::mem::take(&mut self.raw_data);
            let pairs: Vec<KvPair> = std::mem::take(&mut self.offsets)
                .into_iter()
                .map(|(ks, kl, vs, vl)| (raw_data[ks..ks + kl].to_vec(), raw_data[vs..vs + vl].to_vec()))
                .collect();
            return Ok(SortedIter::Memory(pairs.into_iter()));
        }

        // Flush any remaining buffer as a final chunk.
        if !self.offsets.is_empty() {
            if let Err(e) = self.spill() {
                self.cleanup_chunk_files();
                return Err(e);
            }
        }

        // Reduce fan-in to <= MAX_OPEN_CHUNKS via cascaded intermediate merges.
        if let Err(e) = self.cascade_merge() {
            self.cleanup_chunk_files();
            return Err(e);
        }

        // Final K-way merge.
        let chunk_files = std::mem::take(&mut self.chunk_files);
        let readers: Vec<_> = chunk_files.into_iter().map(ChunkReader::open).collect::<Result<_, _>>()?;
        Ok(SortedIter::Merge(Merger::new(readers)))
    }

    // ── Private helpers ────────────────────────────────────────────────────────

    fn spill(&mut self) -> Result<(), StoreError> {
        // Sort offsets by key using the flat raw_data buffer — zero extra allocations.
        let raw = &self.raw_data;
        self.offsets
            .sort_unstable_by(|&(ks1, kl1, _, _), &(ks2, kl2, _, _)| raw[ks1..ks1 + kl1].cmp(&raw[ks2..ks2 + kl2]));

        let path = self.work_dir.join(format!("chunk_{:06}.bin", self.chunk_counter));
        self.chunk_counter += 1;

        let file = File::create(&path).map_err(StoreError::Io)?;
        let mut w = BufWriter::new(file);
        let count = self.offsets.len() as u64;
        w.write_all(&count.to_le_bytes()).map_err(StoreError::Io)?;
        for &(ks, kl, vs, vl) in &self.offsets {
            w.write_all(&(kl as u32).to_le_bytes()).map_err(StoreError::Io)?;
            w.write_all(&self.raw_data[ks..ks + kl]).map_err(StoreError::Io)?;
            w.write_all(&(vl as u32).to_le_bytes()).map_err(StoreError::Io)?;
            w.write_all(&self.raw_data[vs..vs + vl]).map_err(StoreError::Io)?;
        }
        w.flush().map_err(StoreError::Io)?;

        self.chunk_files.push(path);
        self.raw_data.clear();
        self.offsets.clear();
        self.buffer_bytes = 0;
        Ok(())
    }

    /// Reduce `chunk_files` to at most `MAX_OPEN_CHUNKS` via iterative merge passes.
    ///
    /// Each pass groups up to `MAX_OPEN_CHUNKS` consecutive chunk files, merges
    /// them into a single new chunk file, and repeats until the fan-in is within
    /// the limit. Input chunk files are deleted automatically via `ChunkReader`'s
    /// RAII `Drop` impl when the merger is consumed.
    /// Reduce `chunk_files` to at most `MAX_OPEN_CHUNKS` via iterative merge passes.
    fn cascade_merge(&mut self) -> Result<(), StoreError> {
        while self.chunk_files.len() > MAX_OPEN_CHUNKS {
            let input_chunks: Vec<PathBuf> = self.chunk_files.drain(..).collect();

            if let Err(e) = self.cascade_merge_pass(&input_chunks) {
                // Clean up input files that were not consumed by ChunkReader::Drop
                // (unprocessed groups further in the loop never had their files opened).
                for path in &input_chunks {
                    let _ = std::fs::remove_file(path);
                }
                // Clean up any merged output files from successful iterations plus
                // the partial file from the failing iteration (pushed early, see below).
                self.cleanup_chunk_files();
                return Err(e);
            }
        }
        Ok(())
    }

    /// One pass of the cascade: merge groups of `MAX_OPEN_CHUNKS` input chunk files into
    /// single merged chunk files, accumulating results into `self.chunk_files`.
    ///
    /// `merged_path` is pushed to `self.chunk_files` **before** writing so that
    /// `cleanup_chunk_files()` in the error path of `cascade_merge` covers partial files.
    fn cascade_merge_pass(&mut self, input_chunks: &[PathBuf]) -> Result<(), StoreError> {
        for group in input_chunks.chunks(MAX_OPEN_CHUNKS) {
            if group.len() == 1 {
                // Single chunk — carry it forward unchanged.
                self.chunk_files.push(group[0].clone());
                continue;
            }

            let merged_path = self.work_dir.join(format!("chunk_{:06}.bin", self.chunk_counter));
            self.chunk_counter += 1;
            // Push before writing: if a write error occurs, cleanup_chunk_files() will
            // delete this file (which may be empty or partially written).
            self.chunk_files.push(merged_path.clone());

            // Open this group for K-way merging.
            let readers: Vec<_> = group.iter().map(|p| ChunkReader::open(p.clone())).collect::<Result<_, _>>()?;
            let mut merger = Merger::new(readers);

            // Write placeholder count, fill records, then seek back to fix the count.
            let mut file = File::create(&merged_path).map_err(StoreError::Io)?;
            file.write_all(&0u64.to_le_bytes()).map_err(StoreError::Io)?;
            let mut w = BufWriter::new(file);
            let mut count = 0u64;
            for item in &mut merger {
                let (key, val) = item?;
                w.write_all(&(key.len() as u32).to_le_bytes()).map_err(StoreError::Io)?;
                w.write_all(&key).map_err(StoreError::Io)?;
                w.write_all(&(val.len() as u32).to_le_bytes()).map_err(StoreError::Io)?;
                w.write_all(&val).map_err(StoreError::Io)?;
                count += 1;
            }
            w.flush().map_err(StoreError::Io)?;
            // `merger` drops here — ChunkReader::Drop deletes all input files in this group.
            drop(merger);

            let mut file = w.into_inner().map_err(|e| StoreError::Io(e.into_error()))?;
            file.seek(SeekFrom::Start(0)).map_err(StoreError::Io)?;
            file.write_all(&count.to_le_bytes()).map_err(StoreError::Io)?;
        }
        Ok(())
    }

    fn cleanup_chunk_files(&mut self) {
        for path in self.chunk_files.drain(..) {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Drop for ExternalSorter {
    fn drop(&mut self) {
        // Clean up any spill chunks that were not consumed by finish().
        // On the success path, finish() calls std::mem::take on chunk_files
        // before returning, so this is a no-op. On error or panic paths, any
        // remaining chunk files are deleted here.
        self.cleanup_chunk_files();
    }
}

// ── Output iterator ────────────────────────────────────────────────────────────

enum SortedIter {
    Memory(std::vec::IntoIter<KvPair>),
    Merge(Merger),
}

impl Iterator for SortedIter {
    type Item = KvResult;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            SortedIter::Memory(iter) => iter.next().map(Ok),
            SortedIter::Merge(merger) => merger.next(),
        }
    }
}

// ── Chunk reader ───────────────────────────────────────────────────────────────

struct ChunkReader {
    reader: BufReader<File>,
    remaining: u64,
    path: Option<PathBuf>,
}

impl ChunkReader {
    fn open(path: PathBuf) -> Result<Self, StoreError> {
        let file = File::open(&path).map_err(StoreError::Io)?;
        let mut reader = BufReader::new(file);
        let mut count_buf = [0u8; 8];
        reader.read_exact(&mut count_buf).map_err(StoreError::Io)?;
        let remaining = u64::from_le_bytes(count_buf);
        Ok(Self { reader, remaining, path: Some(path) })
    }

    fn take_front(&mut self) -> Option<KvResult> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let mut len_buf = [0u8; 4];
        if let Err(e) = self.reader.read_exact(&mut len_buf) {
            return Some(Err(StoreError::Io(e)));
        }
        let key_len = u32::from_le_bytes(len_buf) as usize;
        let mut key = vec![0u8; key_len];
        if let Err(e) = self.reader.read_exact(&mut key) {
            return Some(Err(StoreError::Io(e)));
        }
        if let Err(e) = self.reader.read_exact(&mut len_buf) {
            return Some(Err(StoreError::Io(e)));
        }
        let val_len = u32::from_le_bytes(len_buf) as usize;
        let mut val = vec![0u8; val_len];
        if let Err(e) = self.reader.read_exact(&mut val) {
            return Some(Err(StoreError::Io(e)));
        }
        Some(Ok((key, val)))
    }
}

impl Drop for ChunkReader {
    fn drop(&mut self) {
        if let Some(ref p) = self.path {
            let _ = std::fs::remove_file(p);
        }
    }
}

// ── K-way merger ───────────────────────────────────────────────────────────────

struct Merger {
    readers: Vec<ChunkReader>,
    heap: BinaryHeap<HeapEntry>,
}

impl Merger {
    fn new(mut readers: Vec<ChunkReader>) -> Self {
        let mut heap = BinaryHeap::with_capacity(readers.len());
        for (i, r) in readers.iter_mut().enumerate() {
            if let Some(Ok((key, val))) = r.take_front() {
                heap.push(HeapEntry { key, idx: i, val });
            }
        }
        Self { readers, heap }
    }
}

impl Iterator for Merger {
    type Item = KvResult;

    fn next(&mut self) -> Option<Self::Item> {
        let HeapEntry { key, idx, val } = self.heap.pop()?;
        if let Some(next) = self.readers[idx].take_front() {
            match next {
                Ok((nk, nv)) => self.heap.push(HeapEntry { key: nk, idx, val: nv }),
                Err(e) => return Some(Err(e)),
            }
        }
        Some(Ok((key, val)))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn check_sorted(results: &[(Vec<u8>, Vec<u8>)]) {
        for w in results.windows(2) {
            assert!(w[0].0 <= w[1].0, "out of order: {:?} > {:?}", w[0].0, w[1].0);
        }
    }

    #[test]
    fn test_in_memory_sort() {
        let dir = tempdir().unwrap();
        let mut sorter = ExternalSorter::new(dir.path().join("s"), 1024 * 1024);
        for i in (0i32..200).rev() {
            sorter.push(i.to_be_bytes().to_vec(), Vec::new()).unwrap();
        }
        let results: Vec<_> = sorter.finish().unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(results.len(), 200);
        check_sorted(&results);
    }

    #[test]
    fn test_spill_and_merge() {
        let dir = tempdir().unwrap();
        // max_memory_bytes=50, each entry ≈ 34 bytes → ~250 spill chunks → cascaded merge.
        let mut sorter = ExternalSorter::new(dir.path().join("s"), 50);
        for i in (0i32..500).rev() {
            sorter.push(i.to_be_bytes().to_vec(), vec![0u8; 30]).unwrap();
        }
        let results: Vec<_> = sorter.finish().unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(results.len(), 500);
        check_sorted(&results);
    }

    #[test]
    fn test_empty() {
        let dir = tempdir().unwrap();
        let sorter = ExternalSorter::new(dir.path().join("s"), 1024);
        assert_eq!(sorter.finish().unwrap().count(), 0);
    }

    #[test]
    fn test_duplicate_keys_stable() {
        let dir = tempdir().unwrap();
        let mut sorter = ExternalSorter::new(dir.path().join("s"), 1024 * 1024);
        for _ in 0..5 {
            sorter.push(vec![0], vec![]).unwrap();
            sorter.push(vec![1], vec![]).unwrap();
        }
        let results: Vec<_> = sorter.finish().unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(results.len(), 10);
        check_sorted(&results);
    }

    #[test]
    fn test_cascaded_merge() {
        // max_memory_bytes=1: each push of a 1-byte key immediately spills.
        // 300 entries → 300 chunk files > MAX_OPEN_CHUNKS=128 → cascade triggered.
        let dir = tempdir().unwrap();
        let mut sorter = ExternalSorter::new(dir.path().join("s"), 1);
        for i in (0u32..300).rev() {
            sorter.push(i.to_be_bytes().to_vec(), Vec::new()).unwrap();
        }
        let results: Vec<_> = sorter.finish().unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(results.len(), 300);
        check_sorted(&results);
        // Verify no chunk files were left behind.
        let leftover = std::fs::read_dir(dir.path().join("s")).map(|d| d.count()).unwrap_or(0);
        assert_eq!(leftover, 0, "chunk files leaked");
    }
}
