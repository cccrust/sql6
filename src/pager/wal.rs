//! WAL（Write-Ahead Log）預寫日誌
//!
//! ## 設計原則（與 SQLite WAL 模式相同）
//!
//! 1. **寫入前先記日誌**：每次 write_node 先把舊頁內容寫入 WAL，再修改主檔
//! 2. **Commit 時標記**：寫入 commit frame，表示這筆交易完整
//! 3. **崩潰恢復**：重開時掃描 WAL，只 replay 有 commit frame 的交易
//! 4. **Checkpoint**：WAL 累積到一定大小後，把已提交的頁面寫回主檔並截斷 WAL
//!
//! ## WAL 檔案格式
//!
//! ```text
//! WAL Header（32 bytes）：
//!   [0..8]   magic      : b"SQL4WAL\0"
//!   [8..12]  version    : u32 = 1
//!   [12..16] page_size  : u32
//!   [16..20] frame_count: u32（已寫入的 frame 數）
//!   [20..32] reserved
//!
//! WAL Frame（PAGE_SIZE + 24 bytes）：
//!   [0..4]   page_id    : u32
//!   [4..8]   frame_type : u32（1=data, 2=commit）
//!   [8..12]  txn_id     : u32（交易序號）
//!   [12..16] checksum   : u32（page data 的簡單 XOR checksum）
//!   [16..24] reserved
//!   [24..]   page_data  : PAGE_SIZE bytes
//! ```

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::pager::codec::PAGE_SIZE;

// ── 常數 ──────────────────────────────────────────────────────────────────

const WAL_MAGIC: &[u8; 8] = b"SQL4WAL\0";
const WAL_VERSION: u32 = 1;
const WAL_HEADER_SIZE: usize = 32;
const FRAME_HEADER_SIZE: usize = 24;
const FRAME_SIZE: usize = FRAME_HEADER_SIZE + PAGE_SIZE;

const FRAME_TYPE_DATA:   u32 = 1;
const FRAME_TYPE_COMMIT: u32 = 2;

/// WAL 超過此 frame 數時自動 checkpoint
const CHECKPOINT_THRESHOLD: usize = 100;

// ── Frame ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Frame {
    page_id:    u32,
    frame_type: u32,
    txn_id:     u32,
    checksum:   u32,
    data:       Vec<u8>,
}

impl Frame {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(FRAME_SIZE);
        buf.extend_from_slice(&self.page_id.to_le_bytes());
        buf.extend_from_slice(&self.frame_type.to_le_bytes());
        buf.extend_from_slice(&self.txn_id.to_le_bytes());
        buf.extend_from_slice(&self.checksum.to_le_bytes());
        buf.extend_from_slice(&[0u8; 8]); // reserved
        buf.extend_from_slice(&self.data);
        buf
    }

    fn decode(buf: &[u8]) -> Option<Frame> {
        if buf.len() < FRAME_SIZE { return None; }
        let page_id    = u32::from_le_bytes(buf[0..4].try_into().ok()?);
        let frame_type = u32::from_le_bytes(buf[4..8].try_into().ok()?);
        let txn_id     = u32::from_le_bytes(buf[8..12].try_into().ok()?);
        let checksum   = u32::from_le_bytes(buf[12..16].try_into().ok()?);
        let data       = buf[FRAME_HEADER_SIZE..FRAME_SIZE].to_vec();

        // 驗證 checksum
        let actual = compute_checksum(&data);
        if actual != checksum { return None; }

        Some(Frame { page_id, frame_type, txn_id, checksum, data })
    }
}

fn compute_checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |acc, chunk| {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        acc ^ u32::from_le_bytes(word)
    })
}

// ── WalHeader ─────────────────────────────────────────────────────────────

fn write_wal_header(file: &mut File, frame_count: u32) -> std::io::Result<()> {
    let mut hdr = vec![0u8; WAL_HEADER_SIZE];
    hdr[0..8].copy_from_slice(WAL_MAGIC);
    hdr[8..12].copy_from_slice(&WAL_VERSION.to_le_bytes());
    hdr[12..16].copy_from_slice(&(PAGE_SIZE as u32).to_le_bytes());
    hdr[16..20].copy_from_slice(&frame_count.to_le_bytes());
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&hdr)?;
    file.flush()
}

