//! REPL：互動式 SQL 命令列介面
//!
//! 功能：
//!   - 多行輸入（以 ; 結尾才送出）
//!   - 點指令（.help / .tables / .schema / .quit / .fts）
//!   - 結果以對齊表格輸出
//!   - 執行時間顯示
//!   - 錯誤訊息友善顯示

use std::io::{self, BufRead, Write};
use std::time::Instant;
use std::collections::HashMap;

use crate::fts::FtsTable;
use crate::vector::vec_table::{VecTable, parse_columns, parse_vector_value, ColumnDef};
use crate::vector::vector::VectorType;
use crate::vector::functions;
use crate::parser::parse;
use crate::planner::planner::Planner;
use crate::planner::{Executor, ResultSet};
use crate::table::row::Value;

// ── REPL ─────────────────────────────────────────────────────────────────

pub struct Repl {
    executor:   Executor,
    fts_tables: HashMap<String, FtsTable>,
    vec_tables: HashMap<String, VecTable>,
    prompt:     &'static str,
    history:    Vec<String>,
    db_path:    Option<String>,
    trace:      bool,
}

impl Repl {
    /// 建立記憶體資料庫
    pub fn new() -> Self {
        Repl {
            executor:   Executor::new(),
            fts_tables: HashMap::new(),
            vec_tables: HashMap::new(),
            prompt:     "sql6> ",
            history:    Vec::new(),
            db_path:    None,
            trace:      false,
        }
    }

