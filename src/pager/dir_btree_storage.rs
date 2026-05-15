//! 目錄式儲存（多檔 BTree）
#![allow(dead_code, unused)]

use super::codec::{decode_node, encode_node, PAGE_SIZE};
use super::wal::Wal;
use super::cache::{PageCache, BloomFilter, DIR_DEFAULT_CACHE_SIZE};
use super::page_lock::PageLockManager;
use super::storage::Storage;
use crate::btree::node::Node;
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, RwLock, Mutex};
use std::time::{Duration, Instant};

const FD_POOL_MAX_SIZE: usize = 256;
const NODE_CACHE_MAX_SIZE: usize = 1024;
const DEFAULT_STRIPE_COUNT: usize = 64;
const WRITE_COALESCE_WINDOW_MS: u64 = 5;
const WRITE_COALESCE_THRESHOLD: usize = 100;

struct FdEntry {
    file: File,
    last_access: Instant,
}

pub struct FileHandleCache {
    data_dir: std::path::PathBuf,
    handles: RwLock<HashMap<usize, FdEntry>>,
    max_size: usize,
}

impl FileHandleCache {
    fn new(data_dir: std::path::PathBuf) -> Self {
        Self {
            data_dir,
            handles: RwLock::new(HashMap::new()),
            max_size: FD_POOL_MAX_SIZE,
        }
    }

    fn get_file(&self, page_id: usize) -> std::io::Result<std::fs::File> {
        let path = self.data_dir.join(format!("{}.page", page_id));
        let now = Instant::now();

        if let Ok(handles) = self.handles.read() {
            if let Some(entry) = handles.get(&page_id) {
                let file = entry.file.try_clone()?;
                drop(handles);
                if let Ok(mut handles) = self.handles.write() {
                    if let Some(entry) = handles.get_mut(&page_id) {
                        entry.last_access = now;
                    }
                }
                return Ok(file);
            }
        }

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)?;

        let mut handles = self.handles.write().map_err(|_| std::io::Error::new(std::io::ErrorKind::WouldBlock, "lock poisoned"))?;
        if handles.len() >= self.max_size {
            if let Some((oldest_id, _)) = handles
                .iter()
                .min_by_key(|(_, entry)| entry.last_access)
                .map(|(id, entry)| (*id, entry.last_access))
            {
                handles.remove(&oldest_id);
            }
        }
        handles.insert(page_id, FdEntry { file: file.try_clone()?, last_access: now });
        Ok(file)
    }

    fn invalidate(&self, page_id: usize) {
        if let Ok(mut handles) = self.handles.write() {
            handles.remove(&page_id);
        }
    }

    fn clear(&self) {
        if let Ok(mut handles) = self.handles.write() {
            handles.clear();
        }
    }

    fn stats(&self) -> usize {
        self.handles.read().map(|h| h.len()).unwrap_or(0)
    }
}

struct NodeCacheEntry {
    node: Node,
    last_access: Instant,
}

pub struct NodeCache {
    nodes: RwLock<HashMap<usize, NodeCacheEntry>>,
    max_size: usize,
}

impl NodeCache {
    fn new(max_size: usize) -> Self {
        Self {
            nodes: RwLock::new(HashMap::new()),
            max_size,
        }
    }

    fn get(&self, page_id: usize) -> Option<Node> {
        let now = Instant::now();
        let mut result = None;
        if let Ok(mut nodes) = self.nodes.write() {
            if let Some(entry) = nodes.get_mut(&page_id) {
                entry.last_access = now;
                result = Some(entry.node.clone());
            }
        }
        result
    }

    fn put(&self, page_id: usize, node: Node) {
        if let Ok(mut nodes) = self.nodes.write() {
            if nodes.len() >= self.max_size {
                if let Some((oldest_id, _)) = nodes
                    .iter()
                    .min_by_key(|(_, entry)| entry.last_access)
                    .map(|(id, entry)| (*id, entry.last_access))
                {
                    nodes.remove(&oldest_id);
                }
            }
            nodes.insert(page_id, NodeCacheEntry { node, last_access: Instant::now() });
        }
    }

    fn invalidate(&self, page_id: usize) {
        if let Ok(mut nodes) = self.nodes.write() {
            nodes.remove(&page_id);
        }
    }

