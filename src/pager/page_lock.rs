//! Page Lock 管理器 - 支援頁面級並發鎖定

use crate::btree::node::Node;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::thread::ThreadId;

/// 鎖定模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// 共享鎖 - 允許多個讀取同時持有
    Shared,
    /// 排他鎖 - 僅允許單一寫入持有
    Exclusive,
}

/// 頁面鎖定狀態
struct PageLockState {
    mode: LockMode,
    shared_count: usize,  // 共享鎖持有者數量
    exclusive_holder: Option<ThreadId>,
}

/// Page Lock Manager - 實現頁面級鎖定
pub struct PageLockManager {
    locks: Arc<RwLock<HashMap<usize, PageLockState>>>,
}

impl PageLockManager {
    pub fn new() -> Self {
        PageLockManager {
            locks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 取得共享鎖（用於讀取）
    pub fn lock_shared(&self, page_id: usize) -> Result<PageLockGuard, LockError> {
        let mut locks = self.locks.write().unwrap();
        
        // 檢查是否有排他鎖
        if let Some(state) = locks.get(&page_id) {
            if state.exclusive_holder.is_some() {
                return Err(LockError::BlockedByExclusive);
            }
        }
        
        // 增加共享計數或創建新鎖
        let state = locks.entry(page_id).or_insert(PageLockState {
            mode: LockMode::Shared,
            shared_count: 0,
            exclusive_holder: None,
        });
        state.shared_count += 1;
        
        Ok(PageLockGuard {
            page_id,
            manager: self.locks.clone(),
            mode: LockMode::Shared,
        })
    }

    /// 取得排他鎖（用於寫入）
    pub fn lock_exclusive(&self, page_id: usize) -> Result<PageLockGuard, LockError> {
        let mut locks = self.locks.write().unwrap();
        
        // 檢查是否有其他鎖
        if let Some(state) = locks.get(&page_id) {
            if state.shared_count > 0 {
                return Err(LockError::BlockedByShared);
            }
            if state.exclusive_holder.is_some() {
                return Err(LockError::Deadlock);
            }
        }
        
        // 創建排他鎖
        let state = locks.entry(page_id).or_insert(PageLockState {
            mode: LockMode::Exclusive,
            shared_count: 0,
            exclusive_holder: None,
        });
        state.exclusive_holder = Some(std::thread::current().id());
        
        Ok(PageLockGuard {
            page_id,
            manager: self.locks.clone(),
            mode: LockMode::Exclusive,
        })
    }

    /// 嘗試取得共享鎖（非阻塞）
    pub fn try_lock_shared(&self, page_id: usize) -> Option<PageLockGuard> {
        self.lock_shared(page_id).ok()
    }

    /// 嘗試取得排他鎖（非阻塞）
    pub fn try_lock_exclusive(&self, page_id: usize) -> Option<PageLockGuard> {
        self.lock_exclusive(page_id).ok()
    }

    /// 釋放所有鎖（用於清理）
    pub fn release_all(&self) {
        let mut locks = self.locks.write().unwrap();
        locks.clear();
    }
}

impl Default for PageLockManager {
    fn default() -> Self { Self::new() }
}

/// 鎖定守護者 - RAII 自動釋放
pub struct PageLockGuard {
    page_id: usize,
    manager: Arc<RwLock<HashMap<usize, PageLockState>>>,
    mode: LockMode,
}

impl Drop for PageLockGuard {
    fn drop(&mut self) {
        let mut locks = self.manager.write().unwrap();
        if let Some(state) = locks.get_mut(&self.page_id) {
            match self.mode {
                LockMode::Shared => {
                    state.shared_count = state.shared_count.saturating_sub(1);
                    if state.shared_count == 0 && state.exclusive_holder.is_none() {
                        locks.remove(&self.page_id);
                    }
                }
                LockMode::Exclusive => {
                    state.exclusive_holder = None;
                    if state.shared_count == 0 {
                        locks.remove(&self.page_id);
                    }
                }
            }
        }
    }
}

/// 鎖定錯誤
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockError {
    /// 被排他鎖阻塞
    BlockedByExclusive,
    /// 被共享鎖阻塞
    BlockedByShared,
    /// 死鎖（嘗試取得已被自己持有的鎖）
    Deadlock,
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::BlockedByExclusive => write!(f, "blocked by exclusive lock"),
            LockError::BlockedByShared => write!(f, "blocked by shared locks"),
            LockError::Deadlock => write!(f, "deadlock detected"),
        }
    }
}

impl std::error::Error for LockError {}

/// 頁面鎖定的 Storage 包裝器
pub struct LockedStorage<S> {
    pub inner: S,
    pub lock_manager: Arc<PageLockManager>,
}

impl<S> LockedStorage<S> {
    pub fn new(inner: S) -> Self {
        LockedStorage {
            inner,
            lock_manager: Arc::new(PageLockManager::new()),
        }
    }
}

impl<S: Storage> Storage for LockedStorage<S> {
    fn read_node(&mut self, page_id: usize) -> Node {
        // 自動取得共享鎖
        let _lock = self.lock_manager.lock_shared(page_id).unwrap();
        self.inner.read_node(page_id)
    }

    fn write_node(&mut self, page_id: usize, node: &Node) {
        // 自動取得排他鎖
        let _lock = self.lock_manager.lock_exclusive(page_id).unwrap();
        self.inner.write_node(page_id, node);
    }

    fn alloc_page(&mut self) -> usize {
        // 新頁面不需要鎖定（使用新頁面號）
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

    fn catalog_root(&self) -> Option<usize> {
        self.inner.catalog_root()
    }

    fn set_catalog_root(&mut self, root: usize) {
        self.inner.set_catalog_root(root)
    }

    fn is_wal(&self) -> bool {
        self.inner.is_wal()
    }

    fn cache_size(&self) -> usize {
        self.inner.cache_size()
    }

    fn set_cache_size(&mut self, size: usize) {
        self.inner.set_cache_size(size)
    }

    fn page_size(&self) -> usize {
        self.inner.page_size()
    }

    fn freelist_count(&self) -> usize {
        self.inner.freelist_count()
    }
}

// 重新匯入 Storage trait
use super::storage::Storage;

/// 測試
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pager::storage::MemoryStorage;
    use std::sync::Arc;

    #[test]
    fn test_shared_lock() {
        let manager = Arc::new(PageLockManager::new());
        
        // 取得共享鎖
        let lock1 = manager.lock_shared(1).unwrap();
        let lock2 = manager.lock_shared(1).unwrap();
        
        // 兩個共享鎖可以同時存在
        drop(lock1);
        drop(lock2);
        
        // 現在應該可以取得排他鎖
        let exclusive = manager.lock_exclusive(1).unwrap();
        drop(exclusive);
    }

    #[test]
    fn test_exclusive_blocks_shared() {
        let manager = PageLockManager::new();
        
        let _exclusive = manager.lock_exclusive(1).unwrap();
        
        // 共享鎖應該被阻塞
        let result = manager.try_lock_shared(1);
        assert!(result.is_none());
    }

    #[test]
    fn test_locked_storage() {
        let lock_manager = Arc::new(PageLockManager::new());
        let mut storage = LockedStorage {
            inner: MemoryStorage::new(),
            lock_manager,
        };
        
        // 測試基本操作
        let id = storage.alloc_page();
        let node = crate::btree::node::Node::new_leaf();
        storage.write_node(id, &node);
        let _read = storage.read_node(id);
    }
}