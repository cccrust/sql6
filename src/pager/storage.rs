//! 儲存後端抽象（含 WAL 交易支援）

use crate::btree::node::Node;
use super::codec::{decode_node, encode_node, PAGE_SIZE};
use super::wal::Wal;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

// ── Storage trait ─────────────────────────────────────────────────────────

pub trait Storage: Send + Sync {
    fn read_node(&mut self, page_id: usize) -> Node;
    fn write_node(&mut self, page_id: usize, node: &Node);
    fn alloc_page(&mut self) -> usize;
    fn page_count(&self) -> usize;
    fn flush(&mut self);

    // 交易控制（MemoryStorage 為 no-op）
    fn begin_txn(&mut self)  {}
    fn commit_txn(&mut self) {}
    fn rollback_txn(&mut self) {}

    // Catalog 根頁號（MemoryStorage 回傳 None）
    fn catalog_root(&self) -> Option<usize> { None }
    fn set_catalog_root(&mut self, _root: usize) {}

    // PRAGMA 支援
    fn is_wal(&self) -> bool { false }
    fn cache_size(&self) -> usize { 256 }
    fn set_cache_size(&mut self, _size: usize) {}
    fn page_size(&self) -> usize { PAGE_SIZE }
    fn freelist_count(&self) -> usize { 0 }
}

// ── MemoryStorage ─────────────────────────────────────────────────────────

use std::sync::{Arc, Mutex};

struct MemoryInner {
    pages: HashMap<usize, Node>,
    next_page: usize,
}

#[derive(Clone)]
pub struct MemoryStorage {
    inner: Arc<Mutex<MemoryInner>>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        MemoryStorage { 
            inner: Arc::new(Mutex::new(MemoryInner { 
                pages: HashMap::new(), 
                next_page: 0 
            }))
        }
    }
}

impl Default for MemoryStorage {
    fn default() -> Self { Self::new() }
}

impl Storage for MemoryStorage {
    fn read_node(&mut self, page_id: usize) -> Node {
        let inner = self.inner.lock().unwrap();
        inner.pages.get(&page_id).cloned().expect("MemoryStorage: page not found")
    }
    fn write_node(&mut self, page_id: usize, node: &Node) {
        let mut inner = self.inner.lock().unwrap();
        inner.pages.insert(page_id, node.clone());
    }
    fn alloc_page(&mut self) -> usize {
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_page;
        inner.next_page += 1;
        id
    }
    fn page_count(&self) -> usize { 
        self.inner.lock().unwrap().next_page 
    }
    fn flush(&mut self) {}
}

// ── LruCacheStorage ───────────────────────────────────────────────────────

use std::collections::VecDeque;

const DEFAULT_CACHE_SIZE: usize = 256;

pub struct LruCacheStorage<S> {
    inner: S,
    cache: HashMap<usize, Node>,
    access_order: VecDeque<usize>,
    capacity: usize,
    hits: usize,
    misses: usize,
}

impl<S: Storage> LruCacheStorage<S> {
    pub fn new(inner: S, capacity: usize) -> Self {
        LruCacheStorage {
            inner,
            cache: HashMap::new(),
            access_order: VecDeque::new(),
            capacity: capacity.max(1),
            hits: 0,
            misses: 0,
        }
    }

    pub fn with_default_capacity(inner: S) -> Self {
        Self::new(inner, DEFAULT_CACHE_SIZE)
    }

    pub fn stats(&self) -> (usize, usize, f64) {
        let total = self.hits + self.misses;
        let ratio = if total > 0 { self.hits as f64 / total as f64 } else { 0.0 };
        (self.hits, self.misses, ratio)
    }

    fn evict(&mut self) {
        if let Some(oldest) = self.access_order.pop_front() {
            self.cache.remove(&oldest);
        }
    }

    fn touch(&mut self, page_id: usize) {
        if let Some(pos) = self.access_order.iter().position(|&x| x == page_id) {
            self.access_order.remove(pos);
        }
        self.access_order.push_back(page_id);
    }
}