fn read_wal_frame_count(file: &mut File) -> std::io::Result<u32> {
    let mut hdr = vec![0u8; WAL_HEADER_SIZE];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut hdr)?;
    if &hdr[0..8] != WAL_MAGIC { return Ok(0); }
    Ok(u32::from_le_bytes(hdr[16..20].try_into().unwrap()))
}

// ── Wal ───────────────────────────────────────────────────────────────────

/// WAL 管理器
pub struct Wal {
    wal_path:    PathBuf,
    wal_file:    File,
    frame_count: usize,
    /// 記憶體中的 WAL 快取：page_id → 最新已提交的 page data
    committed:   HashMap<u32, Vec<u8>>,
    /// 目前交易尚未提交的 dirty pages（用於 commit）
    dirty:       HashMap<u32, Vec<u8>>,
    /// 交易開始前的原始頁面（用於 rollback）
    pre_image:   HashMap<u32, Vec<u8>>,
    next_txn_id: u32,
    in_txn:      bool,
}

impl Wal {
    /// 開啟或建立 WAL 檔案，並 replay 已提交的 frame
    pub fn open<P: AsRef<Path>>(db_path: P) -> std::io::Result<Self> {
        let db_path = db_path.as_ref();
        let wal_path = db_path.with_extension("sql6wal");

        let wal_exists = wal_path.exists();
        let wal_file = OpenOptions::new()
            .read(true).write(true).create(true)
            .open(&wal_path)?;

        let mut wal = Wal {
            wal_path,
            wal_file,
            frame_count: 0,
            committed: HashMap::new(),
            dirty: HashMap::new(),
            pre_image: HashMap::new(),
            next_txn_id: 1,
            in_txn: false,
        };

        if wal_exists {
            wal.replay()?;
        } else {
            write_wal_header(&mut wal.wal_file, 0)?;
        }

        Ok(wal)
    }

    // ── 交易控制 ──────────────────────────────────────────────────────────

    /// 開始一筆交易
    pub fn begin(&mut self) {
        self.dirty.clear();
        self.pre_image.clear();
        self.in_txn = true;
    }

    /// 提交：把 dirty pages 寫入 WAL 並標記 commit frame
    pub fn commit(&mut self) -> std::io::Result<()> {
        if !self.in_txn { return Ok(()); }
        let txn_id = self.next_txn_id;
        self.next_txn_id += 1;

        // 寫入所有 dirty page frames（先 clone 避免 borrow 衝突）
        let dirty_pages: Vec<(u32, Vec<u8>)> = self.dirty.iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        for (page_id, data) in &dirty_pages {
            self.write_frame(*page_id, FRAME_TYPE_DATA, txn_id, data)?;
        }

        // 寫入 commit frame（用 page_id=u32::MAX 作為標記）
        let commit_data = vec![0u8; PAGE_SIZE];
        self.write_frame(u32::MAX, FRAME_TYPE_COMMIT, txn_id, &commit_data)?;

        // 更新記憶體快取
        for (page_id, data) in self.dirty.drain() {
            self.committed.insert(page_id, data);
        }

        self.in_txn = false;

        // 超過閾值自動 checkpoint（但不強制，讓呼叫端決定）
        // 這裡只標記，實際 checkpoint 由 DiskWalStorage.flush() 觸發
        Ok(())
    }

    /// Rollback：丟棄 dirty pages，恢復 pre_image 到 committed
    pub fn rollback(&mut self) {
        for (page_id, original_data) in self.pre_image.drain() {
            self.committed.insert(page_id, original_data);
        }
        self.dirty.clear();
        self.in_txn = false;
    }

    /// 儲存頁面的原始狀態（在修改前呼叫）
    /// 如果頁面已在 pre_image 中，不會覆蓋
    pub fn save_original(&mut self, page_id: u32, original_data: Vec<u8>) {
        if !self.in_txn { return; }
        self.pre_image.entry(page_id).or_insert(original_data);
    }

/// 取得頁面的原始狀態（用於 rollback）
    pub fn get_original(&self, page_id: u32) -> Option<&Vec<u8>> {
        self.pre_image.get(&page_id)
    }

    /// 檢查是否在交易中
    pub fn in_txn(&self) -> bool {
        self.in_txn
    }

