//! B+Tree 模組
//!
//! 提供 sql6 資料庫的核心索引結構。

//! use sql6::btree::{BPlusTree, Key};
//!
//! let mut tree = BPlusTree::new(4);
//! tree.insert(Key::Integer(42), b"hello".to_vec());
//! assert_eq!(tree.search(&Key::Integer(42)), Some(b"hello".as_slice()));
//! ```

pub mod node;
pub mod tree;

