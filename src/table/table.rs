//! Table：把 Schema + BPlusTree 組合成一個完整的資料表
//!
//! 使用範例：
//! ```rust
//! use sql6::table::schema::{Column, DataType, Schema};

//! use sql6::table::row::{Row, Value};

//! use sql6::table::table::Table;

//! use sql6::pager::storage::MemoryStorage;
//!
//! let schema = Schema::new(vec![
//!     Column::new("id",   DataType::Integer),
//!     Column::new("name", DataType::Text),
//! ]);
//! let mut table = Table::new("users", schema, MemoryStorage::new());
//! table.insert(Row::new(vec![Value::Integer(1), Value::Text("Alice".into())]));
//! ```

use crate::btree::node::Key;
use crate::btree::tree::BPlusTree;
use crate::pager::storage::Storage;
use super::row::{Row, Value};
use super::schema::Schema;
use super::serialize;

pub struct Table<S: Storage> {
    pub name:   String,
    pub schema: Schema,
    tree:       BPlusTree<S>,
}

impl<S: Storage> Table<S> {
    /// 建立全新的資料表
    pub fn new(name: &str, schema: Schema, storage: S) -> Self {
        Table {
            name: name.to_string(),
            schema,
            tree: BPlusTree::new(64, storage), // order=64 適合磁碟頁面
        }
    }

    /// 開啟已有資料表（磁碟後端）
    pub fn open(name: &str, schema: Schema, storage: S, root: usize, size: usize) -> Self {
        Table {
            name: name.to_string(),
            schema,
            tree: BPlusTree::open(64, storage, root, size),
        }
    }

    /// 插入一列；第一個欄位自動作為主鍵
    pub fn insert(&mut self, row: Row) -> Result<(), String> {
        let key = self.row_key(&row)?;
        let bytes = serialize::serialize(&self.schema, &row);
        self.tree.insert(key, bytes);
        Ok(())
    }

    /// 以主鍵查詢單筆資料
    pub fn get(&mut self, key: &Key) -> Option<Row> {
        self.tree
            .search(key)
            .map(|bytes| serialize::deserialize(&self.schema, &bytes))
    }

    /// 範圍查詢 [start, end]
    pub fn range(&mut self, start: &Key, end: &Key) -> Vec<Row> {
        self.tree
            .range_search(start, end)
            .into_iter()
            .map(|record| serialize::deserialize(&self.schema, &record.value))
            .collect()
    }

    /// 刪除一筆資料
    pub fn delete(&mut self, key: &Key) -> bool {
        self.tree.delete(key)
    }

    /// 全表掃描（從最小 key 到最大 key）
    pub fn scan(&mut self) -> Vec<Row> {
        self.tree
            .scan_all()
            .into_iter()
            .map(|record| serialize::deserialize(&self.schema, &record.value))
            .collect()
    }

    pub fn len(&self) -> usize { self.tree.len() }
    pub fn is_empty(&self) -> bool { self.tree.is_empty() }
    pub fn root_page(&self) -> usize { self.tree.root_page() }

    pub fn flush(&mut self) { self.tree.flush(); }

    // ------------------------------------------------------------------ //
    //  內部輔助                                                            //
    // ------------------------------------------------------------------ //