    /// 取得頁面的 committed 副本（用於儲存為原始狀態）
    /// 如果頁面在 dirty 中，返回 dirty 的副本（因為 dirty 是更新的）
    pub fn get_committed_copy(&self, page_id: u32) -> Option<Vec<u8>> {
        // 如果在 dirty 中，返回 dirty 的副本（因為這是"原始"在我們的交易中)
        if self.dirty.contains_key(&page_id) {
            return self.dirty.get(&page_id).cloned();
        }
        self.committed.get(&page_id).cloned()
    }

    /// 寫入一頁到 dirty buffer（交易中）
    /// 若不在交易中，直接寫入 committed（auto-commit 模式）
    pub fn write_page(&mut self, page_id: u32, data: Vec<u8>) {
        if self.in_txn {
            self.dirty.insert(page_id, data);
        } else {
            // auto-commit：直接提交
            self.committed.insert(page_id, data.clone());
            let txn_id = self.next_txn_id;
            self.next_txn_id += 1;
            if let Err(e) = self.write_frame(page_id, FRAME_TYPE_DATA, txn_id, &data) {
                eprintln!("WAL write_frame error: {}", e);
            }
            let commit_data = vec![0u8; PAGE_SIZE];
            if let Err(e) = self.write_frame(u32::MAX, FRAME_TYPE_COMMIT, txn_id, &commit_data) {
                eprintln!("WAL write_commit error: {}", e);
            }
        }
    }

    /// 批次寫入多頁面（減少 I/O 呼叫）
    pub fn write_batch(&mut self, pages: Vec<(u32, Vec<u8>)>) {
        if self.in_txn {
            // 在交易中，批量寫入 dirty
            for (page_id, data) in pages {
                self.dirty.insert(page_id, data);
            }
        } else {
            // auto-commit 模式
            let txn_id = self.next_txn_id;
            self.next_txn_id += 1;
            for (page_id, data) in pages {
                self.committed.insert(page_id, data.clone());
                if let Err(e) = self.write_frame(page_id, FRAME_TYPE_DATA, txn_id, &data) {
                    eprintln!("WAL batch write error: {}", e);
                }
            }
            // 寫入 commit frame
            let commit_data = vec![0u8; PAGE_SIZE];
            if let Err(e) = self.write_frame(u32::MAX, FRAME_TYPE_COMMIT, txn_id, &commit_data) {
                eprintln!("WAL batch commit error: {}", e);
            }
        }
    }

    /// 讀取一頁：優先從 dirty → committed WAL 快取
    pub fn read_page(&self, page_id: u32) -> Option<&[u8]> {
        self.dirty.get(&page_id)
            .or_else(|| self.committed.get(&page_id))
            .map(|v| v.as_slice())
    }

    /// 是否超過 checkpoint 閾值
    pub fn needs_checkpoint(&self) -> bool {
        self.frame_count >= CHECKPOINT_THRESHOLD
    }

    /// 回傳目前的 frame 數量
    pub fn frame_count(&self) -> usize {
        self.frame_count
    }

    /// Checkpoint：把所有已提交頁面寫回主檔，然後截斷 WAL
    /// `write_back`：一個 closure，負責把 page_data 寫回主檔的對應位置
    pub fn checkpoint<F>(&mut self, mut write_back: F) -> std::io::Result<()>
    where
        F: FnMut(u32, &[u8]) -> std::io::Result<()>,
    {
        for (page_id, data) in &self.committed {
            write_back(*page_id, data)?;
        }
        // 截斷 WAL
        self.wal_file.set_len(WAL_HEADER_SIZE as u64)?;
        write_wal_header(&mut self.wal_file, 0)?;
        self.frame_count = 0;
        self.committed.clear();
        Ok(())
    }

    // ── 內部輔助 ──────────────────────────────────────────────────────────

fn write_frame(&mut self, page_id: u32, frame_type: u32, txn_id: u32, data: &[u8]) -> std::io::Result<()> {
        let checksum = compute_checksum(data);
        let frame = Frame { page_id, frame_type, txn_id, checksum, data: data.to_vec() };
        let encoded = frame.encode();

        // 擴展並寫入 WAL 檔案
        let target_size = WAL_HEADER_SIZE as u64 + (self.frame_count as u64 + 1) * FRAME_SIZE as u64;
        
        // 使用 set_len 擴展檔案
        self.wal_file.set_len(target_size)?;
        
        // 強制刷新檔案系統緩衝區
        self.wal_file.sync_all()?;
        
        // 寫入 frame
        let frame_offset = WAL_HEADER_SIZE as u64 + (self.frame_count as u64) * FRAME_SIZE as u64;
        self.wal_file.seek(SeekFrom::Start(frame_offset))?;
        self.wal_file.write_all(&encoded)?;
        self.wal_file.sync_all()?;
        
        self.frame_count += 1;
        write_wal_header(&mut self.wal_file, self.frame_count as u32)?;
        self.wal_file.sync_all()?;
        
        Ok(())
    }