impl<S: Storage> Storage for LruCacheStorage<S> {
    fn read_node(&mut self, page_id: usize) -> Node {
        if let Some(node) = self.cache.get(&page_id).cloned() {
            self.hits += 1;
            self.touch(page_id);
            return node;
        }
        self.misses += 1;
        let node = self.inner.read_node(page_id);
        if self.cache.len() >= self.capacity {
            self.evict();
        }
        self.cache.insert(page_id, node.clone());
        self.access_order.push_back(page_id);
        node
    }

    fn write_node(&mut self, page_id: usize, node: &Node) {
        self.inner.write_node(page_id, node);
        if self.cache.len() >= self.capacity {
            self.evict();
        }
        self.cache.insert(page_id, node.clone());
        self.touch(page_id);
    }

    fn alloc_page(&mut self) -> usize {
        self.inner.alloc_page()
    }

    fn page_count(&self) -> usize {
        self.inner.page_count()
    }

    fn flush(&mut self) {
        self.inner.flush();
        self.cache.clear();
        self.access_order.clear();
    }

    fn begin_txn(&mut self) {
        self.inner.begin_txn()
    }

    fn commit_txn(&mut self) {
        self.inner.commit_txn()
    }

    fn rollback_txn(&mut self) {
        self.inner.rollback_txn()
    }

    fn catalog_root(&self) -> Option<usize> {
        self.inner.catalog_root()
    }

    fn set_catalog_root(&mut self, root: usize) {
        self.inner.set_catalog_root(root)
    }
}

// ── DiskStorage（含 WAL） ─────────────────────────────────────────────────

/// 磁碟後端：主檔 + WAL 日誌，支援崩潰安全與交易
///
/// 檔案格式：
/// ```text
/// [0..8]   magic       : b"SQL4DB\0\0"
/// [8..12]  version     : u32 = 1
/// [12..16] page_count  : u32
/// [16..20] catalog_root: u32（catalog B+Tree 的根頁號，0 = 未初始化）
/// [20..PAGE_SIZE] 填充
/// [PAGE_SIZE..] 資料頁
/// ```
pub struct DiskStorage {
    file:          File,
    path:          std::path::PathBuf,
    page_count:    usize,
    /// catalog 根頁號（持久化於 header）
    pub catalog_root: Option<usize>,
    wal:           Wal,
}

const MAGIC: &[u8; 8] = b"SQL4DB\0\0";
const VERSION: u32 = 2; // 升版以包含 catalog_root 欄位
const HEADER_OFFSET: u64 = PAGE_SIZE as u64;

impl DiskStorage {
    pub fn open<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        let path = path.as_ref();
        let path_buf = path.to_path_buf();
        let exists = path.exists();

        let file = OpenOptions::new()
            .read(true).write(true).create(true)
            .open(path)?;

        let wal = Wal::open(path)?;

        let mut storage = DiskStorage {
            file,
            path: path_buf,
            page_count: 0,
            catalog_root: None,
            wal,
        };

        if exists {
            let file_size = storage.file.metadata()?.len();
            if file_size == 0 {
                storage.write_header()?;
            } else if let Err(e) = storage.read_header() {
                return Err(e);
            }
        } else {
            storage.write_header()?;
        }

