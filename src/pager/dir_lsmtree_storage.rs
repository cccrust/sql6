//! LSM Tree 儲存引擎
//!
//! 提供 Log-Structured Merge-Tree 實現，適合寫入密集的工作負載。

use crate::btree::node::Node;
use crate::pager::storage::Storage;
use super::codec::{decode_node, encode_node, PAGE_SIZE};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// LSM Tree 儲存引擎
pub struct LsmStorage {
    dir_path: PathBuf,
    mem_table: BTreeMap<u64, Vec<u8>>,  // 記憶體表
    l0_tables: Vec<SSTable>,             // L0 層
    l1_tables: Vec<SSTable>,             // L1 層
    l2_tables: Vec<SSTable>,             // L2 層
    next_file_id: u64,
    page_count: usize,
    mem_size: usize,         // memtable 當前大小
    mem_threshold: usize,   // flush 閾值
}

impl LsmStorage {
    /// 建立或開啟 LSM Tree 儲存
    pub fn open<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        let dir_path = path.as_ref().to_path_buf();
        let data_dir = dir_path.join("lsm");
        fs::create_dir_all(&data_dir)?;
        
        // 讀取當前最大 file_id
        let next_file_id = Self::find_max_file_id(&data_dir) + 1;
        
        let mut storage = LsmStorage {
            dir_path: data_dir,
            mem_table: BTreeMap::new(),
            l0_tables: Vec::new(),
            l1_tables: Vec::new(),
            l2_tables: Vec::new(),
            next_file_id,
            page_count: 0,
            mem_size: 0,
            mem_threshold: 1024 * 1024,  // 1MB default
        };
        
        // 載入現有 SSTable
        storage.load_existing_tables()?;
        
        Ok(storage)
    }

    /// 找出最大檔案 ID
    fn find_max_file_id(dir: &Path) -> u64 {
        let mut max_id = 0u64;
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if let Some(s) = name.to_str() {
                    if s.starts_with("sst_") && s.ends_with(".data") {
                        if let Ok(id) = s[4..s.len()-5].parse::<u64>() {
                            max_id = max_id.max(id);
                        }
                    }
                }
            }
        }
        max_id
    }

    /// 載入現有的 SSTable
    fn load_existing_tables(&mut self) -> std::io::Result<()> {
        // 掃描目錄，載入所有 SSTable
        if let Ok(entries) = fs::read_dir(&self.dir_path) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if let Some(s) = name.to_str() {
                    if s.starts_with("sst_") && s.ends_with(".data") {
                        let file_id: u64 = s[4..s.len()-5].parse().unwrap_or(0);
                        let sst = SSTable::open(file_id, &self.dir_path)?;
                        // 根據層級放入不同向量
                        // 這裡簡化處理，都放 l0_tables
                        self.l0_tables.push(sst);
                    }
                }
            }
        }
        Ok(())
    }

    /// 將 mem_table flush 到磁碟
    fn flush(&mut self) -> std::io::Result<()> {
        if self.mem_table.is_empty() {
            return Ok(());
        }

        let file_id = self.next_file_id;
        self.next_file_id += 1;

        // 建立 SSTable
        let sst = SSTable::create(file_id, &self.dir_path, &self.mem_table)?;
        
        self.l0_tables.push(sst);
        
        // 清空 mem_table
        self.mem_table.clear();
        self.mem_size = 0;

        Ok(())
    }

    /// 壓縮 L0 -> L1
    fn compact(&mut self) -> std::io::Result<()> {
        if self.l0_tables.len() >= 4 {
            // 簡化：將 L0 合併到 L1
            self.l1_tables.extend(self.l0_tables.drain(..));
        }
        Ok(())
    }
}

impl Storage for LsmStorage {
    fn read_node(&mut self, page_id: usize) -> Node {
        // 先查 mem_table
        if let Some(data) = self.mem_table.get(&(page_id as u64)) {
            return decode_node(data);
        }
        
        // 查 L0
        for table in &self.l0_tables {
            if let Some(data) = table.get(page_id as u64) {
                return decode_node(&data);
            }
        }
        
        // 查 L1
        for table in &self.l1_tables {
            if let Some(data) = table.get(page_id as u64) {
                return decode_node(&data);
            }
        }
        
        // 查 L2
        for table in &self.l2_tables {
            if let Some(data) = table.get(page_id as u64) {
                return decode_node(&data);
            }
        }
        
        panic!("page not found: {}", page_id)
    }

    fn write_node(&mut self, page_id: usize, node: &Node) {
        let data = encode_node(node);
        self.mem_table.insert(page_id as u64, data);
        self.mem_size += PAGE_SIZE;
        
        // 檢查是否需要 flush
        if self.mem_size >= self.mem_threshold {
            let _ = self.flush();
        }
    }