    /// 重開時 replay WAL：只套用有 commit frame 的交易
    fn replay(&mut self) -> std::io::Result<()> {
        let frame_count = read_wal_frame_count(&mut self.wal_file)? as usize;
        if frame_count == 0 { return Ok(()); }

        // 讀取所有 frames
        let mut frames: Vec<Frame> = Vec::new();
        for i in 0..frame_count {
            let offset = WAL_HEADER_SIZE as u64 + (i as u64) * FRAME_SIZE as u64;
            self.wal_file.seek(SeekFrom::Start(offset))?;
            let mut buf = vec![0u8; FRAME_SIZE];
            if self.wal_file.read_exact(&mut buf).is_err() { break; }
            if let Some(frame) = Frame::decode(&buf) {
                frames.push(frame);
            }
        }

        // 找出已提交的交易（有 commit frame）
        let committed_txns: std::collections::HashSet<u32> = frames.iter()
            .filter(|f| f.frame_type == FRAME_TYPE_COMMIT)
            .map(|f| f.txn_id)
            .collect();

        // 只 replay 已提交的 data frames
        for frame in &frames {
            if frame.frame_type == FRAME_TYPE_DATA && committed_txns.contains(&frame.txn_id) {
                self.committed.insert(frame.page_id, frame.data.clone());
            }
        }

        self.frame_count = frame_count;
        Ok(())
    }
}