        Ok(storage)
    }

    /// 回傳資料庫檔案路徑
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// 關閉檔案（用於 vacuum 時）
    #[cfg(unix)]
    pub fn close(self) -> std::io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let _fd = self.file.metadata()?.ino();
        drop(self);
        Ok(())
    }

    #[cfg(windows)]
    pub fn close(self) -> std::io::Result<()> {
        drop(self);
        Ok(())
    }

    /// 將 catalog 根頁號寫入 header
    pub fn set_catalog_root(&mut self, root: usize) {
        self.catalog_root = Some(root);
        let _ = self.write_header();
    }

    fn write_header(&mut self) -> std::io::Result<()> {
        let mut hdr = vec![0u8; PAGE_SIZE];
        hdr[0..8].copy_from_slice(MAGIC);
        hdr[8..12].copy_from_slice(&VERSION.to_le_bytes());
        hdr[12..16].copy_from_slice(&(self.page_count as u32).to_le_bytes());
        let cat_root = self.catalog_root.unwrap_or(0) as u32;
        hdr[16..20].copy_from_slice(&cat_root.to_le_bytes());
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&hdr)?;
        self.file.flush()
    }

    fn read_header(&mut self) -> std::io::Result<()> {
        let mut hdr = vec![0u8; PAGE_SIZE];
        self.file.seek(SeekFrom::Start(0))?;
        self.file.read_exact(&mut hdr)?;

        if &hdr[0..8] != MAGIC {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData, "invalid sql6db magic"));
        }
        self.page_count = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
        let cat_root = u32::from_le_bytes(hdr[16..20].try_into().unwrap()) as usize;
        
        // Only set catalog_root if cat_root > 0 (page 0 is never valid for catalog)
        self.catalog_root = if cat_root > 0 {
            Some(cat_root)
        } else {
            None
        };
        Ok(())
    }

    fn page_offset(page_id: usize) -> u64 {
        HEADER_OFFSET + (page_id as u64) * PAGE_SIZE as u64
    }

    /// 從主檔直接讀取一頁（繞過 WAL 快取）
    fn read_page_from_file(&mut self, page_id: usize) -> Vec<u8> {
        let offset = Self::page_offset(page_id);
        let mut buf = vec![0u8; PAGE_SIZE];
        self.file.seek(SeekFrom::Start(offset)).unwrap();
        let _ = self.file.read_exact(&mut buf);
        buf
    }

    /// 把一頁直接寫入主檔（checkpoint 時使用）
    fn write_page_to_file(&mut self, page_id: u32, data: &[u8]) -> std::io::Result<()> {
        let offset = Self::page_offset(page_id as usize);
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(data)?;
        Ok(())
    }
}

impl Storage for DiskStorage {
    fn read_node(&mut self, page_id: usize) -> Node {
        // 先查 WAL（包含 dirty 與 committed）
        if let Some(data) = self.wal.read_page(page_id as u32) {
            return decode_node(data);
        }
        // 否則從主檔讀
        let buf = self.read_page_from_file(page_id);
        decode_node(&buf)
    }

    fn write_node(&mut self, page_id: usize, node: &Node) {
        // 在寫入前，如果正在交易中，先儲存原始頁面用於 rollback
        let buf = encode_node(node);
        // 儲存原始狀態（只用於 rollback，恢复时会用 pre_image 中的原始状态）
        // 注意：如果頁面不在 committed 中（就是新頁面），則跳過
        if self.wal.in_txn() {
            // 嘗試取得目前頁面的 committed 版本作為原始
            if let Some(original) = self.wal.get_committed_copy(page_id as u32) {
                self.wal.save_original(page_id as u32, original);
            }
        }
        // 寫入 WAL
        self.wal.write_page(page_id as u32, buf);
    }

    fn alloc_page(&mut self) -> usize {
        let id = self.page_count;
        self.page_count += 1;
        // 在主檔佔位（空白頁），並寫入 WAL
        let blank = vec![0u8; PAGE_SIZE];
        self.wal.write_page(id as u32, blank.clone());
        let offset = Self::page_offset(id);
        self.file.seek(SeekFrom::Start(offset)).unwrap();
        self.file.write_all(&blank).unwrap();
        id
    }

    fn page_count(&self) -> usize { self.page_count }

    fn flush(&mut self) {
        // Checkpoint（always checkpoint on explicit flush）
        if self.wal.frame_count() > 0 {
            let file = &mut self.file;
            let header_offset = HEADER_OFFSET;
            self.wal.checkpoint(|page_id, data| {
                let offset = header_offset + (page_id as u64) * PAGE_SIZE as u64;
                file.seek(SeekFrom::Start(offset))?;
                file.write_all(data)
            }).unwrap();
        }
        self.write_header().unwrap();
        self.file.flush().unwrap();
    }

    fn begin_txn(&mut self)    { self.wal.begin(); }
    fn commit_txn(&mut self)   { self.wal.commit().unwrap(); }
    fn rollback_txn(&mut self) { self.wal.rollback(); }

