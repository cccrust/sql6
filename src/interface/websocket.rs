//! WebSocket Server：支援多客戶端的非同步伺服器
//!
//! 使用方式：
//! ```bash
//! # 啟動伺服器
//! sql6 --websocket 8080 mydb.db

//! from sql6 import connect
//! conn = connect("mydb.db", transport="websocket", port=8080)
//! cursor = conn.execute("SELECT * FROM users")
//! ```
//!
//! 協定格式（與 stdio server 相同）：
//! - 請求：{"method": "execute", "sql": "..."}
//! - 回應：{"ok": true, "columns": [...], "rows": [...], "affected": N}

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

// 非同步 WebSocket 處理
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_tungstenite::{accept_async, tungstenite::Message};

use crate::fts::FtsTable;
use crate::vector::vec_table::{VecTable, parse_columns, parse_vector_value, ColumnDef};
use crate::vector::vector::VectorType;
use crate::parser::parse;
use crate::planner::planner::Planner;
use crate::planner::{Executor, ResultSet};
use crate::table::row::Value;

// ============================================================================
// WsServer：WebSocket 伺服器主結構
// ============================================================================

/// WebSocket 伺服器
///
/// 支援多客戶端並發連線，每個連線獨立處理請求。
/// 使用 tokio 异步執行環境和 broadcast channel 實現優雅關閉。
pub struct WsServer {
    /// 查詢執行器（跨連線共享）
    executor: Arc<Mutex<Executor>>,
    /// FTS5 虛擬表格集合（跨連線共享）
    fts_tables: Arc<Mutex<HashMap<String, FtsTable>>>,
    /// vec0 向量表格集合（跨連線共享）
    vec_tables: Arc<Mutex<HashMap<String, VecTable>>>,
    /// 資料庫檔案路徑（若有）
    db_path: Option<String>,
    /// 廣播通道用於觸發關閉
    shutdown: broadcast::Sender<()>,
}

// ============================================================================
// WsServer 實作
// ============================================================================

impl WsServer {
/// 建立記憶體模式的 WebSocket 伺服器
    pub fn new() -> Self {
        let (shutdown, _) = broadcast::channel(1);
        WsServer {
            executor: Arc::new(Mutex::new(Executor::new())),
            fts_tables: Arc::new(Mutex::new(HashMap::new())),
            vec_tables: Arc::new(Mutex::new(HashMap::new())),
            db_path: None,
            shutdown,
        }
    }

    /// 開啟帶有資料庫檔案的 WebSocket 伺服器
    pub fn open<P: AsRef<std::path::Path>>(path: P) -> std::io::Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();
        let executor = Executor::with_disk(&path_str)?;
        let (shutdown, _) = broadcast::channel(1);
        Ok(WsServer {
            executor: Arc::new(Mutex::new(executor)),
            fts_tables: Arc::new(Mutex::new(HashMap::new())),
            vec_tables: Arc::new(Mutex::new(HashMap::new())),
            db_path: Some(path_str),
            shutdown,
        })
    }

    /// 啟動 WebSocket 伺服器並監聽連線
    ///
    /// 使用 tokio 异步 runtime，支援：
    /// - 接受新的 TCP 連線
    /// - 優雅關閉（透過 shutdown channel）
    pub async fn run(&mut self, port: u16) -> std::io::Result<()> {
        let addr = format!("127.0.0.1:{}", port);
        let listener = TcpListener::bind(&addr).await?;
        println!("WebSocket 伺服器監聽中：ws://{}", addr);

        // 訂閱關閉信號
        let mut shutdown_rx = self.shutdown.subscribe();

        loop {
            tokio::select! {
                // 接受新的 TCP 連線
                result = listener.accept() => {
                    match result {
                        Ok((stream, addr)) => {
                            // 克隆 Arc 以共享給每個連線的處理任務
                            let executor = Arc::clone(&self.executor);
                            let fts_tables = Arc::clone(&self.fts_tables);
                            let vec_tables = Arc::clone(&self.vec_tables);
                            // 為每個連線 spawn 一個獨立的非同步任務
                            tokio::spawn(handle_connection(stream, addr, executor, fts_tables, vec_tables));
                        }
                        Err(e) => {
                            eprintln!("接受連線錯誤：{}", e);
                        }
                    }
                }
                // 接收關閉信號
                _ = shutdown_rx.recv() => {
                    println!("正在關閉 WebSocket 伺服器");
                    break;
                }
            }
        }
        Ok(())
    }

    /// 發送關閉信號，終止伺服器
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(());
    }
}

