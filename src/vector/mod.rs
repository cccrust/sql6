//! sql6 向量搜尋模組
//!
//! 支援近似最近鄰（ANN）查詢，參考 sqlite-vec API 設計。
//!
//! # 向量表
//!
//! ```sql
//! CREATE VIRTUAL TABLE items USING vec0(
//!   embedding float[768],
//!   id integer,
//!   category text,
//!   +description text
//! );
//! ```
//!
//! # 向量操作
//!
//! ```sql
//! -- 插入向量
//! INSERT INTO items(rowid, embedding) VALUES (1, '[0.1, 0.2, ...]');
//!
//! -- KNN 搜尋
//! SELECT rowid, distance FROM items
//! WHERE embedding match '[0.1, 0.2, ...]'
//! ORDER BY distance LIMIT 10;
//!
//! -- 距離計算
//! SELECT vec_distance_L2('[1,2,3]', '[4,5,6]');
//! ```

#![allow(dead_code, unused)]

pub mod vector;
pub mod distance;
pub mod vec_table;
pub mod functions;

pub use vector::VectorType;
pub use distance::{distance_l2, distance_cosine, distance_hamming};
pub use vec_table::{VecTable, ColumnDef, parse_columns, parse_vector_value};
pub use functions::*;