//! Pager 模組：儲存引擎與分頁管理
//!
//! 負責管理資料庫檔案的分頁讀寫，提供 Storage trait 抽象。
//!
//! # 組成
//! - `storage`：儲存抽象介面（Storage trait + MemoryStorage + LruCacheStorage）
//! - `file_btree_storage`：單一檔案 BTree 儲存
//! - `dir_btree_storage`：目錄式多檔 BTree 儲存
//! - `dir_lsmtree_storage`：LSM Tree 儲存引擎
//! - `codec`：分頁編碼/解碼
//! - `wal`：預寫式日誌（Write-Ahead Logging）
//! - `page_lock`：頁面級鎖定管理器
//! - `cache`：頁面快取與 Bloom Filter

pub mod codec;
pub mod storage;
pub mod file_btree_storage;
pub mod dir_btree_storage;
pub mod dir_lsmtree_storage;
pub mod wal;
pub mod page_lock;
pub mod cache;

