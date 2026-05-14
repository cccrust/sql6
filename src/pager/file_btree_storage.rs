//! 單一檔案磁碟儲存（傳統 BTree）

use super::codec::{decode_node, encode_node, PAGE_SIZE};
use super::wal::Wal;
use super::storage::Storage;
use crate::btree::node::Node;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};

pub struct DiskStorage {
    file: File,
    path: std::path::PathBuf,
    page_count: usize,
    pub catalog_root: Option<usize>,
    wal: Wal,
}

const MAGIC: &[u8; 8] = b"SQL4DB\0\0";
const VERSION: u32 = 2;
const HEADER_OFFSET: u64 = PAGE_SIZE as u64;

impl DiskStorage {
    pub fn open<P: AsRef<std::path::Path>>(path: P) -> std::io::Result<Self> {
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

    pub fn path(&self) -> &std::path::Path { &self.path }

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
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid sql6db magic"));
        }
        self.page_count = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
        let cat_root = u32::from_le_bytes(hdr[16..20].try_into().unwrap()) as usize;
        self.catalog_root = if cat_root > 0 { Some(cat_root) } else { None };
        Ok(())
    }

    fn page_offset(page_id: usize) -> u64 {
        HEADER_OFFSET + (page_id as u64) * PAGE_SIZE as u64
    }

    fn read_page_from_file(&mut self, page_id: usize) -> Vec<u8> {
        let offset = Self::page_offset(page_id);
        let mut buf = vec![0u8; PAGE_SIZE];
        self.file.seek(SeekFrom::Start(offset)).unwrap();
        let _ = self.file.read_exact(&mut buf);
        buf
    }

    fn write_page_to_file(&mut self, page_id: u32, data: &[u8]) -> std::io::Result<()> {
        let offset = Self::page_offset(page_id as usize);
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(data)?;
        Ok(())
    }
}

impl Storage for DiskStorage {
    fn read_node(&mut self, page_id: usize) -> Node {
        if let Some(data) = self.wal.read_page(page_id as u32) { return decode_node(data); }
        let buf = self.read_page_from_file(page_id);
        decode_node(&buf)
    }

    fn write_node(&mut self, page_id: usize, node: &Node) {
        let buf = encode_node(node);
        if self.wal.in_txn() {
            if let Some(original) = self.wal.get_committed_copy(page_id as u32) {
                self.wal.save_original(page_id as u32, original);
            }
        }
        self.wal.write_page(page_id as u32, buf);
    }

    fn alloc_page(&mut self) -> usize {
        let id = self.page_count;
        self.page_count += 1;
        let blank = vec![0u8; PAGE_SIZE];
        self.wal.write_page(id as u32, blank.clone());
        let offset = Self::page_offset(id);
        self.file.seek(SeekFrom::Start(offset)).unwrap();
        self.file.write_all(&blank).unwrap();
        id
    }

    fn page_count(&self) -> usize { self.page_count }

    fn flush(&mut self) {
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
    fn set_catalog_root(&mut self, root: usize) { self.catalog_root = Some(root); let _ = self.write_header(); }
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
        let _ = std::fs::remove_file(format!("/tmp/sql6_{}.db", name));
        let _ = std::fs::remove_file(format!("/tmp/sql6_{}.sql6wal", name));
    }

    #[test]
    fn disk_write_and_read() {
        cleanup("disk_rw");
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
    fn disk_storage_rollback() {
        cleanup("disk_rollback");
        {
            let mut store = DiskStorage::open("/tmp/sql6_disk_rollback.db").unwrap();
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
        cleanup("disk_rollback");
    }

    #[test]
    fn disk_catalog() {
        cleanup("disk_catalog");
        {
            let mut store = DiskStorage::open("/tmp/sql6_disk_catalog.db").unwrap();
            store.set_catalog_root(42);
            store.flush();
        }
        {
            let store = DiskStorage::open("/tmp/sql6_disk_catalog.db").unwrap();
            assert_eq!(store.catalog_root(), Some(42));
        }
        cleanup("disk_catalog");
    }
}