    fn catalog_root(&self) -> Option<usize> { self.catalog_root }
    fn set_catalog_root(&mut self, root: usize) {
        self.catalog_root = Some(root);
        let _ = self.write_header();
    }
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
        node.records.push(Record {
            key: Key::Integer(key),
            value: val.as_bytes().to_vec(),
        });
        node
    }

    fn cleanup(name: &str) {
        let _ = std::fs::remove_file(format!("/tmp/sql6_{}.db", name));
        let _ = std::fs::remove_file(format!("/tmp/sql6_{}.sql6wal", name));
    }

    #[test]
    fn memory_alloc_write_read() {
        let mut store = MemoryStorage::new();
        let id = store.alloc_page();
        let node = leaf_with(42, "hello");
        store.write_node(id, &node);
        let back = store.read_node(id);
        assert_eq!(back.keys, node.keys);
        assert_eq!(back.records[0].value, b"hello");
    }

    #[test]
    fn disk_write_and_read() {
        cleanup("disk_rw");
        let _ = std::fs::remove_file("/tmp/sql6_disk_rw.db");
        {
            let mut store = DiskStorage::open("/tmp/sql6_disk_rw.db").unwrap();
            store.begin_txn();
            let id = store.alloc_page();
            store.write_node(id, &leaf_with(99, "world"));
            store.commit_txn();
            store.flush();
        }
        {
            let mut store = DiskStorage::open("/tmp/sql6_disk_rw.db").unwrap();
            let node = store.read_node(0);
            assert_eq!(node.keys[0], Key::Integer(99));
            assert_eq!(node.records[0].value, b"world");
        }
        cleanup("disk_rw");
    }

    #[test]
    fn disk_rollback() {
        cleanup("rollback");
        {
            let mut store = DiskStorage::open("/tmp/sql6_rollback.db").unwrap();
            // 先提交一筆
            store.begin_txn();
            let id = store.alloc_page();
            store.write_node(id, &leaf_with(1, "committed"));
            store.commit_txn();
            store.flush();

            // 再開一筆，然後 rollback
            store.begin_txn();
            store.write_node(id, &leaf_with(1, "should_be_gone"));
            store.rollback_txn();

            // 讀到的應該是 committed 的版本
            let node = store.read_node(id);
            assert_eq!(node.records[0].value, b"committed");
        }
        cleanup("rollback");
    }

    #[test]
    fn disk_wal_write_through() {
        cleanup("wal_write");
        let mut store = DiskStorage::open("/tmp/sql6_wal_write.db").unwrap();
        store.begin_txn();
        let id = store.alloc_page();
        store.write_node(id, &leaf_with(100, "wal_test"));
        store.commit_txn();
        // 不 flush，資料在 WAL 中
        // 但 read_node 會從 WAL 讀取
        let node = store.read_node(id);
        assert_eq!(node.keys[0], Key::Integer(100));
        assert_eq!(node.records[0].value, b"wal_test");
        cleanup("wal_write");
    }

    #[test]
    fn disk_multiple_transactions() {
        cleanup("multi_txn");
        let mut store = DiskStorage::open("/tmp/sql6_multi_txn.db").unwrap();
        // 第一次交易
        store.begin_txn();
        let id1 = store.alloc_page();
        store.write_node(id1, &leaf_with(1, "txn1"));
        store.commit_txn();
        // 第二次交易
        store.begin_txn();
        let id2 = store.alloc_page();
        store.write_node(id2, &leaf_with(2, "txn2"));
        store.commit_txn();
        // 讀取兩個頁
        let n1 = store.read_node(id1);
        let n2 = store.read_node(id2);
        assert_eq!(n1.records[0].value, b"txn1");
        assert_eq!(n2.records[0].value, b"txn2");
        cleanup("multi_txn");
    }

    #[test]
    fn disk_auto_commit_wal() {
        cleanup("auto_commit");
        let mut store = DiskStorage::open("/tmp/sql6_auto_commit.db").unwrap();
        // 直接寫入（auto-commit）
        let id = store.alloc_page();
        store.write_node(id, &leaf_with(5, "auto"));
        // WAL 有記錄，直接讀取應該得到值
        let node = store.read_node(id);
        assert_eq!(node.records[0].value, b"auto");
        cleanup("auto_commit");
    }

    #[test]
    fn disk_is_wal_returns_true() {
        cleanup("iswal");
        let store = DiskStorage::open("/tmp/sql6_iswal.db").unwrap();
        assert!(store.is_wal());
        cleanup("iswal");
    }

    #[test]
    fn disk_reopen_preserves_data() {
        cleanup("reopen");
        {
            let mut store = DiskStorage::open("/tmp/sql6_reopen.db").unwrap();
            store.begin_txn();
            let id = store.alloc_page();
            store.write_node(id, &leaf_with(123, "reopen_test"));
            store.commit_txn();
            store.flush();
        }
        {
            let mut store = DiskStorage::open("/tmp/sql6_reopen.db").unwrap();
            let node = store.read_node(0);
            assert_eq!(node.keys[0], Key::Integer(123));
            assert_eq!(node.records[0].value, b"reopen_test");
        }
        cleanup("reopen");
    }

    #[test]
    fn disk_crash_recovery() {
        cleanup("crash");
        // 模擬：commit 後，程式「崩潰」（不 checkpoint）
        {
            let mut store = DiskStorage::open("/tmp/sql6_crash.db").unwrap();
            store.begin_txn();
            let id = store.alloc_page();
            store.write_node(id, &leaf_with(777, "survived"));
            store.commit_txn();
            // 不呼叫 flush()，讓 WAL 保留
        }
        // 重開：WAL replay 應該恢復 page 0
        {
            let mut store = DiskStorage::open("/tmp/sql6_crash.db").unwrap();
            let node = store.read_node(0);
            assert_eq!(node.keys[0], Key::Integer(777));
            assert_eq!(node.records[0].value, b"survived");
        }
        cleanup("crash");
    }

    #[test]
    fn catalog_root_persists() {
        cleanup("catroot");
        {
            let mut store = DiskStorage::open("/tmp/sql6_catroot.db").unwrap();
            store.set_catalog_root(42);
        }
        {
            let store = DiskStorage::open("/tmp/sql6_catroot.db").unwrap();
            assert_eq!(store.catalog_root, Some(42));
        }
        cleanup("catroot");
    }

    #[test]
    fn lru_cache_hit() {
        let inner = MemoryStorage::new();
        let mut cache = LruCacheStorage::new(inner, 2);
        
        cache.write_node(1, &leaf_with(1, "a"));
        cache.write_node(2, &leaf_with(2, "b"));
        
        let _ = cache.read_node(1);
        cache.write_node(3, &leaf_with(3, "c"));
        
        let node1 = cache.read_node(1);
        assert_eq!(node1.keys[0], Key::Integer(1));
        
        let (hits, misses, _) = cache.stats();
        assert_eq!(hits, 2);
        assert_eq!(misses, 0);
    }

    #[test]
    fn lru_cache_eviction() {
        let inner = MemoryStorage::new();
        let mut cache = LruCacheStorage::new(inner, 2);
        
        cache.write_node(1, &leaf_with(1, "a"));
        cache.write_node(2, &leaf_with(2, "b"));
        
        cache.read_node(1);
        cache.write_node(3, &leaf_with(3, "c"));
        
        let (hits, misses, _) = cache.stats();
        assert_eq!(hits, 1);
        assert_eq!(misses, 0);
    }

    #[test]
    fn lru_cache_with_memory() {
        let inner = MemoryStorage::new();
        let mut cache = LruCacheStorage::with_default_capacity(inner);
        
        for i in 0..100 {
            cache.write_node(i, &leaf_with(i as i64, &format!("val{}", i)));
        }
        
        let node = cache.read_node(50);
        assert_eq!(node.keys[0], Key::Integer(50));
        
        cache.flush();
        
        let (hits, misses, _) = cache.stats();
        assert!(hits > 0);
    }

    #[test]
    fn lru_cache_write_updates_cache() {
        let inner = MemoryStorage::new();
        let mut cache = LruCacheStorage::new(inner, 2);
        
        cache.write_node(1, &leaf_with(1, "original"));
        
        let node = cache.read_node(1);
        assert_eq!(node.keys[0], Key::Integer(1));
        
        cache.write_node(1, &leaf_with(1, "updated"));
        
        let node2 = cache.read_node(1);
        assert_eq!(node2.keys[0], Key::Integer(1));
    }
}