    /// 開啟磁碟資料庫
    pub fn open<P: AsRef<std::path::Path>>(path: P) -> std::io::Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();
        let executor = Executor::with_disk(&path_str)?;
        Ok(Repl {
            executor,
            fts_tables: HashMap::new(),
            vec_tables: HashMap::new(),
            prompt:     "sql6> ",
            history:    Vec::new(),
            db_path:    Some(path_str),
            trace:      false,
        })
    }

    /// 關閉資料庫（flush 到磁碟）
    pub fn close(&mut self) {
        if self.db_path.is_some() {
            self.executor.flush();
        }
    }

    /// 啟動互動式 REPL（從 stdin 讀取）
    pub fn run(&mut self) {
        self.print_banner();
        let stdin  = io::stdin();
        let mut buf = String::new();

        loop {
            // 顯示提示符
            if buf.trim().is_empty() {
                print!("{}", self.prompt);
            } else {
                print!("   ...> ");
            }
            io::stdout().flush().unwrap();

            let mut line = String::new();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => break,          // EOF
                Ok(_) => {}
                Err(e) => { eprintln!("read error: {}", e); break; }
            }

            let trimmed = line.trim_end().to_string();

            // 點指令（立即執行，不需要 ;）
            if trimmed.starts_with('.') {
                if self.handle_dot_command(&trimmed) {
                    break;  // quit 命令
                }
                buf.clear();
                continue;
            }

            buf.push_str(&trimmed);
            buf.push(' ');

            // 以 ; 判斷語句結束
            if trimmed.ends_with(';') || is_complete(&buf) {
                let sql = buf.trim().to_string();
                if !sql.is_empty() {
                    self.history.push(sql.clone());
                    self.execute_sql(&sql);
                }
                buf.clear();
            }
        }
        println!("\nBye!");
    }

    /// 執行單一 SQL 字串（非互動式，用於腳本 / 測試）
    pub fn execute_sql(&mut self, sql: &str) {
        let start = Instant::now();
        self.history.push(sql.trim_end_matches(';').trim().to_string());
        if self.trace { println!("[trace] {}", sql); }
        // 先嘗試攔截 FTS 特殊語法
        if let Some(result) = self.try_handle_fts(sql) {
            match result {
                Ok(rs)  => { print_result_set(&rs); println!("({:.3}s)", start.elapsed().as_secs_f64()); }
                Err(e)  => eprintln!("Error: {}", e),
            }
            return;
        }

        let stmts = match parse(sql) {
            Ok(s)  => s,
            Err(e) => { eprintln!("Parse error: {}", e); return; }
        };

        for stmt in stmts {
            let plan = match Planner::new(self.executor.catalog()).plan(stmt) {
                Ok(p)  => p,
                Err(e) => { eprintln!("Plan error: {}", e); return; }
            };
            match self.executor.execute(plan) {
                Ok(rs) => {
                    print_result_set(&rs);
                    println!("({:.3}s)", start.elapsed().as_secs_f64());
                }
                Err(e) => eprintln!("Error: {}", e),
            }
        }
    }

    // ── 點指令 ────────────────────────────────────────────────────────────

    fn handle_dot_command(&mut self, cmd: &str) -> bool {
        let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
        match parts[0] {
            ".quit" | ".exit" | ".q" => {
                println!("Bye!");
                return true;
            }
            ".help" | ".h" => self.print_help(),
            ".tables"      => self.cmd_tables(),
            ".indices"     => self.cmd_indices(),
            ".databases"   => self.cmd_databases(),
            ".schema"      => self.cmd_schema(parts.get(1).copied()),
            ".fts"         => self.cmd_fts(parts.get(1).copied()),
            ".history"     => self.cmd_history(),
            ".trace"       => self.cmd_trace(),
            ".timing"      => println!("(timing always on)"),
            _ => eprintln!("Unknown command: {}  (type .help for help)", parts[0]),
        }
        false
    }

    fn cmd_tables(&self) {
        let mut names = self.executor.catalog().table_names();
        names.sort();
        if names.is_empty() {
            println!("(no tables)");
        } else {
            for n in names { println!("{}", n); }
        }
        // 也列出 FTS 虛擬表
        for name in self.fts_tables.keys() {
            println!("{} (fts)", name);
        }
    }

    fn cmd_indices(&self) {
        let names = self.executor.catalog().index_names();
        if names.is_empty() {
            println!("(no indices)");
        } else {
            for n in names { println!("{}", n); }
        }
    }

    fn cmd_databases(&self) {
        println!("main:");
        if let Some(path) = &self.db_path {
            println!("  {}", path);
        } else {
            println!("  (memory)");
        }
    }

    fn cmd_schema(&self, table: Option<&str>) {
        let catalog = self.executor.catalog();
        let names: Vec<&str> = match table {
            Some(t) => vec![t],
            None    => catalog.table_names(),
        };
        for name in names {
            if let Some(meta) = catalog.get_table(name) {
                println!("CREATE TABLE {} (", meta.name);
                let cols = &meta.schema.columns;
                for (i, col) in cols.iter().enumerate() {
                    let comma = if i + 1 < cols.len() { "," } else { "" };
                    println!("  {} {}{}", col.name, col.data_type, comma);
                }
                println!(");");
            } else {
                eprintln!("table '{}' not found", name);
            }
        }
        // 視圖
        if let Some(t) = table {
            if catalog.view_exists(t) {
                if let Some(view) = catalog.get_view(t) {
                    println!("CREATE VIEW {} AS {}", view.name, view.query);
                }
            }
        } else {
            for name in catalog.view_names() {
                if let Some(view) = catalog.get_view(name) {
                    println!("CREATE VIEW {} AS {}", view.name, view.query);
                }
            }
        }
        // FTS 虛擬表
        if let Some(t) = table.and_then(|n| self.fts_tables.get(n)) {
            println!("CREATE VIRTUAL TABLE {} USING fts5({});",
                t.name, t.columns.join(", "));
        }
    }

    fn cmd_trace(&mut self) {
        self.trace = !self.trace;
        println!("trace {}", if self.trace { "on" } else { "off" });
    }

    fn cmd_fts(&mut self, arg: Option<&str>) {
        // .fts <table> <query>
        let arg = match arg {
            Some(a) => a,
            None    => { eprintln!("Usage: .fts <table> <query>"); return; }
        };
        let (table_name, query) = match arg.splitn(2, ' ').collect::<Vec<_>>()[..] {
            [t, q] => (t, q),
            _      => { eprintln!("Usage: .fts <table> <query>"); return; }
        };
        let tbl = match self.fts_tables.get_mut(table_name) {
            Some(t) => t,
            None    => { eprintln!("FTS table '{}' not found", table_name); return; }
        };
        let results = tbl.search(query);
        if results.is_empty() {
            println!("(no results)");
            return;
        }
        // 顯示結果
        let header = format!("{:<8} {:<10} {}", "rowid", "score", tbl.columns.join(" | "));
        println!("{}", header);
        println!("{}", "-".repeat(header.len()));
        for (rowid, score, vals) in &results {
            println!("{:<8} {:<10.4} {}", rowid, score, vals.join(" | "));
        }
        println!("({} result{})", results.len(), if results.len() == 1 { "" } else { "s" });
    }

    fn cmd_history(&self) {
        for (i, h) in self.history.iter().enumerate() {
            println!("{:>3}  {}", i + 1, h);
        }
    }

    // ── FTS / vec0 SQL 攔截 ─────────────────────────────────────────────────
    // 處理 SQLite FTS5 相容語法：
    //   CREATE VIRTUAL TABLE t USING fts5(col1, col2)
    //   INSERT INTO t VALUES (...)
    //   SELECT * FROM t WHERE t MATCH 'query'
    //
    // vec0 向量表語法：
    //   CREATE VIRTUAL TABLE t USING vec0(col float[768])
    //   INSERT INTO t(rowid, col) VALUES (1, '[...]')
    //   SELECT * FROM t WHERE col MATCH '[...]' LIMIT 10

    fn try_handle_fts(&mut self, sql: &str) -> Option<Result<ResultSet, String>> {
        let upper = sql.trim().to_uppercase();

        // CREATE VIRTUAL TABLE ... USING vec0(...)
        if upper.contains("USING VEC0") || upper.contains("USING VEC0(") {
            return Some(self.vec_create(sql));
        }

        // INSERT INTO <vec_table> ...
        if upper.starts_with("INSERT INTO") {
            let table_name = extract_table_name_from_insert(sql)?;
            // 先檢查 vec0 表
            if self.vec_tables.contains_key(&table_name) {
                return Some(self.vec_insert(sql, &table_name));
            }
            // 再檢查 FTS 表
            if self.fts_tables.contains_key(&table_name) {
                return Some(self.fts_insert(sql, &table_name));
            }
        }

        // SELECT ... FROM <table> WHERE <col> MATCH '...' (vec0 KNN 查詢)
        if upper.contains("MATCH") {
            if let Some((table_name, query)) = extract_match_query(sql) {
                // 先檢查 vec0 表
                if self.vec_tables.contains_key(&table_name) {
                    return Some(self.vec_search(&table_name, &query));
                }
                // 再檢查 FTS 表
                if self.fts_tables.contains_key(&table_name) {
                    return Some(self.fts_select(&table_name, &query));
                }
            }
        }

        // 檢查是否為 vec0 表的 SELECT（可能是 FROM vec_table WHERE ...）
        if upper.starts_with("SELECT") && upper.contains("FROM") {
            if let Some(table_name) = extract_table_name_from_select(sql) {
                if self.vec_tables.contains_key(&table_name) {
                    return Some(self.vec_select(sql, &table_name));
                }
            }
        }

        // CREATE VIRTUAL TABLE ... USING fts5(...)
        if upper.starts_with("CREATE VIRTUAL TABLE") && upper.contains("FTS5") {
            return Some(self.fts_create(sql));
        }

        // 嘗試向量函數
        if let Some(result) = self.try_handle_vec_function(sql) {
            return Some(result);
        }

        None
    }

    fn fts_create(&mut self, sql: &str) -> Result<ResultSet, String> {
        // 解析：CREATE VIRTUAL TABLE <name> USING fts5(<col1>, <col2>, ...)
        let lower = sql.to_lowercase();
        let after_table = lower.find("table").ok_or("parse error")? + 5;
        let after_using = lower.find("using").ok_or("parse error")?;
        let name = sql[after_table..after_using].trim().to_string();

        let after_fts5 = lower.find("fts5").ok_or("parse error")? + 4;
        let lparen = sql[after_fts5..].find('(').ok_or("parse error")? + after_fts5;
        let rparen = sql.rfind(')').ok_or("parse error")?;
        let cols_str = &sql[lparen+1..rparen];
        let columns: Vec<String> = cols_str.split(',')
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect();

        if self.fts_tables.contains_key(&name) {
            return Err(format!("FTS table '{}' already exists", name));
        }
        self.fts_tables.insert(name.clone(), FtsTable::new(&name, columns));
        Ok(ResultSet::ok_msg("fts5 virtual table created"))
    }

    fn fts_insert(&mut self, sql: &str, table_name: &str) -> Result<ResultSet, String> {
        // 簡單解析 INSERT INTO t VALUES ('v1', 'v2', ...)
        let lower = sql.to_lowercase();
        let after_values = lower.find("values").ok_or("parse error")? + 6;
        let lparen = sql[after_values..].find('(').ok_or("parse error")? + after_values;
        let rparen = sql.rfind(')').ok_or("parse error")?;
        let vals_str = &sql[lparen+1..rparen];

        // 解析以逗號分隔的值（簡單版，不處理值內有逗號）
        let values: Vec<String> = split_sql_values(vals_str);
        let tbl = self.fts_tables.get_mut(table_name).ok_or("table not found")?;
        tbl.insert(values);
        Ok(ResultSet::ok_msg("1 row(s) inserted"))
    }

    fn fts_select(&mut self, table_name: &str, query: &str) -> Result<ResultSet, String> {
        let tbl = self.fts_tables.get_mut(table_name).ok_or("table not found")?;
        let results = tbl.search(query);
        let col_names = tbl.columns.clone();

        let mut out_cols = vec!["rowid".to_string(), "score".to_string()];
        out_cols.extend(col_names);

        let rows: Vec<Vec<Value>> = results.into_iter().map(|(rowid, score, vals)| {
            let mut row = vec![Value::Integer(rowid as i64), Value::Float(score)];
            row.extend(vals.into_iter().map(Value::Text));
            row
        }).collect();

        Ok(ResultSet { columns: out_cols, rows, affected: 0, lastrowid: None })
    }

    // ── vec0 向量表處理 ─────────────────────────────────────────────────

    fn vec_create(&mut self, sql: &str) -> Result<ResultSet, String> {
        // 解析：CREATE VIRTUAL TABLE <name> USING vec0(<col1>, <col2>, ...)
        let lower = sql.to_lowercase();
        let after_table = lower.find("table").ok_or("parse error")? + 5;
        let after_using = lower.find("using").ok_or("parse error")?;
        let name = sql[after_table..after_using].trim().to_string();

        // 找到 vec0(...)
        let after_vec0 = lower.find("vec0").ok_or("parse error")? + 4;
        let lparen = sql[after_vec0..].find('(').ok_or("parse error")? + after_vec0;
        let rparen = sql.rfind(')').ok_or("parse error")?;
        let cols_str = &sql[lparen+1..rparen];

        let columns = parse_columns(cols_str)?;
        self.vec_tables.insert(name.clone(), VecTable::new(&name, columns));
        Ok(ResultSet::ok_msg("vec0 virtual table created"))
    }

    fn vec_insert(&mut self, sql: &str, table_name: &str) -> Result<ResultSet, String> {
        let table = self.vec_tables.get_mut(table_name).ok_or("table not found")?;

        // 解析 INSERT INTO t(rowid, col) VALUES (1, '[...]')
        let lower = sql.to_lowercase();
        let after_into = lower.find("into").ok_or("parse error")? + 4;
        let after_name = after_into + table_name.len() + 1;
        // For searching, use lower; for extracting values, use original sql
        let rest_lower = &lower[after_name..];
        let rest_original = &sql[after_name..];

        // 解析 column list (use original to preserve case)
        let lparen = rest_original.find('(').ok_or("no column list")?;
        let rparen = rest_original.find(')').ok_or("no values")?;
        let col_list = &rest_original[lparen+1..rparen];
        let cols: Vec<&str> = col_list.split(',').map(|s| s.trim()).collect();

        // 解析 values (search in lower, extract from original)
        let values_pos = rest_lower.find("values");
        let after_values = values_pos.ok_or("parse error")? + 6;
        let original_after_values = after_values;
        let lparen2 = rest_original[original_after_values..].find('(');
        let lparen2 = lparen2.ok_or("parse error")? + original_after_values;
        let rparen2 = rest_original.rfind(')').ok_or("parse error")?;
        let vals_str = &rest_original[lparen2+1..rparen2];

        let values: Vec<String> = split_sql_values(vals_str);
        if values.len() != cols.len() {
            return Err("column count mismatch".to_string());
        }

        // 找出向量欄位和其他欄位
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
                rowid = Some(value.parse().map_err(|_| "invalid rowid")?);
            } else if let Some(vec_idx) = vector_col_idx {
                if let ColumnDef::Vector { name, .. } = &table.columns[vec_idx] {
                    if col_lower == name.to_lowercase() {
                        vector = Some(parse_vector_value(value)?);
                        continue;
                    }
                }
            }

            // 其他欄位視為元資料或輔助欄位
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

        let vector = vector.ok_or("vector column not found")?;
        table.insert(rowid, vector, metadata, auxiliary);
        Ok(ResultSet::ok_msg("1 row(s) inserted"))
    }

    fn vec_search(&mut self, table_name: &str, query: &str) -> Result<ResultSet, String> {
        let table: &mut VecTable = self.vec_tables.get_mut(table_name).ok_or("table not found")?;

        // 解析 query 為向量
        let query_vector = parse_vector_value(query.trim().trim_matches('\''))?;

        // 解析 LIMIT
        let k = 10;

        let results = table.search(&query_vector, k, &std::collections::HashMap::new());

        let rows: Vec<Vec<Value>> = results.into_iter().map(|(rowid, distance)| {
            vec![Value::Integer(rowid as i64), Value::Float(distance)]
        }).collect();

        Ok(ResultSet {
            columns: vec!["rowid".to_string(), "distance".to_string()],
            rows,
            affected: 0,
            lastrowid: None
        })
    }

    fn vec_select(&mut self, sql: &str, table_name: &str) -> Result<ResultSet, String> {
        let table = self.vec_tables.get(table_name).ok_or("table not found")?;
        let upper = sql.to_uppercase();

        // 解析 KNN 查詢：WHERE col MATCH '[...]' LIMIT N
        let mut query_vector: Option<VectorType> = None;
        let mut k = 10;

        // 找 MATCH 子句
        if let Some(match_pos) = upper.find("MATCH") {
            let after_match = &sql[match_pos + 5..];
            if let Some(quote_pos) = after_match.find('\'') {
                let after_quote = &after_match[quote_pos + 1..];
                if let Some(end_quote) = after_quote.find('\'') {
                    let query_str = &after_quote[..end_quote];
                    query_vector = Some(parse_vector_value(query_str)?);
                }
            }
        }

        // 找 LIMIT
        if let Some(limit_pos) = upper.find("LIMIT") {
            let after_limit = &sql[limit_pos + 5..];
            let limit_str = after_limit.split_whitespace().next().unwrap_or("10");
            k = limit_str.parse().unwrap_or(10);
        }

        let query_vector = query_vector.ok_or("no query vector found")?;
        let results = table.search(&query_vector, k, &std::collections::HashMap::new());

        // 建立結果
        let mut out_cols = vec!["rowid".to_string(), "distance".to_string()];

        // 加入元資料欄位名稱
        for col in &table.columns {
            match col {
                ColumnDef::Metadata { name, .. } => out_cols.push(name.clone()),
                ColumnDef::PartitionKey { name, .. } => out_cols.push(name.clone()),
                _ => {}
            }
        }

        let rows: Vec<Vec<Value>> = results.into_iter().map(|(rowid, distance)| {
            let mut row = vec![Value::Integer(rowid as i64), Value::Float(distance)];

            // 加入元資料
            if let Some(meta) = table.get_metadata(rowid) {
                for col in &table.columns {
                    match col {
                        ColumnDef::Metadata { name, .. } => {
                            row.push(Value::Text(meta.get(name).cloned().unwrap_or_default()));
                        }
                        ColumnDef::PartitionKey { name, .. } => {
                            row.push(Value::Text(meta.get(name).cloned().unwrap_or_default()));
                        }
                        _ => {}
                    }
                }
            }

            row
        }).collect();

        Ok(ResultSet { columns: out_cols, rows, affected: 0, lastrowid: None })
    }

    fn try_handle_vec_function(&mut self, sql: &str) -> Option<Result<ResultSet, String>> {
        let upper = sql.trim().to_uppercase();

        // vec_f32('[...]')
        if upper.starts_with("SELECT VEC_F32") {
            let start = sql.find('(')? + 1;
            let end = sql.rfind(')')?;
            let arg = &sql[start..end];
            match functions::vec_f32(arg) {
                Ok(hex) => Some(Ok(ResultSet {
                    columns: vec!["result".to_string()],
                    rows: vec![vec![Value::Text(hex)]],
                    affected: 0,
                    lastrowid: None,
                })),
                Err(e) => Some(Err(e)),
            }
        }
        // vec_to_json('...')
        else if upper.starts_with("SELECT VEC_TO_JSON") {
            let start = sql.find('(')? + 1;
            let end = sql.rfind(')')?;
            let arg = &sql[start..end];
            match functions::vec_to_json(arg) {
                Ok(json) => Some(Ok(ResultSet {
                    columns: vec!["result".to_string()],
                    rows: vec![vec![Value::Text(json)]],
                    affected: 0,
                    lastrowid: None,
                })),
                Err(e) => Some(Err(e)),
            }
        }
        // vec_length('...')
        else if upper.starts_with("SELECT VEC_LENGTH") {
            let start = sql.find('(')? + 1;
            let end = sql.rfind(')')?;
            let arg = &sql[start..end];
            match functions::vec_length(arg) {
                Ok(len) => Some(Ok(ResultSet {
                    columns: vec!["result".to_string()],
                    rows: vec![vec![Value::Integer(len as i64)]],
                    affected: 0,
                    lastrowid: None,
                })),
                Err(e) => Some(Err(e)),
            }
        }
        // vec_distance_L2(a, b)
        else if upper.starts_with("SELECT VEC_DISTANCE_L2") {
            let start = sql.find('(')? + 1;
            let end = sql.rfind(')')?;
            let args = &sql[start..end];
            let parts: Vec<&str> = args.split(',').collect();
            if parts.len() == 2 {
                match functions::vec_distance_l2(parts[0].trim(), parts[1].trim()) {
                    Ok(d) => Some(Ok(ResultSet {
                        columns: vec!["distance".to_string()],
                        rows: vec![vec![Value::Float(d)]],
                        affected: 0,
                        lastrowid: None,
                    })),
                    Err(e) => Some(Err(e)),
                }
            } else {
                None
            }
        }
        // vec_distance_cosine(a, b)
        else if upper.starts_with("SELECT VEC_DISTANCE_COSINE") {
            let start = sql.find('(')? + 1;
            let end = sql.rfind(')')?;
            let args = &sql[start..end];
            let parts: Vec<&str> = args.split(',').collect();
            if parts.len() == 2 {
                match functions::vec_distance_cosine(parts[0].trim(), parts[1].trim()) {
                    Ok(d) => Some(Ok(ResultSet {
                        columns: vec!["distance".to_string()],
                        rows: vec![vec![Value::Float(d)]],
                        affected: 0,
                        lastrowid: None,
                    })),
                    Err(e) => Some(Err(e)),
                }
            } else {
                None
            }
        }
        // vec_normalize('...')
        else if upper.starts_with("SELECT VEC_NORMALIZE") {
            let start = sql.find('(')? + 1;
            let end = sql.rfind(')')?;
            let arg = &sql[start..end];
            match functions::vec_normalize(arg) {
                Ok(hex) => Some(Ok(ResultSet {
                    columns: vec!["result".to_string()],
                    rows: vec![vec![Value::Text(hex)]],
                    affected: 0,
                    lastrowid: None,
                })),
                Err(e) => Some(Err(e)),
            }
        }
        // vec_quantize_binary('...')
        else if upper.starts_with("SELECT VEC_QUANTIZE_BINARY") {
            let start = sql.find('(')? + 1;
            let end = sql.rfind(')')?;
            let arg = &sql[start..end];
            match functions::vec_quantize_binary(arg) {
                Ok(hex) => Some(Ok(ResultSet {
                    columns: vec!["result".to_string()],
                    rows: vec![vec![Value::Text(hex)]],
                    affected: 0,
                    lastrowid: None,
                })),
                Err(e) => Some(Err(e)),
            }
        }
        else {
            None
        }
    }

    // ── Banner & Help ────────────────────────────────────────────────────

    fn print_banner(&self) {
        println!("sql6 v6.0.0 — SQLite-compatible database with FTS and vector similarity");
        println!("Type .help for help, .quit to exit");
        println!();
    }

    fn print_help(&self) {
        println!("Commands:");
        println!("  .help            Show this help");
        println!("  .tables          List all tables");
        println!("  .schema [TABLE]  Show CREATE statement");
        println!("  .fts TABLE QUERY Full-text search");
        println!("  .history         Show command history");
        println!("  .quit            Exit");
        println!();
        println!("SQL Examples:");
        println!("  CREATE TABLE users (id INTEGER, name TEXT, age INTEGER);");
        println!("  INSERT INTO users VALUES (1, 'Alice', 30);");
        println!("  SELECT * FROM users WHERE age > 25 ORDER BY name;");
        println!("  UPDATE users SET age = 31 WHERE id = 1;");
        println!("  DELETE FROM users WHERE id = 1;");
        println!();
        println!("FTS Examples:");
        println!("  CREATE VIRTUAL TABLE articles USING fts5(title, body);");
        println!("  INSERT INTO articles VALUES ('Rust lang', 'Fast systems');");
        println!("  SELECT * FROM articles WHERE articles MATCH 'rust';");
        println!("  SELECT * FROM articles WHERE articles MATCH '\"rust lang\"';");
        println!("  SELECT * FROM articles WHERE articles MATCH 'rust AND fast';");
    }
}

