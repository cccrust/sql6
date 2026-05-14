//! Pager 模組：儲存引擎與分頁管理
//!
//! 負責管理資料庫檔案的分頁讀寫，提供 Storage trait 抽象。
//!
//! # 組成
//! - `storage`：儲存抽象介面（記憶體/磁碟）
//! - `codec`：分頁編碼/解碼
//! - `wal`：預寫式日誌（Write-Ahead Logging）
//! - `page_lock`：頁面級鎖定管理器

pub mod codec;
pub mod storage;
pub mod wal;
pub mod page_lock;