    fn clear(&self) {
        if let Ok(mut nodes) = self.nodes.write() {
            nodes.clear();
        }
    }

    fn stats(&self) -> usize {
        self.nodes.read().map(|n| n.len()).unwrap_or(0)
    }
}

pub struct StripeLockManager {
    stripes: Vec<RwLock<()>>,
    num_stripes: usize,
}

impl StripeLockManager {
    fn new(num_stripes: usize) -> Self {
        let stripes = (0..num_stripes).map(|_| RwLock::new(())).collect();
        Self { stripes, num_stripes }
    }

    fn stripe(&self, page_id: usize) -> usize {
        page_id % self.num_stripes
    }

    fn lock_shared(&self, page_id: usize) -> std::sync::RwLockReadGuard<'_, ()> {
        let idx = self.stripe(page_id);
        self.stripes[idx].read().unwrap()
    }

    fn lock_exclusive(&self, page_id: usize) -> std::sync::RwLockWriteGuard<'_, ()> {
        let idx = self.stripe(page_id);
        self.stripes[idx].write().unwrap()
    }

    fn stats(&self) -> (usize, usize) {
        (self.num_stripes, self.stripes.iter().filter(|s| s.read().is_ok()).count())
    }
}

struct WriteEntry {
    data: Vec<u8>,
    timestamp: Instant,
}

pub struct WriteCoalescer {
    pending: Mutex<HashMap<usize, WriteEntry>>,
    window_ms: u64,
    threshold: usize,
    last_flush: Mutex<Instant>,
}

impl WriteCoalescer {
    fn new(window_ms: u64, threshold: usize) -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            window_ms,
            threshold,
            last_flush: Mutex::new(Instant::now()),
        }
    }

    fn add(&self, page_id: usize, data: Vec<u8>) -> bool {
        let now = Instant::now();
        let should_trigger = {
            let mut pending = self.pending.lock().unwrap();
            pending.insert(page_id, WriteEntry { data, timestamp: now });
            let last = *self.last_flush.lock().unwrap();
            pending.len() >= self.threshold || now.duration_since(last).as_millis() >= self.window_ms as u128
        };
        should_trigger
    }

    fn should_flush(&self) -> bool {
        let now = Instant::now();
        let should = {
            let pending = self.pending.lock().unwrap();
            let last = *self.last_flush.lock().unwrap();
            pending.is_empty() || now.duration_since(last).as_millis() >= self.window_ms as u128
        };
        should
    }

    fn drain(&mut self) -> Vec<(usize, Vec<u8>)> {
        let entries = {
            let mut pending = self.pending.lock().unwrap();
            *self.last_flush.lock().unwrap() = Instant::now();
            pending.drain()
                .map(|(id, entry)| (id, entry.data))
                .collect()
        };
        entries
    }

    fn pending_count(&self) -> usize {
        self.pending.lock().map(|p| p.len()).unwrap_or(0)
    }

    fn clear(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
    }
}

/// 目錄式儲存：使用單一資料夾多檔案結構
///
/// 目錄結構：
/// ```text
/// db.sql6/
///   catalog.meta      # 中繼資料
///   data/             # 資料頁面
///     0.page          # 頁面 0
///     1.page          # 頁面 1
///   wal/              # WAL 日誌
///     wal.log
/// ```
pub struct DirStorage {
    dir_path: std::path::PathBuf,
    data_dir: std::path::PathBuf,
    wal_dir: std::path::PathBuf,
    page_count: usize,
    catalog_root: Option<usize>,
    wal: Wal,
    is_new: bool,
    cache: PageCache,
    bloom: Option<BloomFilter>,
    lock_manager: Arc<StripeLockManager>,
    fd_pool: FileHandleCache,
    node_cache: NodeCache,
    write_coalescer: WriteCoalescer,
}

impl DirStorage {
    /// 開啟或建立目錄式儲存
    pub fn open<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        let dir_path = path.as_ref().to_path_buf();
        let data_dir = dir_path.join("data");
        let wal_dir = dir_path.join("wal");
        
        let is_new = !dir_path.exists();
        
        std::fs::create_dir_all(&dir_path)?;
        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(&wal_dir)?;
        