impl Default for Repl {
    fn default() -> Self { Self::new() }
}

// ── 結果輸出（對齊表格） ──────────────────────────────────────────────────

fn print_result_set(rs: &ResultSet) {
    if rs.columns.is_empty() { return; }

    // 計算每欄最大寬度
    let mut widths: Vec<usize> = rs.columns.iter().map(|c| c.len()).collect();
    for row in &rs.rows {
        for (i, val) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(val.to_string().len());
            }
        }
    }

    // 表頭
    let header: Vec<String> = rs.columns.iter().enumerate()
        .map(|(i, c)| format!("{:<width$}", c, width = widths[i]))
        .collect();
    println!("{}", header.join(" | "));

    // 分隔線
    let sep: Vec<String> = widths.iter().map(|&w| "-".repeat(w)).collect();
    println!("{}", sep.join("-+-"));

    // 資料列
    for row in &rs.rows {
        let cells: Vec<String> = row.iter().enumerate()
            .map(|(i, v)| {
                let w = widths.get(i).copied().unwrap_or(0);
                format!("{:<width$}", v.to_string(), width = w)
            })
            .collect();
        println!("{}", cells.join(" | "));
    }

    println!("({} row{})", rs.rows.len(), if rs.rows.len() == 1 { "" } else { "s" });
}