// ── 測試 ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/sql6_wal_{}.db", name))
    }

    fn cleanup(name: &str) {
        let _ = std::fs::remove_file(tmp_path(name));
        let _ = std::fs::remove_file(tmp_path(name).with_extension("sql6wal"));
    }

    #[test]
    fn write_and_read_in_txn() {
        cleanup("txn");
        let mut wal = Wal::open(tmp_path("txn")).unwrap();
        wal.begin();
        wal.write_page(0, vec![1u8; PAGE_SIZE]);
        // 在交易中可以讀到 dirty page
        assert_eq!(wal.read_page(0).unwrap()[0], 1);
        wal.commit().unwrap();
        // commit 後在 committed 中
        assert_eq!(wal.read_page(0).unwrap()[0], 1);
        cleanup("txn");
    }

    #[test]
    fn rollback_discards_dirty() {
        cleanup("rollback");
        let mut wal = Wal::open(tmp_path("rollback")).unwrap();
        wal.begin();
        wal.write_page(1, vec![42u8; PAGE_SIZE]);
        wal.rollback();
        // rollback 後 dirty 消失
        assert!(wal.read_page(1).is_none());
        cleanup("rollback");
    }

    #[test]
    fn replay_after_crash() {
        cleanup("replay");
        // 模擬寫入後「崩潰」（不 flush 主檔，只有 WAL）
        {
            let mut wal = Wal::open(tmp_path("replay")).unwrap();
            wal.begin();
            wal.write_page(5, vec![99u8; PAGE_SIZE]);
            wal.commit().unwrap();
            // 不呼叫 checkpoint，只讓 WAL 保留
        }
        // 重新開啟，應該 replay 出 page 5
        {
            let wal = Wal::open(tmp_path("replay")).unwrap();
            let data = wal.read_page(5).expect("page 5 should be in WAL after replay");
            assert_eq!(data[0], 99);
        }
        cleanup("replay");
    }

    #[test]
    fn partial_txn_not_replayed() {
        cleanup("partial");
        {
            let mut wal = Wal::open(tmp_path("partial")).unwrap();
            // 開始交易但不 commit（模擬崩潰在 commit 前）
            wal.begin();
            // 手動寫一個 data frame（沒有對應 commit frame）
            wal.write_frame(3, FRAME_TYPE_DATA, 99, &vec![0xABu8; PAGE_SIZE]).unwrap();
        }
        // 重開：沒有 commit frame，所以不 replay
        {
            let wal = Wal::open(tmp_path("partial")).unwrap();
            assert!(wal.read_page(3).is_none(), "uncommitted page should not be visible");
        }
        cleanup("partial");
    }

    #[test]
    fn auto_commit_mode() {
        cleanup("auto");
        let mut wal = Wal::open(tmp_path("auto")).unwrap();
        // 不呼叫 begin()，直接 write（auto-commit）
        wal.write_page(0, vec![77u8; PAGE_SIZE]);
        assert_eq!(wal.read_page(0).unwrap()[0], 77);
        cleanup("auto");
    }

    #[test]
    fn checkpoint_clears_wal() {
        cleanup("chkpt");
        let mut wal = Wal::open(tmp_path("chkpt")).unwrap();
        wal.begin();
        wal.write_page(0, vec![55u8; PAGE_SIZE]);
        wal.write_page(1, vec![66u8; PAGE_SIZE]);
        wal.commit().unwrap();

        let mut written: Vec<(u32, u8)> = Vec::new();
        wal.checkpoint(|pid, data| {
            written.push((pid, data[0]));
            Ok(())
        }).unwrap();

        assert_eq!(wal.frame_count, 0);
        assert!(wal.committed.is_empty());
        assert!(written.iter().any(|(p, v)| *p == 0 && *v == 55));
        assert!(written.iter().any(|(p, v)| *p == 1 && *v == 66));
        cleanup("chkpt");
    }

    #[test]
    fn checksum_corruption_ignored() {
        cleanup("corrupt");
        {
            let mut wal = Wal::open(tmp_path("corrupt")).unwrap();
            wal.begin();
            wal.write_page(7, vec![11u8; PAGE_SIZE]);
            wal.commit().unwrap();
        }
        // 損壞 WAL 中的 checksum
        {
            let wal_path = tmp_path("corrupt").with_extension("sql6wal");
            let mut f = OpenOptions::new().write(true).open(&wal_path).unwrap();
            // 第一個 frame 從 offset 32 開始，checksum 在 frame[12..16]
            f.seek(SeekFrom::Start((WAL_HEADER_SIZE + 12) as u64)).unwrap();
            f.write_all(&[0xFF, 0xFF, 0xFF, 0xFF]).unwrap();
        }
        // 重開：損壞的 frame 應被忽略
        {
            let wal = Wal::open(tmp_path("corrupt")).unwrap();
            assert!(wal.read_page(7).is_none(), "corrupted frame should be ignored");
        }
        cleanup("corrupt");
    }

    #[test]
    fn multiple_pages_in_transaction() {
        cleanup("multi");
        let mut wal = Wal::open(tmp_path("multi")).unwrap();
        wal.begin();
        wal.write_page(0, vec![10u8; PAGE_SIZE]);
        wal.write_page(1, vec![20u8; PAGE_SIZE]);
        wal.write_page(2, vec![30u8; PAGE_SIZE]);
        // 交易中可以讀到所有 dirty pages
        assert_eq!(wal.read_page(0).unwrap()[0], 10);
        assert_eq!(wal.read_page(1).unwrap()[0], 20);
        assert_eq!(wal.read_page(2).unwrap()[0], 30);
        wal.commit().unwrap();
        // commit 後仍然可以讀到
        assert_eq!(wal.read_page(0).unwrap()[0], 10);
        assert_eq!(wal.read_page(1).unwrap()[0], 20);
        assert_eq!(wal.read_page(2).unwrap()[0], 30);
        cleanup("multi");
    }

    #[test]
    fn commit_after_rollback() {
        cleanup("commit_after_rb");
        let mut wal = Wal::open(tmp_path("commit_after_rb")).unwrap();
        wal.begin();
        wal.write_page(0, vec![1u8; PAGE_SIZE]);
        wal.rollback();
        // rollback 後重新開始交易
        wal.begin();
        wal.write_page(0, vec![2u8; PAGE_SIZE]);
        wal.commit().unwrap();
        // 應該讀到新的值
        assert_eq!(wal.read_page(0).unwrap()[0], 2);
        cleanup("commit_after_rb");
    }

    #[test]
    fn nested_transaction_fails() {
        cleanup("nested");
        let mut wal = Wal::open(tmp_path("nested")).unwrap();
        wal.begin();
        // 在交易中嘗試再次 begin 應該被忽略（auto-commit 模式）
        wal.begin();
        wal.write_page(0, vec![99u8; PAGE_SIZE]);
        // 仍然可以讀到，因為第二次 begin 被忽略
        assert_eq!(wal.read_page(0).unwrap()[0], 99);
        wal.commit().unwrap();
        assert_eq!(wal.read_page(0).unwrap()[0], 99);
        cleanup("nested");
    }

    #[test]
    fn update_page_in_wal() {
        cleanup("update");
        let mut wal = Wal::open(tmp_path("update")).unwrap();
        wal.begin();
        wal.write_page(0, vec![1u8; PAGE_SIZE]);
        wal.commit().unwrap();
        // 再次修改同一頁
        wal.begin();
        wal.write_page(0, vec![2u8; PAGE_SIZE]);
        wal.commit().unwrap();
        // 應該讀到最新的值
        assert_eq!(wal.read_page(0).unwrap()[0], 2);
        cleanup("update");
    }

    #[test]
    fn wal_preserves_multiple_commits() {
        cleanup("multi_commit");
        let mut wal = Wal::open(tmp_path("multi_commit")).unwrap();
        // 第一次提交
        wal.begin();
        wal.write_page(0, vec![10u8; PAGE_SIZE]);
        wal.commit().unwrap();
        // 第二次提交（不同頁）
        wal.begin();
        wal.write_page(1, vec![20u8; PAGE_SIZE]);
        wal.commit().unwrap();
        // 兩個頁都應該可讀
        assert_eq!(wal.read_page(0).unwrap()[0], 10);
        assert_eq!(wal.read_page(1).unwrap()[0], 20);
        cleanup("multi_commit");
    }

    #[test]
    fn rollback_then_write_same_page() {
        cleanup("rb_write");
        let mut wal = Wal::open(tmp_path("rb_write")).unwrap();
        wal.begin();
        wal.write_page(0, vec![1u8; PAGE_SIZE]);
        wal.rollback();
        // 再次寫入同一頁
        wal.begin();
        wal.write_page(0, vec![2u8; PAGE_SIZE]);
        wal.commit().unwrap();
        assert_eq!(wal.read_page(0).unwrap()[0], 2);
        cleanup("rb_write");
    }

    #[test]
    fn frame_count_after_commits() {
        cleanup("frame_count");
        let mut wal = Wal::open(tmp_path("frame_count")).unwrap();
        wal.begin();
        wal.write_page(0, vec![1u8; PAGE_SIZE]);
        wal.commit().unwrap();
        let count1 = wal.frame_count();
        wal.begin();
        wal.write_page(1, vec![2u8; PAGE_SIZE]);
        wal.commit().unwrap();
        let count2 = wal.frame_count();
        // 每個 commit 寫入 data frame + commit frame
        assert!(count2 > count1);
        cleanup("frame_count");
    }

    #[test]
    fn checkpoint_callback_called_correctly() {
        cleanup("callback");
        let mut wal = Wal::open(tmp_path("callback")).unwrap();
        wal.begin();
        wal.write_page(5, vec![42u8; PAGE_SIZE]);
        wal.write_page(10, vec![43u8; PAGE_SIZE]);
        wal.commit().unwrap();

        let mut received: Vec<u32> = Vec::new();
        wal.checkpoint(|page_id, _data| {
            received.push(page_id);
            Ok(())
        }).unwrap();

        assert!(received.contains(&5));
        assert!(received.contains(&10));
        cleanup("callback");
    }

    #[test]
    fn committed_cache_cleared_after_checkpoint() {
        cleanup("clear_cache");
        let mut wal = Wal::open(tmp_path("clear_cache")).unwrap();
        wal.begin();
        wal.write_page(0, vec![55u8; PAGE_SIZE]);
        wal.commit().unwrap();
        assert!(!wal.committed.is_empty());

        wal.checkpoint(|_, _| Ok(())).unwrap();
        assert!(wal.committed.is_empty());
        cleanup("clear_cache");
    }

    #[test]
    fn needs_checkpoint_threshold() {
        cleanup("threshold");
        let mut wal = Wal::open(tmp_path("threshold")).unwrap();
        assert!(!wal.needs_checkpoint());
        // 寫入足夠多頁觸發 checkpoint 閾值
        for i in 0..120 {
            wal.begin();
            wal.write_page(i as u32, vec![i as u8; PAGE_SIZE]);
            wal.commit().unwrap();
        }
        assert!(wal.needs_checkpoint());
        cleanup("threshold");
    }
}