        let catalog_path = dir_path.join("catalog.meta");
        let (page_count, catalog_root) = if catalog_path.exists() {
            Self::read_catalog(&catalog_path)?
        } else {
            (0, None)
        };
        
        let wal = Wal::open(&dir_path.with_extension("sql6wal"))?;
        
        let mut storage = DirStorage {
            dir_path,
            data_dir: data_dir.clone(),
            wal_dir,
            page_count,
            catalog_root,
            wal,
            is_new,
            cache: PageCache::new(DIR_DEFAULT_CACHE_SIZE),
            bloom: Some(BloomFilter::new(1024 * 1024, 7)),
            lock_manager: Arc::new(StripeLockManager::new(DEFAULT_STRIPE_COUNT)),
            fd_pool: FileHandleCache::new(data_dir),
            node_cache: NodeCache::new(NODE_CACHE_MAX_SIZE),
            write_coalescer: WriteCoalescer::new(WRITE_COALESCE_WINDOW_MS, WRITE_COALESCE_THRESHOLD),
        };
        
        if is_new {
            storage.write_catalog()?;
        }
        
        Ok(storage)
    }

    fn read_catalog(path: &Path) -> std::io::Result<(usize, Option<usize>)> {
        let data = std::fs::read(path)?;
        if data.len() < 8 { return Ok((0, None)); }
        let page_count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let catalog_root = if data.len() >= 8 && data[4] != 0 || data.len() > 8 {
            Some(u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize)
        } else { None };
        Ok((page_count, catalog_root))
    }

    fn write_catalog(&self) -> std::io::Result<()> {
        let catalog_path = self.dir_path.join("catalog.meta");
        let mut data = vec![0u8; 16];
        data[0..4].copy_from_slice(&(self.page_count as u32).to_le_bytes());
        if let Some(root) = self.catalog_root {
            data[4..8].copy_from_slice(&(root as u32).to_le_bytes());
        }
        std::fs::write(catalog_path, &data)
    }

    fn page_path(&self, page_id: usize) -> std::path::PathBuf {
        self.data_dir.join(format!("{}.page", page_id))
    }

    /// 從磁碟讀取（不用快取）
    fn read_page_from_disk(&self, page_id: usize) -> std::io::Result<Vec<u8>> {
        let path = self.page_path(page_id);
        if path.exists() {
            let mut file = self.fd_pool.get_file(page_id)?;
            let mut data = Vec::new();
            file.read_to_end(&mut data)?;
            Ok(data)
        } else if let Some(data) = self.wal.read_page(page_id as u32) {
            Ok(data.to_vec())
        } else {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "page not found"))
        }
    }

    /// 讀取頁面（含快取+鎖）
    fn read_page(&self, page_id: usize) -> std::io::Result<Vec<u8>> {
        if let Some(data) = self.cache.get(page_id) {
            return Ok(data);
        }
        let _lock = self.lock_manager.lock_shared(page_id);
        let data = self.read_page_from_disk(page_id)?;
        self.cache.put(page_id, data.clone());
        Ok(data)
    }

    /// 寫入頁面（含快取失效+鎖）
    fn write_page(&mut self, page_id: usize, data: &[u8]) -> std::io::Result<()> {
        let _lock = self.lock_manager.lock_exclusive(page_id);
        let path = self.page_path(page_id);
        if page_id >= self.page_count { self.page_count = page_id + 1; }
        self.cache.invalidate(page_id);
        self.fd_pool.invalidate(page_id);
        self.node_cache.invalidate(page_id);
        self.wal.write_page(page_id as u32, data.to_vec());
        if !self.wal.in_txn() {
            std::fs::write(&path, data)?;
            self.cache.put(page_id, data.to_vec());
        }
        Ok(())
    }

    pub fn cache_stats(&self) -> (usize, usize, usize) { self.cache.stats() }
    pub fn fd_pool_stats(&self) -> usize { self.fd_pool.stats() }
    pub fn node_cache_stats(&self) -> usize { self.node_cache.stats() }
    pub fn stripe_stats(&self) -> (usize, usize) { self.lock_manager.stats() }
    pub fn write_coalescer_pending(&self) -> usize { self.write_coalescer.pending_count() }
    pub fn clear_cache(&self) { self.cache.clear(); self.fd_pool.clear(); self.node_cache.clear(); self.write_coalescer.clear(); }
    pub fn bloom_insert(&mut self, key: &[u8]) { if let Some(ref mut b) = self.bloom { b.insert(key); } }
    pub fn bloom_might_contain(&self, key: &[u8]) -> bool { self.bloom.as_ref().map(|b| b.might_contain(key)).unwrap_or(true) }

    /// 批量讀取多個頁面（平行）
    pub fn read_pages(&self, page_ids: &[usize]) -> Vec<Option<Vec<u8>>> {
        page_ids.par_iter().map(|&id| self.read_page(id).ok()).collect()
    }

    /// 讀取頁面範圍（優化全表掃描，平行）
    pub fn read_page_range(&self, start: usize, count: usize) -> Vec<Option<Vec<u8>>> {
        (start..start + count).into_par_iter().map(|id| self.read_page(id).ok()).collect()
    }

    /// 預取頁面範圍（快取優化）
    pub fn prefetch_range(&self, start: usize, count: usize) {
        self.cache.prefetch_range(start, count, |id| self.read_page_from_disk(id).ok());
    }

    /// 預取特定頁面
    pub fn prefetch(&self, page_id: usize) {
        if let Ok(data) = self.read_page_from_disk(page_id) {
            self.cache.prefetch(page_id, data);
        }
    }

    /// 儲存熱門頁面（關閉時呼叫）
    pub fn save_hot_pages(&self) -> std::io::Result<()> {
        self.cache.save_hot_pages(&self.dir_path)
    }

    /// 預熱快取（開啟時呼叫）
    pub fn warm_up_cache(&self) -> std::io::Result<()> {
        self.cache.warm_up(&self.dir_path)
    }

    /// 取得熱門頁面列表
    pub fn get_hot_pages(&self) -> Vec<usize> {
        self.cache.get_hot_page_list()
    }