// ── 輔助解析函式 ──────────────────────────────────────────────────────────

/// 判斷輸入是否已完整（簡單版：包含 ; 或為點指令）
fn is_complete(buf: &str) -> bool {
    let t = buf.trim();
    t.ends_with(';') || t.starts_with('.')
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
        let after_from = &sql[from_pos + 4..];
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

    // 取出 query（去掉外層引號）
    let query = after_match.trim_matches(|c| c == '\'' || c == '"' || c == ';').to_string();
    if table_name.is_empty() || query.is_empty() { return None; }
    Some((table_name, query))
}

fn extract_table_name_from_select(sql: &str) -> Option<String> {
    // SELECT * FROM table_name WHERE ...
    let lower = sql.to_lowercase();
    let from_pos = lower.find("from")? + 4;
    let rest = sql[from_pos..].trim();

    // 找下一個關鍵字或空白結束
    let keywords = ["where", "order", "limit", "group", "having"];
    let mut end_pos = rest.len();
    for kw in &keywords {
        if let Some(pos) = rest.to_lowercase().find(kw) {
            if pos < end_pos {
                end_pos = pos;
            }
        }
    }

    let name = rest[..end_pos].trim();
    let name: String = name.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    if name.is_empty() { None } else { Some(name) }
}

/// 簡單解析 SQL VALUES 內的逗號分隔值（去引號）
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