// ============================================================================
// 連線處理
// ============================================================================

/// 處理單個 WebSocket 連線
///
/// 每個連線獨立运行，使用 spawn_blocking 將 SQL 執行交給執行緒池處理，
/// 避免阻塞非同步任務。
async fn handle_connection(
    stream: TcpStream,
    addr: SocketAddr,
    executor: Arc<Mutex<Executor>>,
    fts_tables: Arc<Mutex<HashMap<String, FtsTable>>>,
    vec_tables: Arc<Mutex<HashMap<String, VecTable>>>,
) {
    println!("收到來自 {} 的 WebSocket 連線", addr);

    // 執行 WebSocket 握手
    let ws_stream = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("WebSocket 握手失敗：{}", e);
            return;
        }
    };

    // 分割為讀寫半雙工
    let (mut write, mut read) = ws_stream.split();

    // 發送就緒信號
    let _ = write.send(Message::Text(r#"{"ok":true,"ready":true}"#.to_string())).await;

    // 處理訊息迴圈
    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                let executor = Arc::clone(&executor);
                let fts_tables = Arc::clone(&fts_tables);
                let vec_tables = Arc::clone(&vec_tables);
                // 在執行緒池中處理請求，避免阻塞
                let response = tokio::task::spawn_blocking(move || {
                    process_request(&text, &executor, &fts_tables, &vec_tables)
                }).await.unwrap_or_else(|_| r#"{"ok":false,"error":"task error"}"#.to_string());
                let _ = write.send(Message::Text(response)).await;
            }
            Ok(Message::Close(_)) => {
                println!("客戶端 {} 斷開連線", addr);
                break;
            }
            Err(e) => {
                eprintln!("讀取錯誤（{}）：{}", addr, e);
                break;
            }
            _ => {}
        }
    }
}

fn process_request(
    line: &str,
    executor: &Arc<Mutex<Executor>>,
    fts_tables: &Arc<Mutex<HashMap<String, FtsTable>>>,
    vec_tables: &Arc<Mutex<HashMap<String, VecTable>>>,
) -> String {
    let request: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return format!(r#"{{"ok":false,"error":"json parse error: {}"}}"#, e),
    };

    let method = match request.get("method").and_then(|v| v.as_str()) {
        Some(m) => m,
        None => return r#"{"ok":false,"error":"missing method"}"#.to_string(),
    };

    match method {
        "execute" => {
            let sql = match request.get("sql").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => return r#"{"ok":false,"error":"missing sql"}"#.to_string(),
            };
            execute_sql(sql, executor, fts_tables, vec_tables)
        }
        "close" => {
            r#"{"ok":true}"#.to_string()
        }
        _ => format!(r#"{{"ok":false,"error":"unknown method: {}"}}"#, method),
    }
}