/// 非同步 checkpoint（在後台執行）
    /// 注意：這是一個簡單的實現，完整實現需要更複雜的狀態管理
    pub fn async_checkpoint(&self) {
        // 記錄需要 checkpoint，在下次 flush 時觸發
        // 完整實現需要獨立的背景執行緒
    }

    /// 檢查是否需要 checkpoint
    pub fn needs_checkpoint(&self) -> bool {
        self.wal.frame_count() > 1000 // 閾值
    }

    /// 觸發 checkpoint（同步）
    pub fn maybe_checkpoint(&mut self) {
        if self.needs_checkpoint() {
            self.flush();
        }
    }

    /// 批次寫入（減少 I/O）
    pub fn write_batch(&mut self, pages: Vec<(usize, Vec<u8>)>) {
        let wal_pages: Vec<(u32, Vec<u8>)> = pages.into_iter()
            .map(|(id, data)| (id as u32, data))
            .collect();
        self.wal.write_batch(wal_pages);
    }
}

impl Storage for DirStorage {
    fn read_node(&mut self, page_id: usize) -> Node {
        if let Some(node) = self.node_cache.get(page_id) {
            return node;
        }
        if let Some(data) = self.wal.read_page(page_id as u32) {
            let node = decode_node(data);
            self.node_cache.put(page_id, node.clone());
            return node;
        }
        let data = self.read_page(page_id).expect("page not found");
        let node = decode_node(&data);
        self.node_cache.put(page_id, node.clone());
        node
    }

    fn write_node(&mut self, page_id: usize, node: &Node) {
        let buf = encode_node(node);
        if self.wal.in_txn() {
            if let Ok(data) = self.read_page_from_disk(page_id) {
                self.wal.save_original(page_id as u32, data);
            } else if let Some(data) = self.wal.read_page(page_id as u32) {
                self.wal.save_original(page_id as u32, data.to_vec());
            }
        }
        self.node_cache.invalidate(page_id);
        self.wal.write_page(page_id as u32, buf);
    }

    fn alloc_page(&mut self) -> usize {
        let id = self.page_count;
        self.page_count += 1;
        self.wal.write_page(id as u32, vec![0u8; PAGE_SIZE]);
        let _ = self.write_catalog();
        id
    }

    fn page_count(&self) -> usize { self.page_count }