    /// 取第一個欄位作為 B+Tree 的 key
    fn row_key(&self, row: &Row) -> Result<Key, String> {
        match row.values.first() {
            Some(Value::Integer(v)) => Ok(Key::Integer(*v)),
            Some(Value::Text(s))    => Ok(Key::Text(s.clone())),
            Some(Value::Null)       => Err("primary key cannot be NULL".to_string()),
            Some(other) => Err(format!("unsupported key type: {}", other)),
            None => Err("row has no values".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btree::node::Key;
    use crate::pager::storage::MemoryStorage;
    use crate::table::schema::{Column, DataType};

    fn create_test_schema() -> Schema {
        Schema::new(vec![
            Column::new("id", DataType::Integer),
            Column::new("name", DataType::Text),
        ])
    }

    #[test]
    fn test_table_new() {
        let storage = MemoryStorage::new();
        let schema = create_test_schema();
        let table = Table::new("users", schema.clone(), storage);

        assert_eq!(table.name, "users");
        assert_eq!(table.schema.len(), 2);
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn test_table_insert_and_get() {
        let storage = MemoryStorage::new();
        let schema = create_test_schema();
        let mut table = Table::new("users", schema, storage);

        let row = Row::new(vec![Value::Integer(1), Value::Text("Alice".to_string())]);
        table.insert(row).unwrap();

        assert_eq!(table.len(), 1);
        assert!(!table.is_empty());

        let retrieved = table.get(&Key::Integer(1));
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.values[0], Value::Integer(1));
        assert_eq!(retrieved.values[1], Value::Text("Alice".to_string()));
    }

    #[test]
    fn test_table_get_not_found() {
        let storage = MemoryStorage::new();
        let schema = create_test_schema();
        let mut table = Table::new("users", schema, storage);

        let row = Row::new(vec![Value::Integer(1), Value::Text("Alice".to_string())]);
        table.insert(row).unwrap();

        let retrieved = table.get(&Key::Integer(999));
        assert!(retrieved.is_none());
    }

    #[test]
    fn test_table_insert_text_key() {
        let schema = Schema::new(vec![
            Column::new("name", DataType::Text),
            Column::new("age", DataType::Integer),
        ]);
        let storage = MemoryStorage::new();
        let mut table = Table::new("users", schema, storage);

        let row = Row::new(vec![Value::Text("alice".to_string()), Value::Integer(30)]);
        table.insert(row).unwrap();

        let retrieved = table.get(&Key::Text("alice".to_string()));
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().values[1], Value::Integer(30));
    }

    #[test]
    fn test_table_delete() {
        let storage = MemoryStorage::new();
        let schema = create_test_schema();
        let mut table = Table::new("users", schema, storage);

        let row = Row::new(vec![Value::Integer(1), Value::Text("Alice".to_string())]);
        table.insert(row).unwrap();
        assert_eq!(table.len(), 1);

        let deleted = table.delete(&Key::Integer(1));
        assert!(deleted);
        assert_eq!(table.len(), 0);
        assert!(table.get(&Key::Integer(1)).is_none());
    }

    #[test]
    fn test_table_delete_not_found() {
        let storage = MemoryStorage::new();
        let schema = create_test_schema();
        let mut table = Table::new("users", schema, storage);

        let row = Row::new(vec![Value::Integer(1), Value::Text("Alice".to_string())]);
        table.insert(row).unwrap();

        let deleted = table.delete(&Key::Integer(999));
        assert!(!deleted);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn test_table_scan() {
        let storage = MemoryStorage::new();
        let schema = create_test_schema();
        let mut table = Table::new("users", schema, storage);

        for i in 1..=5 {
            let row = Row::new(vec![Value::Integer(i), Value::Text(format!("User{}", i))]);
            table.insert(row).unwrap();
        }

        let all = table.scan();
        assert_eq!(all.len(), 5);
    }

    #[test]
    fn test_table_range() {
        let storage = MemoryStorage::new();
        let schema = create_test_schema();
        let mut table = Table::new("users", schema, storage);

        for i in 1..=10 {
            let row = Row::new(vec![Value::Integer(i), Value::Text(format!("User{}", i))]);
            table.insert(row).unwrap();
        }

        let range = table.range(&Key::Integer(3), &Key::Integer(7));
        assert_eq!(range.len(), 5);

        let keys: Vec<i64> = range.iter()
            .map(|r| if let Value::Integer(v) = r.values[0] { v } else { 0 })
            .collect();
        assert_eq!(keys, vec![3, 4, 5, 6, 7]);
    }

    #[test]
    fn test_table_insert_null_key_error() {
        let schema = Schema::new(vec![
            Column::new("id", DataType::Integer),
            Column::new("name", DataType::Text),
        ]);
        let storage = MemoryStorage::new();
        let mut table = Table::new("users", schema, storage);

        let row = Row::new(vec![Value::Null, Value::Text("Alice".to_string())]);
        let result = table.insert(row);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("NULL"));
    }

    #[test]
    fn test_table_insert_empty_row_error() {
        let schema = Schema::new(vec![
            Column::new("id", DataType::Integer),
            Column::new("name", DataType::Text),
        ]);
        let storage = MemoryStorage::new();
        let mut table = Table::new("users", schema, storage);

        let row = Row::new(vec![]);
        let result = table.insert(row);
        assert!(result.is_err());
    }

    #[test]
    fn test_table_multiple_inserts() {
        let storage = MemoryStorage::new();
        let schema = create_test_schema();
        let mut table = Table::new("users", schema, storage);

        for i in 0..100 {
            let row = Row::new(vec![Value::Integer(i), Value::Text(format!("User{}", i))]);
            table.insert(row).unwrap();
        }

        assert_eq!(table.len(), 100);
        assert_eq!(table.scan().len(), 100);
    }

    #[test]
    fn test_table_flush() {
        let storage = MemoryStorage::new();
        let schema = create_test_schema();
        let mut table = Table::new("users", schema, storage);

        let row = Row::new(vec![Value::Integer(1), Value::Text("Alice".to_string())]);
        table.insert(row).unwrap();

        table.flush();
        assert_eq!(table.len(), 1);
    }
}