    fn alloc_page(&mut self) -> usize {
        let id = self.page_count;
        self.page_count += 1;
        
        // 在 mem_table 中預先分配空白頁面
        let blank = vec![0u8; PAGE_SIZE];
        self.mem_table.insert(id as u64, blank);
        
        id
    }

    fn page_count(&self) -> usize {
        self.page_count
    }

    fn flush(&mut self) {
        // 先 flush memtable
        let _ = self.flush();
        
        // 壓縮
        let _ = self.compact();
    }

    fn begin_txn(&mut self)    {}
    fn commit_txn(&mut self)   { let _ = self.flush(); }
    fn rollback_txn(&mut self) { self.mem_table.clear(); self.mem_size = 0; }

    fn catalog_root(&self) -> Option<usize> { None }
    fn set_catalog_root(&mut self, _root: usize) {}
    fn is_wal(&self) -> bool { false }  // LSM 自己處理
}

// ── SSTable ──────────────────────────────────────────────────────────────────

/// SSTable: Sorted String Table
pub struct SSTable {
    file_id: u64,
    data_path: PathBuf,
    index: BTreeMap<u64, u64>,  // page_id -> offset
    min_key: u64,
    max_key: u64,
}

impl SSTable {
    /// 建立新的 SSTable
    fn create(file_id: u64, dir: &Path, data: &BTreeMap<u64, Vec<u8>>) -> std::io::Result<Self> {
        let data_path = dir.join(format!("sst_{}.data", file_id));
        let index_path = dir.join(format!("sst_{}.idx", file_id));
        
        let mut file = File::create(&data_path)?;
        let mut index = BTreeMap::new();
        
        let mut min_key = u64::MAX;
        let mut max_key = u64::MIN;
        
        // 寫入所有資料
        for (key, value) in data {
            let offset = file.metadata()?.len();
            index.insert(*key, offset);
            file.write_all(value)?;
            
            min_key = min_key.min(*key);
            max_key = max_key.max(*key);
        }
        
        // 寫入索引
        let mut index_file = File::create(&index_path)?;
        for (key, offset) in &index {
            index_file.write_all(&key.to_le_bytes())?;
            index_file.write_all(&offset.to_le_bytes())?;
        }
        
        Ok(SSTable {
            file_id,
            data_path,
            index,
            min_key,
            max_key,
        })
    }

    /// 開啟現有的 SSTable
    fn open(file_id: u64, dir: &Path) -> std::io::Result<Self> {
        let data_path = dir.join(format!("sst_{}.data", file_id));
        let index_path = dir.join(format!("sst_{}.idx", file_id));
        
        let mut index = BTreeMap::new();
        
        // 讀取索引
        if let Ok(mut index_file) = File::open(&index_path) {
            let mut buf = [0u8; 16];
            while index_file.read_exact(&mut buf).is_ok() {
                let key = u64::from_le_bytes([buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7]]);
                let offset = u64::from_le_bytes([buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15]]);
                index.insert(key, offset);
            }
        }
        
        let min_key = *index.keys().next().unwrap_or(&0);
        let max_key = *index.keys().last().unwrap_or(&0);
        
        Ok(SSTable {
            file_id,
            data_path,
            index,
            min_key,
            max_key,
        })
    }

    /// 讀取指定 page
    fn get(&self, page_id: u64) -> Option<Vec<u8>> {
        let offset = *self.index.get(&page_id)?;
        
        let mut file = File::open(&self.data_path).ok()?;
        file.seek(SeekFrom::Start(offset)).ok()?;
        
        let mut buf = vec![0u8; PAGE_SIZE];
        file.read_exact(&mut buf).ok()?;
        
        Some(buf)
    }
}

// ── 測試 ───────────────────────────────────────────────────────────────────

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

    #[test]
    fn lsm_write_read() {
        let path = "/tmp/sql6_lsm_test";
        let _ = fs::remove_dir_all(path);
        
        let mut storage = LsmStorage::open(path).unwrap();
        
        // 寫入
        let id = storage.alloc_page();
        storage.write_node(id, &leaf_with(42, "hello"));
        storage.flush();
        
        // 讀取（從磁碟）
        let mut storage = LsmStorage::open(path).unwrap();
        let node = storage.read_node(id);
        assert_eq!(node.keys[0], Key::Integer(42));
        
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn lsm_multiple_writes() {
        let path = "/tmp/sql6_lsm_multi";
        let _ = fs::remove_dir_all(path);
        
        let mut storage = LsmStorage::open(path).unwrap();
        
        // 寫入多筆（觸發 flush）
        for i in 0..1000 {
            let id = storage.alloc_page();
            storage.write_node(id, &leaf_with(i as i64, &format!("value_{}", i)));
        }
        
        storage.flush();
        
        // 讀取
        let node = storage.read_node(500);
        assert_eq!(node.keys[0], Key::Integer(500));
        
        let _ = fs::remove_dir_all(path);
    }
}