    fn flush(&mut self) {
        if self.write_coalescer.should_flush() || self.wal.frame_count() > 0 {
            let pending = self.write_coalescer.drain();
            if !pending.is_empty() {
                for (page_id, data) in pending {
                    let path = self.data_dir.join(format!("{}.page", page_id));
                    let _ = std::fs::write(&path, &data);
                }
            }
        }
        if self.wal.frame_count() > 0 {
            let data_dir = self.data_dir.clone();
            self.wal.checkpoint(|page_id, data| {
                let path = data_dir.join(format!("{}.page", page_id));
                std::fs::write(&path, data)
            }).unwrap();
        }
        let _ = self.write_catalog();
    }

    fn begin_txn(&mut self)    { self.wal.begin(); }
    fn commit_txn(&mut self)   { self.wal.commit().unwrap(); }
    fn rollback_txn(&mut self) { self.wal.rollback(); self.cache.clear(); self.node_cache.clear(); self.write_coalescer.clear(); }
    fn catalog_root(&self) -> Option<usize> { self.catalog_root }
    fn set_catalog_root(&mut self, root: usize) { self.catalog_root = Some(root); let _ = self.write_catalog(); }
    fn is_wal(&self) -> bool { true }
}

// ── 測試 ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btree::node::{Key, Node, Record};

    fn leaf_with(key: i64, val: &str) -> Node {
        let mut node = Node::new_leaf();
        node.keys.push(Key::Integer(key));
        node.records.push(Record { key: Key::Integer(key), value: val.as_bytes().to_vec() });
        node
    }

    fn cleanup(name: &str) {
        let _ = std::fs::remove_dir_all(format!("/tmp/sql6_{}", name));
    }

    #[test]
    fn dir_storage_create() {
        cleanup("dir_create");
        let path = "/tmp/sql6_dir_create";
        let store = DirStorage::open(path).unwrap();
        assert_eq!(store.page_count(), 0);
        cleanup("dir_create");
    }

    #[test]
    fn dir_storage_write_read() {
        cleanup("dir_rw");
        let path = "/tmp/sql6_dir_rw";
        
        let page_id: usize;
        {
            let mut store = DirStorage::open(path).unwrap();
            store.begin_txn();
            page_id = store.alloc_page();
            store.write_node(page_id, &leaf_with(99, "world"));
            store.commit_txn();
            store.flush();
        }
        
        {
            let mut store = DirStorage::open(path).unwrap();
            let node = store.read_node(page_id);
            assert_eq!(node.keys[0], Key::Integer(99));
            assert_eq!(node.records[0].value, b"world");
        }
        
        cleanup("dir_rw");
    }

    #[test]
    fn dir_storage_rollback() {
        cleanup("dir_rollback");
        let path = "/tmp/sql6_dir_rollback";
        
        {
            let mut store = DirStorage::open(path).unwrap();
            store.begin_txn();
            let id = store.alloc_page();
            store.write_node(id, &leaf_with(1, "committed"));
            store.commit_txn();
            store.flush();
            
            store.begin_txn();
            store.write_node(id, &leaf_with(1, "should_be_gone"));
            store.rollback_txn();
            
            let node = store.read_node(id);
            assert_eq!(node.records[0].value, b"committed");
        }
        
        cleanup("dir_rollback");
    }

    #[test]
    fn dir_storage_catalog() {
        cleanup("dir_catalog");
        let path = "/tmp/sql6_dir_catalog";
        
        {
            let mut store = DirStorage::open(path).unwrap();
            store.set_catalog_root(42);
            store.flush();
        }
        
        {
            let store = DirStorage::open(path).unwrap();
            assert_eq!(store.catalog_root(), Some(42));
        }
        
        cleanup("dir_catalog");
    }

    #[test]
    fn page_cache_test() {
        cleanup("dir_cache");
        let path = "/tmp/sql6_dir_cache";
        
        let store = DirStorage::open(path).unwrap();
        let (len, size, max) = store.cache_stats();
        assert_eq!(len, 0);
        assert!(size < max);
        
        cleanup("dir_cache");
    }

    #[test]
    fn bloom_filter_test() {
        let mut bf = BloomFilter::new(1024, 3);
        bf.insert(b"hello");
        assert!(bf.might_contain(b"hello"));
        assert!(!bf.might_contain(b"world"));
    }
}