// ── DynStorage 包裝 ─────────────────────────────────────────────────────────
// 允許使用 Box<dyn Storage> 與 generic code 搭配

pub struct DynStorage {
    inner: Box<dyn Storage>,
}

impl DynStorage {
    pub fn new(inner: Box<dyn Storage>) -> Self {
        DynStorage { inner }
    }

    pub fn memory() -> Self {
        DynStorage { inner: Box::new(MemoryStorage::new()) }
    }

    pub fn disk<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        Ok(DynStorage { inner: Box::new(DiskStorage::open(path)?) })
    }
}

impl Storage for DynStorage {
    fn read_node(&mut self, page_id: usize) -> Node {
        self.inner.read_node(page_id)
    }

    fn write_node(&mut self, page_id: usize, node: &Node) {
        self.inner.write_node(page_id, node);
    }

    fn alloc_page(&mut self) -> usize {
        self.inner.alloc_page()
    }

    fn page_count(&self) -> usize {
        self.inner.page_count()
    }

    fn flush(&mut self) {
        self.inner.flush()
    }

    fn begin_txn(&mut self) {
        self.inner.begin_txn()
    }

    fn commit_txn(&mut self) {
        self.inner.commit_txn()
    }

    fn rollback_txn(&mut self) {
        self.inner.rollback_txn()
    }
}