fn execute_sql(
    sql: &str,
    executor: &Arc<Mutex<Executor>>,
    fts_tables: &Arc<Mutex<HashMap<String, FtsTable>>>,
    vec_tables: &Arc<Mutex<HashMap<String, VecTable>>>,
) -> String {
    let upper = sql.trim().to_uppercase();

    // CREATE VIRTUAL TABLE ... USING vec0(...)
    if upper.contains("USING VEC0") {
        return vec_create(sql, vec_tables);
    }

    // INSERT INTO vec0 表格
    if upper.starts_with("INSERT INTO") {
        if let Some(name) = extract_table_name_from_insert(sql) {
            if vec_tables.lock().unwrap().contains_key(&name) {
                return vec_insert(sql, &name, vec_tables);
            }
        }
    }

    // vec0 KNN 查詢
    if upper.contains("MATCH") {
        if let Some((name, query)) = extract_match_query(sql) {
            if vec_tables.lock().unwrap().contains_key(&name) {
                return vec_search(&name, &query, vec_tables);
            }
        }
    }

    // CREATE VIRTUAL TABLE ... USING FTS5
    if upper.starts_with("CREATE VIRTUAL TABLE") && upper.contains("USING FTS5") {
        return fts_create(sql, fts_tables);
    }

    if let Some(name) = extract_table_name_from_insert(sql) {
        if fts_tables.lock().unwrap().contains_key(&name) {
            return fts_insert(sql, &name, fts_tables);
        }
    }

    if upper.contains("MATCH") {
        if let Some((name, query)) = extract_match_query(sql) {
            if fts_tables.lock().unwrap().contains_key(&name) {
                return fts_select(&name, &query, fts_tables);
            }
        }
    }

    let stmts = match parse(sql) {
        Ok(s) => s,
        Err(e) => return format!(r#"{{"ok":false,"error":"parse error: {}"}}"#, e),
    };

    let mut last_result: Option<String> = None;
    for stmt in stmts {
        let mut exec = executor.lock().unwrap();
        let plan = match Planner::new(exec.catalog()).plan(stmt) {
            Ok(p) => p,
            Err(e) => return format!(r#"{{"ok":false,"error":"plan error: {}"}}"#, e),
        };

        match exec.execute(plan) {
            Ok(rs) => {
                last_result = Some(resultset_to_json(&rs));
            }
            Err(e) => return format!(r#"{{"ok":false,"error":"execution error: {}"}}"#, e),
        }
    }

    last_result.unwrap_or_else(|| r#"{"ok":true,"columns":[],"rows":[],"affected":0}"#.to_string())
}

fn resultset_to_json(rs: &ResultSet) -> String {
    let columns: Vec<String> = rs.columns.clone();
    let rows: Vec<Vec<serde_json::Value>> = rs.rows.iter().map(|row| {
        row.iter().map(|v| value_to_json(v)).collect()
    }).collect();

    let json = serde_json::json!({
        "ok": true,
        "columns": columns,
        "rows": rows,
        "affected": 0
    });
    serde_json::to_string(&json).unwrap_or_else(|_| r#"{"ok":false,"error":"json serialization error"}"#.to_string())
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Integer(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Text(s) => serde_json::Value::String(s.clone()),
        Value::Boolean(b) => serde_json::Value::Bool(*b),
    }
}

fn fts_create(sql: &str, fts_tables: &Arc<Mutex<HashMap<String, FtsTable>>>) -> String {
    let lower = sql.to_lowercase();
    let after_table = match lower.find("table") {
        Some(p) => p + 5,
        None => return r#"{"ok":false,"error":"parse error"}"#.to_string(),
    };
    let after_using = match lower.find("using") {
        Some(p) => p,
        None => return r#"{"ok":false,"error":"parse error"}"#.to_string(),
    };
    let name = sql[after_table..after_using].trim().to_string();

    let after_fts5 = match lower.find("fts5") {
        Some(p) => p + 4,
        None => return r#"{"ok":false,"error":"parse error"}"#.to_string(),
    };
    let lparen = match sql[after_fts5..].find('(') {
        Some(p) => p + after_fts5,
        None => return r#"{"ok":false,"error":"parse error"}"#.to_string(),
    };
    let rparen = match sql.rfind(')') {
        Some(p) => p,
        None => return r#"{"ok":false,"error":"parse error"}"#.to_string(),
    };
    let cols_str = &sql[lparen + 1..rparen];
    let columns: Vec<String> = cols_str.split(',')
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();

    if fts_tables.lock().unwrap().contains_key(&name) {
        return format!(r#"{{"ok":false,"error":"FTS table '{}' already exists"}}"#, name);
    }
    fts_tables.lock().unwrap().insert(name.clone(), FtsTable::new(&name, columns));
    format!(r#"{{"ok":true,"columns":[],"rows":[],"affected":1}}"#)
}

fn fts_insert(sql: &str, table_name: &str, fts_tables: &Arc<Mutex<HashMap<String, FtsTable>>>) -> String {
    let lower = sql.to_lowercase();
    let after_values = match lower.find("values") {
        Some(p) => p + 6,
        None => return r#"{"ok":false,"error":"parse error"}"#.to_string(),
    };
    let lparen = match sql[after_values..].find('(') {
        Some(p) => p + after_values,
        None => return r#"{"ok":false,"error":"parse error"}"#.to_string(),
    };
    let rparen = match sql.rfind(')') {
        Some(p) => p,
        None => return r#"{"ok":false,"error":"parse error"}"#.to_string(),
    };
    let vals_str = &sql[lparen + 1..rparen];
    let values: Vec<String> = split_sql_values(vals_str);

    if let Some(tbl) = fts_tables.lock().unwrap().get_mut(table_name) {
        tbl.insert(values);
        format!(r#"{{"ok":true,"columns":[],"rows":[],"affected":1}}"#)
    } else {
        format!(r#"{{"ok":false,"error":"table '{}' not found"}}"#, table_name)
    }
}

fn fts_select(table_name: &str, query: &str, fts_tables: &Arc<Mutex<HashMap<String, FtsTable>>>) -> String {
    let mut fts = fts_tables.lock().unwrap();
    let tbl = match fts.get_mut(table_name) {
        Some(t) => t,
        None => return format!(r#"{{"ok":false,"error":"table '{}' not found"}}"#, table_name),
    };
    let results = tbl.search(query);
    let col_names = tbl.columns.clone();

    let mut out_cols = vec!["rowid".to_string(), "score".to_string()];
    out_cols.extend(col_names);

    let rows: Vec<Vec<serde_json::Value>> = results.into_iter().map(|(rowid, score, vals)| {
        let mut row = vec![serde_json::Value::Number(rowid.into()), serde_json::Number::from_f64(score).map(|n| serde_json::Value::Number(n)).unwrap_or(serde_json::Value::Null)];
        row.extend(vals.into_iter().map(|v| serde_json::Value::String(v)));
        row
    }).collect();

    let json = serde_json::json!({
        "ok": true,
        "columns": out_cols,
        "rows": rows,
        "affected": 0
    });
    serde_json::to_string(&json).unwrap_or_else(|_| r#"{"ok":false,"error":"json error"}"#.to_string())
}

fn extract_table_name_from_insert(sql: &str) -> Option<String> {
    let lower = sql.to_lowercase();
    let after_into = lower.find("into")? + 4;
    let rest = sql[after_into..].trim();
    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    if name.is_empty() { None } else { Some(name) }
}

fn extract_match_query(sql: &str) -> Option<(String, String)> {
    // vec0 語法: SELECT * FROM t WHERE col MATCH 'query'
    // FTS5 語法: SELECT * FROM t WHERE t MATCH 'query'
    let lower = sql.to_lowercase();
    let match_pos = lower.find("match")?;
    let after_match = sql[match_pos + 5..].trim();

    // 先嘗試 FROM 子句中的表名（vec0 用這個）
    if let Some(from_pos) = lower.find("from") {
        let after_from = sql[from_pos + 4..].trim_start();
        let table_name: String = after_from.chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !table_name.is_empty() {
            let query = after_match.trim_matches(|c| c == '\'' || c == '"' || c == ';').to_string();
            if !query.is_empty() {
                return Some((table_name, query));
            }
        }
    }

    // 向後相容：取得 MATCH 前的表名（FTS5 用這個）
    let where_pos = lower.find("where")?;
    let between = sql[where_pos + 5..match_pos].trim();
    let table_name: String = between.chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    let query = after_match.trim_matches(|c| c == '\'' || c == '"' || c == ';').to_string();
    if table_name.is_empty() || query.is_empty() { None } else { Some((table_name, query)) }
}

fn vec_create(sql: &str, vec_tables: &Arc<Mutex<HashMap<String, VecTable>>>) -> String {
    let lower = sql.to_lowercase();
    let after_table = match lower.find("table") {
        Some(p) => p + 5,
        None => return r#"{"ok":false,"error":"parse error"}"#.to_string(),
    };
    let after_using = match lower.find("using") {
        Some(p) => p,
        None => return r#"{"ok":false,"error":"parse error"}"#.to_string(),
    };
    let name = sql[after_table..after_using].trim().to_string();

    let after_vec0 = match lower.find("vec0") {
        Some(p) => p + 4,
        None => return r#"{"ok":false,"error":"parse error"}"#.to_string(),
    };
    let lparen = match sql[after_vec0..].find('(') {
        Some(p) => p + after_vec0,
        None => return r#"{"ok":false,"error":"parse error"}"#.to_string(),
    };
    let rparen = match sql.rfind(')') {
        Some(p) => p,
        None => return r#"{"ok":false,"error":"parse error"}"#.to_string(),
    };
    let cols_str = &sql[lparen + 1..rparen];

    match parse_columns(cols_str) {
        Ok(columns) => {
            if vec_tables.lock().unwrap().contains_key(&name) {
                return format!(r#"{{"ok":false,"error":"vec0 table '{}' already exists"}}"#, name);
            }
            vec_tables.lock().unwrap().insert(name.clone(), VecTable::new(&name, columns));
            format!(r#"{{"ok":true,"columns":[],"rows":[],"affected":1}}"#)
        }
        Err(e) => format!(r#"{{"ok":false,"error":"{}"}}"#, e),
    }
}

fn vec_insert(sql: &str, table_name: &str, vec_tables: &Arc<Mutex<HashMap<String, VecTable>>>) -> String {
    let lower = sql.to_lowercase();
    let after_into = match lower.find("into") {
        Some(p) => p + 4,
        None => return r#"{"ok":false,"error":"parse error"}"#.to_string(),
    };
    let after_name = after_into + table_name.len() + 1;
    let rest_lower = &lower[after_name..];
    let rest_original = &sql[after_name..];

    let lparen = match rest_original.find('(') {
        Some(p) => p,
        None => return r#"{"ok":false,"error":"no column list"}"#.to_string(),
    };
    let rparen = match rest_original.find(')') {
        Some(p) => p,
        None => return r#"{"ok":false,"error":"no values"}"#.to_string(),
    };
    let col_list = &rest_original[lparen+1..rparen];
    let cols: Vec<&str> = col_list.split(',').map(|s| s.trim()).collect();

    let values_pos = match rest_lower.find("values") {
        Some(p) => p,
        None => return r#"{"ok":false,"error":"parse error"}"#.to_string(),
    };
    let after_values = values_pos + 6;
    let lparen2 = match rest_original[after_values..].find('(') {
        Some(p) => p + after_values,
        None => return r#"{"ok":false,"error":"parse error"}"#.to_string(),
    };
    let rparen2 = match rest_original.rfind(')') {
        Some(p) => p,
        None => return r#"{"ok":false,"error":"parse error"}"#.to_string(),
    };
    let vals_str = &rest_original[lparen2+1..rparen2];
    let values: Vec<String> = split_sql_values(vals_str);

    if values.len() != cols.len() {
        return r#"{"ok":false,"error":"column count mismatch"}"#.to_string();
    }

    let mut vec_tables = vec_tables.lock().unwrap();
    let table = match vec_tables.get_mut(table_name) {
        Some(t) => t,
        None => return format!(r#"{{"ok":false,"error":"table '{}' not found"}}"#, table_name),
    };

    let vector_col_idx = table.columns.iter()
        .position(|c| matches!(c, ColumnDef::Vector { .. }));

    let mut rowid: Option<u64> = None;
    let mut vector: Option<VectorType> = None;
    let mut metadata = std::collections::HashMap::new();
    let mut auxiliary = std::collections::HashMap::new();

    for (i, col) in cols.iter().enumerate() {
        let value = &values[i];
        let col_lower = col.to_lowercase();

        if col_lower == "rowid" {
            match value.parse() {
                Ok(id) => rowid = Some(id),
                Err(_) => return r#"{"ok":false,"error":"invalid rowid"}"#.to_string(),
            }
        } else if let Some(vec_idx) = vector_col_idx {
            if let ColumnDef::Vector { name, .. } = &table.columns[vec_idx] {
                if col_lower == name.to_lowercase() {
                    match parse_vector_value(value) {
                        Ok(v) => vector = Some(v),
                        Err(e) => return format!(r#"{{"ok":false,"error":"{}"}}"#, e),
                    }
                    continue;
                }
            }
        }

        for col_def in &table.columns {
            match col_def {
                ColumnDef::Metadata { name, .. } if name.to_lowercase() == col_lower => {
                    metadata.insert(name.clone(), value.trim_matches('\'').to_string());
                }
                ColumnDef::Auxiliary { name, .. } if name.to_lowercase() == col_lower => {
                    auxiliary.insert(name.clone(), value.trim_matches('\'').to_string());
                }
                ColumnDef::PartitionKey { name, .. } if name.to_lowercase() == col_lower => {
                    metadata.insert(name.clone(), value.trim_matches('\'').to_string());
                }
                _ => {}
            }
        }
    }

    let vector = match vector {
        Some(v) => v,
        None => return r#"{"ok":false,"error":"vector column not found"}"#.to_string(),
    };

    table.insert(rowid, vector, metadata, auxiliary);
    format!(r#"{{"ok":true,"columns":[],"rows":[],"affected":1}}"#)
}

fn vec_search(table_name: &str, query: &str, vec_tables: &Arc<Mutex<HashMap<String, VecTable>>>) -> String {
    let mut vec_tables = vec_tables.lock().unwrap();
    let table = match vec_tables.get_mut(table_name) {
        Some(t) => t,
        None => return format!(r#"{{"ok":false,"error":"table '{}' not found"}}"#, table_name),
    };

    let query_vector = match parse_vector_value(query.trim().trim_matches('\'')) {
        Ok(v) => v,
        Err(e) => return format!(r#"{{"ok":false,"error":"{}"}}"#, e),
    };

    let results = table.search(&query_vector, 10, &std::collections::HashMap::new());

    let rows: Vec<Vec<serde_json::Value>> = results.into_iter().map(|(rowid, distance)| {
        vec![
            serde_json::Value::Number(rowid.into()),
            serde_json::Value::Number(serde_json::Number::from_f64(distance).unwrap_or(serde_json::Number::from(0))),
        ]
    }).collect();

    let json = serde_json::json!({
        "ok": true,
        "columns": vec!["rowid", "distance"],
        "rows": rows,
        "affected": 0
    });
    serde_json::to_string(&json).unwrap_or_else(|_| r#"{"ok":false,"error":"json error"}"#.to_string())
}

fn split_sql_values(s: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut quote_char = ' ';

    for c in s.chars() {
        match c {
            '\'' | '"' if !in_quote => { in_quote = true; quote_char = c; }
            c if in_quote && c == quote_char => { in_quote = false; }
            ',' if !in_quote => {
                result.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        result.push(current.trim().to_string());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_table_name_from_insert() {
        assert_eq!(extract_table_name_from_insert("INSERT INTO users VALUES (1, 'a')"), Some("users".to_string()));
        assert_eq!(extract_table_name_from_insert("INSERT INTO my_table (id) VALUES (1)"), Some("my_table".to_string()));
        assert_eq!(extract_table_name_from_insert("INSERT INTO users VALUES (1, 'a')"), Some("users".to_string()));
    }

    #[test]
    fn test_extract_table_name_from_insert_case_insensitive() {
        assert_eq!(extract_table_name_from_insert("insert into users values (1)"), Some("users".to_string()));
    }

    #[test]
    fn test_extract_table_name_from_insert_not_found() {
        assert_eq!(extract_table_name_from_insert("SELECT * FROM users"), None);
    }

    #[test]
    fn test_extract_match_query() {
        let result = extract_match_query("SELECT * FROM articles WHERE articles MATCH 'rust'");
        assert!(result.is_some());
        let (table, query) = result.unwrap();
        assert_eq!(table, "articles");
        assert_eq!(query, "rust");
    }

    #[test]
    fn test_extract_match_query_with_quotes() {
        let result = extract_match_query("SELECT * FROM articles WHERE articles MATCH '中文'");
        assert!(result.is_some());
        let (_, query) = result.unwrap();
        assert_eq!(query, "中文");
    }

    #[test]
    fn test_extract_match_query_not_found() {
        assert_eq!(extract_match_query("SELECT * FROM users"), None);
    }

    #[test]
    fn test_split_sql_values_simple() {
        let result = split_sql_values("1, 2, 3");
        assert_eq!(result, vec!["1", "2", "3"]);
    }

#[test]
    fn test_split_sql_values_with_strings() {
        let result = split_sql_values("'a', 'b', 'c'");
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_split_sql_values_quoted_comma() {
        let result = split_sql_values("'hello, world', 'foo'");
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_split_sql_values_empty() {
        let result = split_sql_values("");
        assert!(result.is_empty());
    }

    #[test]
    fn test_split_sql_values_single_value() {
        let result = split_sql_values("42");
        assert_eq!(result, vec!["42"]);
    }

    #[test]
    fn test_split_sql_values_with_spaces() {
        let result = split_sql_values("  1  ,  2  ,  3  ");
        assert_eq!(result, vec!["1", "2", "3"]);
    }

    #[test]
    fn test_split_sql_values_double_quotes() {
        let result = split_sql_values("\"a\", \"b\"");
        assert_eq!(result.len(), 2);
    }
}