// ── 測試 ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn repl() -> Repl { Repl::new() }

    #[test]
    fn create_and_select() {
        let mut r = repl();
        r.execute_sql("CREATE TABLE users (id INTEGER, name TEXT, age INTEGER);");
        r.execute_sql("INSERT INTO users VALUES (1, 'Alice', 30);");
        r.execute_sql("INSERT INTO users VALUES (2, 'Bob', 25);");
        // 確認沒有 panic，且 executor 有正確執行
        let catalog = r.executor.catalog();
        assert!(catalog.table_exists("users"));
    }

    #[test]
    fn multi_statement() {
        let mut r = repl();
        r.execute_sql("CREATE TABLE t (id INTEGER, val TEXT); INSERT INTO t VALUES (1, 'a');");
        assert!(r.executor.catalog().table_exists("t"));
    }

    #[test]
    fn dot_tables_empty() {
        let r = repl();
        // 不 panic 即可
        r.cmd_tables();
    }

    #[test]
    fn dot_schema() {
        let mut r = repl();
        r.execute_sql("CREATE TABLE products (id INTEGER, name TEXT, price REAL);");
        r.cmd_schema(Some("products"));
    }

    #[test]
    fn fts_create_and_search() {
        let mut r = repl();
        r.execute_sql("CREATE VIRTUAL TABLE docs USING fts5(title, body);");
        assert!(r.fts_tables.contains_key("docs"));

        r.execute_sql("INSERT INTO docs VALUES ('Rust lang', 'Fast safe systems');");
        r.execute_sql("INSERT INTO docs VALUES ('Python intro', 'Easy to learn');");

        // 搜尋
        let result = r.fts_select("docs", "rust").unwrap();
        assert_eq!(result.row_count(), 1);
    }

    #[test]
    fn fts_match_cjk() {
        let mut r = repl();
        r.execute_sql("CREATE VIRTUAL TABLE articles USING fts5(title, body);");
        r.execute_sql("INSERT INTO articles VALUES ('資料庫', '關聯式資料庫設計');");
        r.execute_sql("INSERT INTO articles VALUES ('程式語言', 'Rust 程式語言');");

        let result = r.fts_select("articles", "資料").unwrap();
        assert_eq!(result.row_count(), 1);
    }

    #[test]
    fn fts_and_query() {
        let mut r = repl();
        r.execute_sql("CREATE VIRTUAL TABLE docs USING fts5(title, body);");
        r.execute_sql("INSERT INTO docs VALUES ('Rust Programming', 'Fast and memory safe');");
        r.execute_sql("INSERT INTO docs VALUES ('Python', 'Easy language');");

        let result = r.fts_select("docs", "rust AND safe").unwrap();
        assert_eq!(result.row_count(), 1);
    }

    #[test]
    fn fts_or_query() {
        let mut r = repl();
        r.execute_sql("CREATE VIRTUAL TABLE docs USING fts5(title, body);");
        r.execute_sql("INSERT INTO docs VALUES ('Rust', 'systems language');");
        r.execute_sql("INSERT INTO docs VALUES ('Python', 'scripting language');");
        r.execute_sql("INSERT INTO docs VALUES ('Go', 'concurrent language');");

        let result = r.fts_select("docs", "rust OR python").unwrap();
        assert_eq!(result.row_count(), 2);
    }

    #[test]
    fn extract_match_query_test() {
        let sql = "SELECT * FROM articles WHERE articles MATCH 'rust'";
        let r = extract_match_query(sql).unwrap();
        assert_eq!(r.0, "articles");
        assert_eq!(r.1, "rust");
    }

    #[test]
    fn split_values_test() {
        let vals = split_sql_values("'hello world', 'foo bar'");
        assert_eq!(vals, vec!["hello world", "foo bar"]);
    }

    #[test]
    fn history_tracking() {
        let mut r = repl();
        r.execute_sql("CREATE TABLE t (id INTEGER);");
        r.execute_sql("INSERT INTO t VALUES (1);");
        assert_eq!(r.history.len(), 2);
    }

    #[test]
    fn aligned_output() {
        // print_result_set 不 panic
        let rs = ResultSet {
            columns: vec!["id".into(), "name".into()],
            rows:    vec![
                vec![Value::Integer(1), Value::Text("Alice".into())],
                vec![Value::Integer(2), Value::Text("Bob".into())],
            ],
            affected: 0,
            lastrowid: None,
        };
        print_result_set(&rs);
    }
}