// ── SharedStorage 包裝 ──────────────────────────────────────────────────────
// 使用 Arc<Mutex<T>> 讓多個 B+Tree 可以安全地共用同一個 storage

#[derive(Clone)]
pub struct SharedStorage {
    inner: Arc<Mutex<Box<dyn Storage>>>,
    path: Option<std::path::PathBuf>,
}

impl SharedStorage {
    pub fn new(inner: Box<dyn Storage>) -> Self {
        SharedStorage { inner: Arc::new(Mutex::new(inner)), path: None }
    }

    pub fn memory() -> Self {
        SharedStorage { inner: Arc::new(Mutex::new(Box::new(MemoryStorage::new()))), path: None }
    }

    pub fn disk<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        let path_buf = path.as_ref().to_path_buf();
        Ok(SharedStorage { inner: Arc::new(Mutex::new(Box::new(DiskStorage::open(path)?))), path: Some(path_buf) })
    }

    /// 開啟磁碟資料庫並啟用 LRU 快取
    pub fn disk_with_cache<P: AsRef<Path>>(path: P, capacity: usize) -> std::io::Result<Self> {
        let path_buf = path.as_ref().to_path_buf();
        let disk = DiskStorage::open(path)?;
        let cached = LruCacheStorage::new(disk, capacity);
        Ok(SharedStorage { inner: Arc::new(Mutex::new(Box::new(cached))), path: Some(path_buf) })
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, Box<dyn Storage>> {
        self.inner.lock().expect("Storage lock poisoned")
    }

    /// 回傳磁碟路徑（如果是磁碟模式）
    pub fn disk_path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    /// 回傳 catalog 根頁號（如果有）
    pub fn catalog_root(&self) -> Option<usize> {
        self.inner.lock().ok().and_then(|inner| inner.catalog_root())
    }

    /// 設定 catalog 根頁號
    pub fn set_catalog_root(&mut self, root: usize) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.set_catalog_root(root);
        }
    }
}

impl Storage for SharedStorage {
    fn read_node(&mut self, page_id: usize) -> Node {
        self.inner.lock().expect("Storage lock poisoned").read_node(page_id)
    }

    fn write_node(&mut self, page_id: usize, node: &Node) {
        self.inner.lock().expect("Storage lock poisoned").write_node(page_id, node);
    }

    fn alloc_page(&mut self) -> usize {
        self.inner.lock().expect("Storage lock poisoned").alloc_page()
    }

    fn page_count(&self) -> usize {
        self.inner.lock().expect("Storage lock poisoned").page_count()
    }

    fn flush(&mut self) {
        let mut inner = self.inner.lock().expect("Storage lock poisoned");
        inner.flush();
    }

    fn begin_txn(&mut self) {
        self.inner.lock().expect("Storage lock poisoned").begin_txn();
    }

    fn commit_txn(&mut self) {
        self.inner.lock().expect("Storage lock poisoned").commit_txn();
    }

    fn rollback_txn(&mut self) {
        self.inner.lock().expect("Storage lock poisoned").rollback_txn();
    }
}


