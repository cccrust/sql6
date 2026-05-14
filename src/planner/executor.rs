//! Executor：執行邏輯計畫，回傳 ResultSet
//!
//! ## 執行器職責
//!
//! Executor 負責：
//! 1. 接收 Planner 產出的 Plan（邏輯執行计划）
//! 2. 按照计划的運算子順序執行
//! 3. 返回查詢結果（ResultSet）
//!
//! ## 執行流程
//!
//! ```text
//! Plan → [Executor] → ResultSet → [Server] → JSON
//! ```
//!
//! ## Plan 類型與執行方法對應
//!
//! | Plan 變體 | 執行方法 | 說明 |
//! |-----------|---------|------|
//! | SeqScan | exec_seq_scan | 全表掃描 |
//! | IndexScan | exec_index_scan | 索引掃描 |
//! | Filter | exec_filter | 過濾 |
//! | Projection | exec_projection | 投影 |
//! | Join | exec_join | 連接 |
//! | Aggregate | exec_aggregate | 聚合 |
//! | Sort | exec_sort | 排序 |
//! | Limit | exec_limit | 分頁 |
//! | Insert/Update/Delete | exec_* | 資料修改 |
//!
//! ## 交易的支援
//!
//! 使用 TransactionManager 管理交易狀態，支援：
//! - BEGIN：開始交易
//! - COMMIT：提交交易
//! - ROLLBACK：回滾交易

use std::collections::HashMap;

use crate::btree::node::Key;
use crate::catalog::Catalog;
use crate::catalog::meta::{TriggerTiming as CatTiming, TriggerEvent as CatEvent};
use crate::pager::storage::{MemoryStorage, SharedStorage, Storage};
use crate::parser::ast::{BinOp, ColumnConstraint, Expr, SelectItem, SqlType, UnaryOp};

use crate::table::row::{Row, Value};
use crate::table::Table;
use super::plan::{InsertSource, JoinKind, Plan, TransactionOp};
use super::transaction::TransactionManager;
use super::planner::Planner;

// ── ResultSet ─────────────────────────────────────────────────────────────

/// 查詢結果集
///
/// 包含：
/// - columns：欄位名稱列表
/// - rows：查詢結果（二維向量）
/// - affected：受影響的行數（DML）
/// - lastrowid：最後插入行的 ID（AUTOINCREMENT）
#[derive(Debug, Default, Clone)]
pub struct ResultSet {
    /// 欄位名稱
    pub columns: Vec<String>,
    /// 查詢結果列
    pub rows:    Vec<Vec<Value>>,
    /// 受影響的行數（INSERT/UPDATE/DELETE）
    pub affected: i64,
    /// 最後插入行的 ID（AUTOINCREMENT）
    pub lastrowid: Option<i64>,
}

impl ResultSet {
    /// 建立空的結果集
    pub fn empty() -> Self { Self::default() }

    /// 建立單一訊息的結果集（通常用於元指令）
    pub fn ok_msg(msg: &str) -> Self {
        ResultSet {
            columns: vec!["result".into()],
            rows:    vec![vec![Value::Text(msg.into())]],
            affected: 0,
            lastrowid: None,
        }
    }

    /// DML 結果集（無回傳資料，但有受影響行數和 lastrowid）
    pub fn dml(count: i64, lastrowid: Option<i64>) -> Self {
        ResultSet {
            columns: vec![],
            rows:    vec![],
            affected: count,
            lastrowid,
        }
    }

    /// 取得列數
    pub fn row_count(&self) -> usize { self.rows.len() }

    /// 除錯用：格式化輸出結果
    pub fn display(&self) {
        if self.columns.is_empty() { return; }
        let header = self.columns.join(" | ");
        println!("{}", header);
        println!("{}", "-".repeat(header.len()));
        for row in &self.rows {
            println!("{}", row.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" | "));
        }
        println!("({} row{})", self.rows.len(), if self.rows.len() == 1 { "" } else { "s" });
    }
}

// ── Executor ──────────────────────────────────────────────────────────────
// 使用 SharedStorage 管理資料表（可選 Memory 或 Disk）

/// 查詢執行器
///
/// 持有資料庫的存儲後端、目錄、表格等資源，執行 Plan 並返回結果。
///
/// # 資源管理
///
/// - storage：底層存儲（記憶體或磁碟）
/// - catalog：系統目錄（表格定義）
/// - tables：已開啟的表格
/// - txn_mgr：交易管理器
pub struct Executor {
    /// 存儲後端（支援記憶體或磁碟模式）
    storage:     SharedStorage,
    /// 系統目錄
    catalog:     Catalog<SharedStorage>,
    /// 已開啟的表格
    tables:      HashMap<String, Table<SharedStorage>>,
    /// 交易管理器
    txn_mgr:     TransactionManager,
    /// CTE 查詢結果快取
    cte_cache:   HashMap<String, ResultSet>,
    /// 表格約束
    constraints: HashMap<String, crate::planner::constraints::TableConstraints>,
    /// 快取大小
    cache_size:  usize,
    /// 是否正在執行觸發器（防止遞迴）
    triggering:  bool,
    /// 附加的資料庫
    attached:    HashMap<String, AttachedDb>,
}

/// 附加的資料庫連線
struct AttachedDb {
    alias:   String,
    storage: SharedStorage,
    catalog: Catalog<SharedStorage>,
    tables:  HashMap<String, Table<SharedStorage>>,
}

impl Executor {
    /// 建立記憶體模式的執行器（適用於 REPL、測試）
    pub fn new() -> Self {
        let storage = SharedStorage::memory();
        let catalog = Catalog::new(storage.clone());
        Executor {
            storage,
            catalog,
            tables:      HashMap::new(),
            txn_mgr:     TransactionManager::new(),
            cte_cache:   HashMap::new(),
            constraints: HashMap::new(),
            cache_size:  256,
            triggering:  false,
            attached:    HashMap::new(),
        }
    }

    /// 建立磁碟模式的執行器
    ///
    /// # 參數
    /// - path：資料庫檔案路徑
    pub fn with_disk(path: &str) -> std::io::Result<Self> {
        // 使用 LRU 快取（256 頁容量）
        let storage = SharedStorage::disk_with_cache(path, 256)?;
        let cache_size = storage.cache_size();

        // 先釋放 lock 再使用 storage（避免 deadlock）
        let root = storage.lock().catalog_root();

        let catalog = match root {
            Some(root) => Catalog::open(storage.clone(), root),
            None => Catalog::new(storage.clone()),
        };

        Ok(Executor {
            storage,
            catalog,
            tables:      HashMap::new(),
            txn_mgr:     TransactionManager::new(),
            cte_cache:   HashMap::new(),
            constraints: HashMap::new(),
            cache_size,
            triggering:  false,
            attached:    HashMap::new(),
        })
    }

    /// 取得目錄的唯讀參照
    pub fn catalog(&self) -> &Catalog<SharedStorage> { &self.catalog }

    /// 取得目錄根頁面
    pub fn catalog_root(&self) -> usize {
        self.catalog.root_page()
    }

    /// 刷新資料（刷寫快取到磁碟）
    pub fn flush(&mut self) {
        let root = self.catalog.root_page();
        self.storage.lock().flush();
        self.storage.lock().set_catalog_root(root);
    }

    /// 執行計畫並返回結果
    ///
    /// 根據 Plan 的類型分發到對應的執行方法
    pub fn execute(&mut self, plan: Plan) -> Result<ResultSet, String> {
        match plan {
            Plan::Projection { input, columns }              => self.exec_projection(*input, columns),
            Plan::SeqScan   { table, filter, .. }            => self.exec_seq_scan(&table, filter),
            Plan::IndexScan { table, column, value, .. }     => self.exec_index_scan(&table, &column, value),
            Plan::Filter    { input, expr }                  => self.exec_filter(*input, expr),
            Plan::Sort      { input, keys }                  => self.exec_sort(*input, keys),
            Plan::Limit     { input, limit, offset }         => self.exec_limit(*input, limit, offset),
            Plan::Distinct  { input }                        => self.exec_distinct(*input),
            Plan::Aggregate { input, group_by, having, outputs } =>
                self.exec_aggregate(*input, group_by, having, outputs),
            Plan::Join      { left, right, condition, kind } =>
                self.exec_join(*left, *right, condition, kind),
            Plan::Insert    { table, columns, source, default_values }       => self.exec_insert(table, columns, source, default_values),
            Plan::Update    { table, input, sets }           => self.exec_update(table, *input, sets),
            Plan::Delete    { table, input }                 => self.exec_delete(table, *input),
            Plan::CreateTable { stmt }                       => self.exec_create_table(stmt),
            Plan::DropTable { name, if_exists }              => self.exec_drop_table(name, if_exists),
            Plan::CreateIndex { stmt }                       => self.exec_create_index(stmt),
            Plan::DropIndex { name, if_exists }              => self.exec_drop_index(name, if_exists),
            Plan::AlterTable { stmt }                        => self.exec_alter_table(stmt),
            Plan::Pragma { name, value }                     => self.exec_pragma(name, value),
            Plan::Explain { inner }                          => self.exec_explain(*inner),
            Plan::CreateView { stmt }                        => self.exec_create_view(stmt),
            Plan::DropView { name, if_exists }              => self.exec_drop_view(name, if_exists),
            Plan::CreateTrigger { stmt }                     => self.exec_create_trigger(stmt),
            Plan::DropTrigger { name, if_exists }             => self.exec_drop_trigger(name, if_exists),
            Plan::Reindex { name }                           => self.exec_reindex(name),
            Plan::Analyze { name }                           => self.exec_analyze(name),
            Plan::Attach { path, alias }                      => self.exec_attach(path, alias),
            Plan::Detach { alias }                           => self.exec_detach(alias),
            Plan::Vacuum                                    => self.exec_vacuum(),
            Plan::Transaction(op)                            => self.exec_transaction(op),
            Plan::SubqueryScan { query, alias }                  => self.exec_subquery_scan(*query, alias),
            Plan::Cte { definitions, query }                     => self.exec_cte(definitions, *query),
            Plan::SetOperation { left, right, op }             => self.exec_set_operation(*left, *right, op.clone()),
        }
    }

    // ── 掃描 ──────────────────────────────────────────────────────────────

    fn exec_seq_scan(&mut self, table: &str, filter: Option<Expr>) -> Result<ResultSet, String> {
        // 無 FROM 的 SELECT（dual 虛擬表）：回傳一列空 row 讓 projection 求值
        if table == "__dual__" {
            return Ok(ResultSet { columns: vec![], rows: vec![vec![]], affected: 0, lastrowid: None });
        }
        // CTE 虛擬表優先
        if let Some(rs) = self.cte_cache.get(table).cloned() {
            let col_names = rs.columns.clone();
            let rows = rs.rows.into_iter()
                .filter(|row| match &filter {
                    Some(e) => eval_expr(e, &Row::new(row.clone()), &col_names)
                        .map(|v| is_truthy(&v)).unwrap_or(false),
                    None => true,
                })
                .collect();
            return Ok(ResultSet { columns: col_names, rows, affected: 0, lastrowid: None });
        }
        // sqlite_master 虛擬表
        if table == "sqlite_master" || table == "sqlite_master_mview" {
            let columns = crate::catalog::Catalog::<crate::pager::storage::SharedStorage>::sqlite_master_columns();
            let rows = self.catalog.sqlite_master_rows();
            let rows: Vec<Vec<Value>> = rows.into_iter()
                .filter(|row| match &filter {
                    Some(e) => eval_expr(e, &Row::new(row.clone()), &columns)
                        .map(|v| is_truthy(&v)).unwrap_or(false),
                    None => true,
                })
                .collect();
            return Ok(ResultSet { columns, rows, affected: 0, lastrowid: None });
        }
        let col_names = self.col_names(table)?;
        // 先 resolve 子查詢（需要在掃描前執行，且需要 &mut self）
        let resolved_filter = match filter {
            Some(e) => Some(self.resolve_expr(e)?),
            None    => None,
        };
        let tbl = self.get_table(table)?;
        let all = tbl.scan();

        let rows = all.into_iter()
            .filter(|row| match &resolved_filter {
                Some(e) => eval_expr(e, row, &col_names).map(|v| is_truthy(&v)).unwrap_or(false),
                None    => true,
            })
            .map(|r| r.values)
            .collect();

        Ok(ResultSet { columns: col_names, rows, affected: 0, lastrowid: None })
    }

    fn exec_index_scan(&mut self, table: &str, _col: &str, value: Expr) -> Result<ResultSet, String> {
        let col_names = self.col_names(table)?;
        let key = expr_to_key(&value)?;
        let tbl = self.get_table(table)?;
        let rows = tbl.get(&key).map(|r| vec![r.values]).unwrap_or_default();
        Ok(ResultSet { columns: col_names, rows, affected: 0, lastrowid: None })
    }

    // ── 關聯代數 ──────────────────────────────────────────────────────────

    fn exec_projection(&mut self, input: Plan, columns: Vec<SelectItem>) -> Result<ResultSet, String> {
        let src = self.execute(input)?;

        if columns.len() == 1 && matches!(&columns[0], SelectItem::Star) {
            return Ok(src);
        }

        let mut out_cols: Vec<String> = Vec::new();
        let mut out_rows: Vec<Vec<Value>> = Vec::new();

        for src_row in &src.rows {
            let ctx = Row::new(src_row.clone());
            let mut vals: Vec<Value> = Vec::new();
            let first = out_cols.is_empty();

            for item in &columns {
                match item {
                    SelectItem::Star | SelectItem::TableStar(_) => {
                        if first { out_cols.extend(src.columns.clone()); }
                        vals.extend(src_row.clone());
                    }
                    SelectItem::Expr { expr, alias } => {
                        if first {
                            out_cols.push(alias.clone().unwrap_or_else(|| expr_name(expr)));
                        }
                        let resolved = self.resolve_expr(expr.clone())?;
                        vals.push(eval_expr(&resolved, &ctx, &src.columns)?);
                    }
                }
            }
            out_rows.push(vals);
        }
        Ok(ResultSet { columns: out_cols, rows: out_rows, affected: 0, lastrowid: None })
    }

    fn exec_filter(&mut self, input: Plan, expr: Expr) -> Result<ResultSet, String> {
        let src = self.execute(input)?;
        // 預先解析子查詢（IN subquery, EXISTS, scalar subquery）
        let resolved = self.resolve_expr(expr)?;
        let rows = src.rows.into_iter()
            .filter(|r| {
                let row = Row::new(r.clone());
                eval_expr(&resolved, &row, &src.columns).map(|v| is_truthy(&v)).unwrap_or(false)
            })
            .collect();
        Ok(ResultSet { columns: src.columns, rows, affected: 0, lastrowid: None })
    }

    fn exec_sort(&mut self, input: Plan, keys: Vec<crate::parser::ast::OrderItem>) -> Result<ResultSet, String> {
        let mut src = self.execute(input)?;
        let cols = src.columns.clone();
        src.rows.sort_by(|a, b| {
            for k in &keys {
                let va = eval_expr(&k.expr, &Row::new(a.clone()), &cols).unwrap_or(Value::Null);
                let vb = eval_expr(&k.expr, &Row::new(b.clone()), &cols).unwrap_or(Value::Null);
                let ord = cmp_val(&va, &vb);
                let ord = if k.asc { ord } else { ord.reverse() };
                if ord != std::cmp::Ordering::Equal { return ord; }
            }
            std::cmp::Ordering::Equal
        });
        Ok(src)
    }

    fn exec_limit(&mut self, input: Plan, limit: Option<u64>, offset: u64) -> Result<ResultSet, String> {
        let src = self.execute(input)?;
        let rows = src.rows.into_iter()
            .skip(offset as usize)
            .take(limit.unwrap_or(u64::MAX) as usize)
            .collect();
        Ok(ResultSet { columns: src.columns, rows, affected: 0, lastrowid: None })
    }

    fn exec_distinct(&mut self, input: Plan) -> Result<ResultSet, String> {
        let src = self.execute(input)?;
        let mut seen = std::collections::HashSet::new();
        let rows = src.rows.into_iter()
            .filter(|r| seen.insert(r.iter().map(|v| format!("{:?}", v)).collect::<Vec<_>>().join(",")))
            .collect();
        Ok(ResultSet { columns: src.columns, rows, affected: 0, lastrowid: None })
    }

    fn exec_aggregate(
        &mut self, input: Plan,
        group_by: Vec<Expr>, having: Option<Expr>, outputs: Vec<SelectItem>,
    ) -> Result<ResultSet, String> {
        let src = self.execute(input)?;
        let cols = src.columns.clone();

        // 分組
        type Group = (Vec<Value>, Vec<Row>);
        let mut groups: Vec<Group> = Vec::new();
        let all_src_rows = src.rows.clone();
        for rv in all_src_rows.clone() {
            let row = Row::new(rv);
            let key: Vec<Value> = group_by.iter()
                .map(|e| eval_expr(e, &row, &cols).unwrap_or(Value::Null))
                .collect();
            if let Some(g) = groups.iter_mut().find(|(k, _)| k == &key) {
                g.1.push(row);
            } else {
                groups.push((key, vec![row]));
            }
        }
        if groups.is_empty() && group_by.is_empty() {
            let all_rows: Vec<Row> = all_src_rows.into_iter().map(Row::new).collect();
            groups.push((vec![], all_rows));
        }

        let mut out_cols: Vec<String> = Vec::new();
        let mut out_rows: Vec<Vec<Value>> = Vec::new();

        for (_, rows) in &groups {
            let mut rv: Vec<Value> = Vec::new();
            for item in &outputs {
                if let SelectItem::Expr { expr, alias } = item {
                    if out_cols.len() < outputs.len() {
                        out_cols.push(alias.clone().unwrap_or_else(|| expr_name(expr)));
                    }
                    rv.push(eval_aggregate(expr, rows, &cols)?);
                }
            }
            if let Some(h) = &having {
                let hrow = Row::new(rv.clone());
                if !is_truthy(&eval_expr(h, &hrow, &out_cols).unwrap_or(Value::Null)) { continue; }
            }
            out_rows.push(rv);
        }
        Ok(ResultSet { columns: out_cols, rows: out_rows, affected: 0, lastrowid: None })
    }

    fn exec_join(
        &mut self, left: Plan, right: Plan, condition: Option<Expr>, kind: JoinKind,
    ) -> Result<ResultSet, String> {
        let l = self.execute(left)?;
        let r = self.execute(right)?;
        let mut cols = l.columns.clone();
        cols.extend(r.columns.clone());
        let mut rows: Vec<Vec<Value>> = Vec::new();

        for lr in &l.rows {
            let mut matched = false;
            for rr in &r.rows {
                let combined: Vec<Value> = lr.iter().chain(rr.iter()).cloned().collect();
                let pass = match &condition {
                    Some(cond) => eval_expr(cond, &Row::new(combined.clone()), &cols)
                        .map(|v| is_truthy(&v)).unwrap_or(false),
                    None => true,
                };
                if pass { rows.push(combined); matched = true; }
            }
            if !matched && kind == JoinKind::Left {
                let mut combined = lr.clone();
                combined.extend(vec![Value::Null; r.columns.len()]);
                rows.push(combined);
            }
        }
        Ok(ResultSet { columns: cols, rows, affected: 0, lastrowid: None })
    }

    // ── DML ───────────────────────────────────────────────────────────────

    fn exec_insert(&mut self, table: String, columns: Vec<String>, source: InsertSource, default_values: bool) -> Result<ResultSet, String> {
        let mut meta = self.catalog.get_table(&table)
            .ok_or_else(|| format!("table '{}' not found", table))?.clone();

        let InsertSource::Values(all_values) = source;
        let count = if default_values { 1 } else { all_values.len() as i64 };

        // 檢查是否有 AUTOINCREMENT 欄位
        let autoinc_col_idx = meta.schema.columns.iter()
            .position(|c| c.autoinc);

        // 追蹤最後產生的 AUTOINCREMENT 值
        let mut last_autoinc: Option<i64> = None;

        for value_exprs in all_values {
            // INSERT DEFAULT VALUES - 使用 schema 中的預設值
            let mut vals: Vec<Value> = if default_values {
                meta.schema.columns.iter()
                    .map(|col| {
                        if let Some(ref expr) = col.default {
                            return eval_literal(expr).unwrap_or(Value::Null);
                        }
                        Value::Null
                    })
                    .collect()
            } else {
                value_exprs.iter()
                    .map(eval_literal)
                    .collect::<Result<_, String>>()?
            };

            // 處理 AUTOINCREMENT - 只計算需要多少空間
            let mut autoinc_value: Option<i64> = None;
            if let Some(idx) = autoinc_col_idx {
                let need_autoinc = if columns.is_empty() {
                    idx >= vals.len() || vals.get(idx) == Some(&Value::Null)
                } else {
                    !columns.iter().any(|c| meta.schema.index_of(c) == Some(idx))
                };
                if need_autoinc {
                    meta.autoinc_last += 1;
                    autoinc_value = Some(meta.autoinc_last as i64);
                    last_autoinc = autoinc_value;
                }
            }

            // 對於無指定欄位的情況，需要擴展 vals 以匹配 schema
            let mut row = if columns.is_empty() {
                // 無指定欄位時，確保 vals 足夠長
                if let Some(v) = autoinc_value {
                    if vals.len() <= autoinc_col_idx.unwrap() {
                        vals.resize(autoinc_col_idx.unwrap() + 1, Value::Null);
                    }
                    vals[autoinc_col_idx.unwrap()] = Value::Integer(v);
                }
                Row::new(vals)
            } else {
                // 有指定欄位時，逐一對應
                let mut rv = vec![Value::Null; meta.schema.columns.len()];
                for (col, val) in columns.iter().zip(vals) {
                    let idx = meta.schema.index_of(col)
                        .ok_or_else(|| format!("column '{}' not found", col))?;
                    rv[idx] = val;
                }
                // AUTOINCREMENT 欄位單獨設定
                if let Some(v) = autoinc_value {
                    rv[autoinc_col_idx.unwrap()] = Value::Integer(v);
                }
                Row::new(rv)
            };

            // FOREIGN KEY 驗證 - 先收集需要檢查的 FK 資訊
            let fk_checks: Vec<(Vec<String>, String, Vec<String>)> = if let Some(tc) = self.constraints.get(&table) {
                tc.constraints.iter()
                    .filter_map(|c| {
                        if let crate::planner::constraints::Constraint::ForeignKey { 
                            columns, ref_table, ref_columns 
                        } = c {
                            Some((columns.clone(), ref_table.clone(), ref_columns.clone()))
                        } else {
                            None
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };

            // 執行 FK 驗證
            for (columns, ref_table, ref_columns) in fk_checks {
                // 取得 FK 欄位的值
                let fk_values: Vec<Value> = columns.iter()
                    .map(|c| {
                        let idx = meta.schema.index_of(c)
                            .ok_or_else(|| format!("FK column '{}' not found", c))?;
                        Ok::<Value, String>(row.values.get(idx).cloned().unwrap_or(Value::Null))
                    })
                    .collect::<Result<_, String>>()?;
                
                // 跳過全部為 NULL 的 FK（允許）
                if fk_values.iter().all(|v| matches!(v, Value::Null)) {
                    continue;
                }
                
                // 檢查父表中是否存在對應的列
                if let Some(parent_meta) = self.catalog.get_table(&ref_table).cloned() {
                    let parent_col_idx = if ref_columns.len() == 1 {
                        Some(parent_meta.schema.index_of(&ref_columns[0])
                            .ok_or_else(|| format!("ref column '{}' not found", ref_columns[0]))?)
                    } else {
                        None
                    };
                    
                    // 先取得 parent table 的資料（會 borrow self）
                    let parent_table = self.get_table(&ref_table)?;
                    let parent_rows = parent_table.scan();
                    
                    // 檢查是否有匹配的父記錄
                    if let Some(idx) = parent_col_idx {
                        let mut found = false;
                        for parent_row in &parent_rows {
                            if parent_row.values.get(idx) == Some(&fk_values[0]) {
                                found = true;
                                break;
                            }
                        }
                        
                        if !found {
                            return Err(format!(
                                "FOREIGN KEY constraint failed: '{}' not found in '{}'",
                                fk_values[0], ref_table
                            ));
                        }
                    }
                }
            }

            // 約束檢查（使用當前 meta，不要重新 clone）
            if let Some(tc) = self.constraints.get(&table) {
                let existing = self.tables.get_mut(&table)
                    .map(|t| t.scan()).unwrap_or_default();
                crate::planner::constraints::check_row(&row, &meta.schema, tc, &existing)
                    .map_err(|e| e)?;
            }
            self.fire_triggers(
                        &table,
                        crate::parser::ast::TriggerEvent::Insert,
                        None,
                        None,
                    )?;
                    self.get_table(&table)?.insert(row.clone())?;
                    self.fire_triggers(
                        &table,
                        crate::parser::ast::TriggerEvent::Insert,
                        Some(&row),
                        None,
                    )?;
        }

        // 更新 catalog 中的 autoinc_last
        let root = self.tables[&table].root_page();
        let new_count = self.tables[&table].len();
        self.catalog.update_table_meta_full(&table, root, new_count, Some(meta.autoinc_last))?;

        Ok(ResultSet::dml(count, last_autoinc))
    }

    fn exec_update(&mut self, table: String, input: Plan, sets: Vec<(String, Expr)>) -> Result<ResultSet, String> {
        let src = self.execute(input)?;
        let meta = self.catalog.get_table(&table)
            .ok_or_else(|| format!("table '{}' not found", table))?.clone();
        let col_names = src.columns.clone();
        let count = src.rows.len();

        for rv in src.rows {
            let old = Row::new(rv.clone());
            let key = row_to_key(&old)?;
            let mut new_vals = rv.clone();
            for (col, expr) in &sets {
                let idx = meta.schema.index_of(col)
                    .ok_or_else(|| format!("column '{}' not found", col))?;
                new_vals[idx] = eval_expr(expr, &old, &col_names)?;
            }
            let new_row = Row::new(new_vals.clone());
            self.fire_triggers(&table, crate::parser::ast::TriggerEvent::Update(None), Some(&new_row), Some(&old))?;
            let tbl = self.get_table(&table)?;
            tbl.delete(&key);
            tbl.insert(new_row.clone())?;
            self.fire_triggers(&table, crate::parser::ast::TriggerEvent::Update(None), Some(&new_row), Some(&old))?;
        }
        Ok(ResultSet::dml(count as i64, None))
    }

    fn exec_delete(&mut self, table: String, input: Plan) -> Result<ResultSet, String> {
        let src = self.execute(input)?;
        let count = src.rows.len();
        for rv in src.rows {
            let old = Row::new(rv.clone());
            let key = row_to_key(&old)?;
            self.fire_triggers(&table, crate::parser::ast::TriggerEvent::Delete, None, Some(&old))?;
            self.get_table(&table)?.delete(&key);
            self.fire_triggers(&table, crate::parser::ast::TriggerEvent::Delete, None, Some(&old))?;
        }
        Ok(ResultSet::dml(count as i64, None))
    }

    // ── DDL ───────────────────────────────────────────────────────────────

    fn exec_create_table(&mut self, stmt: crate::parser::ast::CreateTableStmt) -> Result<ResultSet, String> {
        use crate::table::schema::{Column, DataType, Schema};
        use crate::parser::ast::SqlType;

        if self.catalog.table_exists(&stmt.name) {
            if stmt.if_not_exists { return Ok(ResultSet::ok_msg("table already exists")); }
            return Err(format!("table '{}' already exists", stmt.name));
        }
        let columns: Vec<Column> = stmt.columns.iter().map(|cd| {
            let dt = match cd.data_type {
                SqlType::Integer => DataType::Integer,
                SqlType::Real    => DataType::Float,
                SqlType::Text    => DataType::Text,
                SqlType::Blob    => DataType::Text,
                SqlType::Boolean => DataType::Boolean,
                SqlType::Null    => DataType::Text,
            };
            let mut col = Column::new(&cd.name, dt);
            // 檢查約束
            for constraint in &cd.constraints {
                match constraint {
                    ColumnConstraint::PrimaryKey { autoincrement } => {
                        if *autoincrement {
                            col = col.autoincrement();
                        }
                    }
                    ColumnConstraint::Default(expr) => {
                        col = col.with_default(expr.clone());
                    }
                    ColumnConstraint::NotNull => {
                        col = col.not_null();
                    }
                    _ => {}
                }
            }
            col
        }).collect();
        let tc = crate::planner::constraints::constraints_from_ast(&stmt);
        self.constraints.insert(stmt.name.clone(), tc);
        self.catalog.create_table(&stmt.name, Schema::new(columns))?;
        Ok(ResultSet::ok_msg("table created"))
    }

    fn exec_drop_table(&mut self, name: String, if_exists: bool) -> Result<ResultSet, String> {
        if !self.catalog.table_exists(&name) {
            if if_exists { return Ok(ResultSet::ok_msg("table does not exist")); }
            return Err(format!("table '{}' does not exist", name));
        }
        self.tables.remove(&name);
        self.catalog.drop_table(&name)?;
        Ok(ResultSet::ok_msg("table dropped"))
    }

    fn exec_create_index(&mut self, stmt: crate::parser::ast::CreateIndexStmt) -> Result<ResultSet, String> {
        if !self.catalog.table_exists(&stmt.table) {
            return Err(format!("table '{}' does not exist", stmt.table));
        }
        for col in &stmt.columns {
            if !self.catalog.get_table(&stmt.table)
                .map(|t| t.schema.columns.iter().any(|c| &c.name == col))
                .unwrap_or(false)
            {
                return Err(format!("no such column: {}", col));
            }
        }
        self.catalog.create_index(&stmt.name, &stmt.table, &stmt.columns, stmt.unique)?;
        Ok(ResultSet::ok_msg("index created"))
    }

    fn exec_drop_index(&mut self, name: String, if_exists: bool) -> Result<ResultSet, String> {
        if !self.catalog.index_exists(&name) {
            if if_exists { return Ok(ResultSet::ok_msg("index does not exist")); }
            return Err(format!("index '{}' does not exist", name));
        }
        self.catalog.drop_index(&name)?;
        Ok(ResultSet::ok_msg("index dropped"))
    }

    fn exec_create_view(&mut self, stmt: crate::parser::ast::CreateViewStmt) -> Result<ResultSet, String> {
        if self.catalog.view_exists(&stmt.name) {
            if stmt.if_not_exists { return Ok(ResultSet::ok_msg("view already exists")); }
            return Err(format!("view '{}' already exists", stmt.name));
        }
        let query_str = format!("{:?}", stmt.query);
        self.catalog.create_view(&stmt.name, &query_str)?;
        Ok(ResultSet::ok_msg("view created"))
    }

    fn exec_drop_view(&mut self, name: String, if_exists: bool) -> Result<ResultSet, String> {
        if !self.catalog.view_exists(&name) {
            if if_exists { return Ok(ResultSet::ok_msg("view does not exist")); }
            return Err(format!("view '{}' does not exist", name));
        }
        self.catalog.drop_view(&name)?;
        Ok(ResultSet::ok_msg("view dropped"))
    }

    fn exec_create_trigger(&mut self, stmt: crate::parser::ast::CreateTriggerStmt) -> Result<ResultSet, String> {
        use crate::catalog::meta::{TriggerTiming as CatTiming, TriggerEvent as CatEvent};

        if self.catalog.trigger_exists(&stmt.name) {
            if stmt.if_not_exists { return Ok(ResultSet::ok_msg("trigger already exists")); }
            return Err(format!("trigger '{}' already exists", stmt.name));
        }
        let timing = match stmt.timing {
            crate::parser::ast::TriggerTiming::Before => CatTiming::Before,
            crate::parser::ast::TriggerTiming::After => CatTiming::After,
            crate::parser::ast::TriggerTiming::InsteadOf => CatTiming::InsteadOf,
        };
        let event = match stmt.event {
            crate::parser::ast::TriggerEvent::Delete => CatEvent::Delete,
            crate::parser::ast::TriggerEvent::Insert => CatEvent::Insert,
            crate::parser::ast::TriggerEvent::Update(cols) => CatEvent::Update(cols),
        };
        let when = stmt.when.as_ref().map(|e| format!("{:?}", e));
        self.catalog.create_trigger(
            &stmt.name,
            &stmt.table,
            timing,
            event,
            stmt.for_each_row,
            when,
            &stmt.body,
        )?;
        Ok(ResultSet::ok_msg("trigger created"))
    }

    fn exec_drop_trigger(&mut self, name: String, if_exists: bool) -> Result<ResultSet, String> {
        if !self.catalog.trigger_exists(&name) {
            if if_exists { return Ok(ResultSet::ok_msg("trigger does not exist")); }
            return Err(format!("trigger '{}' does not exist", name));
        }
        self.catalog.drop_trigger(&name)?;
        Ok(ResultSet::ok_msg("trigger dropped"))
    }

    fn fire_triggers(
        &mut self,
        table: &str,
        event: crate::parser::ast::TriggerEvent,
        _new_row: Option<&Row>,
        _old_row: Option<&Row>,
    ) -> Result<(), String> {
        if self.triggering { return Ok(()); }

        let cat_event = match &event {
            crate::parser::ast::TriggerEvent::Delete => CatEvent::Delete,
            crate::parser::ast::TriggerEvent::Insert => CatEvent::Insert,
            crate::parser::ast::TriggerEvent::Update(cols) => CatEvent::Update(cols.clone()),
        };

        let triggers = self.catalog.get_triggers(table, &cat_event);
        if triggers.is_empty() { return Ok(()); }

        let trigger_bodies: Vec<(CatTiming, String)> = triggers
            .iter()
            .map(|t| (t.timing.clone(), t.body.clone()))
            .collect();

        for (timing, body) in trigger_bodies {
            let timing_matches = matches!(timing, CatTiming::Before | CatTiming::After);
            if !timing_matches { continue; }

            self.triggering = true;
            let result = self.execute_sql_body(&body);
            self.triggering = false;

            result?;
        }
        Ok(())
    }

    fn execute_sql_body(&mut self, sql: &str) -> Result<(), String> {
        let stmts = crate::parser::parse(sql)
            .map_err(|e| format!("trigger body parse error: {}", e))?;
        let stmt = stmts.into_iter().next()
            .ok_or_else(|| "trigger body is empty".to_string())?;
        let planner = Planner::new(&self.catalog);
        let plan = planner.plan(stmt)
            .map_err(|e| format!("trigger body plan error: {}", e))?;
        self.execute(plan)?;
        Ok(())
    }

    fn exec_reindex(&mut self, name: Option<String>) -> Result<ResultSet, String> {
        match name {
            Some(n) => {
                if self.catalog.index_exists(&n) {
                    Ok(ResultSet::ok_msg("reindex executed"))
                } else {
                    Err(format!("no such index: {}", n))
                }
            }
            None => {
                let index_count = self.catalog.index_names().len();
                Ok(ResultSet::ok_msg(&format!("reindex executed ({} indexes processed)", index_count)))
            }
        }
    }

    fn exec_analyze(&mut self, name: Option<String>) -> Result<ResultSet, String> {
        match name {
            Some(n) => {
                if self.catalog.table_exists(&n) || self.catalog.index_exists(&n) {
                    Ok(ResultSet::ok_msg("analyze executed"))
                } else {
                    Err(format!("no such table or index: {}", n))
                }
            }
            None => {
                Ok(ResultSet::ok_msg("analyze executed"))
            }
        }
    }

    fn exec_attach(&mut self, path: String, alias: String) -> Result<ResultSet, String> {
        if self.attached.contains_key(&alias) {
            return Err(format!("database '{}' is already attached", alias));
        }
        let storage = SharedStorage::disk_with_cache(&path, 256)
            .map_err(|e| format!("cannot open database '{}': {}", path, e))?;
        let root = storage.lock().catalog_root();
        let catalog = match root {
            Some(r) => Catalog::open(storage.clone(), r),
            None => Catalog::new(storage.clone()),
        };
        self.attached.insert(alias.clone(), AttachedDb {
            alias,
            storage,
            catalog,
            tables: HashMap::new(),
        });
        Ok(ResultSet::ok_msg("database attached"))
    }

    fn exec_detach(&mut self, alias: String) -> Result<ResultSet, String> {
        if self.attached.remove(&alias).is_none() {
            return Err(format!("no such attached database: {}", alias));
        }
        Ok(ResultSet::ok_msg("database detached"))
    }

    fn exec_vacuum(&mut self) -> Result<ResultSet, String> {
        let disk_path = self.storage.disk_path().map(|p| p.to_path_buf());
        if let Some(path) = disk_path {
            self.exec_vacuum_disk(&path)
        } else {
            self.exec_vacuum_memory()
        }
    }

    fn exec_vacuum_memory(&mut self) -> Result<ResultSet, String> {
        let new_storage = SharedStorage::memory();
        let mut new_catalog = Catalog::new(new_storage.clone());

        for table_name in self.catalog.table_names() {
            let meta = match self.catalog.get_table(table_name) {
                Some(m) => m.clone(),
                None => continue,
            };

            let _ = new_catalog.create_table(table_name, meta.schema.clone());

            let tbl = if meta.root_page != usize::MAX && meta.root_page > 0 {
                Table::open(table_name, meta.schema.clone(), self.storage.clone(), meta.root_page, meta.row_count)
            } else {
                Table::new(table_name, meta.schema.clone(), self.storage.clone())
            };

            let rows = {
                let mut t = tbl;
                let old_rows = t.scan();
                drop(t);
                old_rows
            };

            let mut new_tbl = Table::new(table_name, meta.schema.clone(), new_storage.clone());
            for row in rows {
                new_tbl.insert(row)?;
            }
            let root = new_tbl.root_page();
            let count = new_tbl.len();
            new_catalog.update_table_meta(table_name, root, count)?;
        }

        for index_name in self.catalog.index_names() {
            if let Some(meta) = self.catalog.get_index(index_name) {
                let _ = new_catalog.create_index(index_name, &meta.table, &meta.columns, meta.unique);
            }
        }

        for view_name in self.catalog.view_names() {
            if let Some(view_meta) = self.catalog.get_view(view_name) {
                let _ = new_catalog.create_view(view_name, &view_meta.query);
            }
        }

        let table_names: Vec<String> = self.tables.keys().cloned().collect();
        self.tables.clear();
        self.storage = new_storage;
        self.catalog = new_catalog;

        for name in table_names {
            let _ = self.get_table(&name);
        }

        Ok(ResultSet::ok_msg("vacuum executed"))
    }

    fn exec_vacuum_disk(&mut self, path: &std::path::Path) -> Result<ResultSet, String> {
        use std::fs;

        let temp_path = path.with_extension("vacuum.tmp");

        let new_storage = SharedStorage::disk(&temp_path)
            .map_err(|e| format!("cannot create vacuum file: {}", e))?;
        let mut new_catalog = Catalog::new(new_storage.clone());

        for table_name in self.catalog.table_names() {
            let meta = match self.catalog.get_table(table_name) {
                Some(m) => m.clone(),
                None => continue,
            };

            let _ = new_catalog.create_table(table_name, meta.schema.clone());

            let tbl = if meta.root_page != usize::MAX && meta.root_page > 0 {
                Table::open(table_name, meta.schema.clone(), self.storage.clone(), meta.root_page, meta.row_count)
            } else {
                Table::new(table_name, meta.schema.clone(), self.storage.clone())
            };

            let rows = {
                let mut t = tbl;
                let old_rows = t.scan();
                drop(t);
                old_rows
            };

            let mut new_tbl = Table::new(table_name, meta.schema.clone(), new_storage.clone());
            for row in rows {
                new_tbl.insert(row)?;
            }
            let root = new_tbl.root_page();
            let count = new_tbl.len();
            new_catalog.update_table_meta(table_name, root, count)?;
        }

        for index_name in self.catalog.index_names() {
            if let Some(meta) = self.catalog.get_index(index_name) {
                let _ = new_catalog.create_index(index_name, &meta.table, &meta.columns, meta.unique);
            }
        }

        for view_name in self.catalog.view_names() {
            if let Some(view_meta) = self.catalog.get_view(view_name) {
                let _ = new_catalog.create_view(view_name, &view_meta.query);
            }
        }

        for trigger_name in self.catalog.trigger_names() {
            if let Some(trigger_meta) = self.catalog.get_trigger(trigger_name) {
                let _ = new_catalog.create_trigger(
                    &trigger_meta.name,
                    &trigger_meta.table,
                    trigger_meta.timing.clone(),
                    trigger_meta.event.clone(),
                    trigger_meta.for_each_row,
                    trigger_meta.when.clone(),
                    &trigger_meta.body,
                );
            }
        }

        new_storage.lock().flush();
        drop(new_storage);
        drop(new_catalog);

        fs::rename(&temp_path, path)
            .map_err(|e| format!("vacuum rename failed: {}", e))?;

        let _ = fs::remove_file(temp_path.with_extension("sql6wal"));

        let new_exec = Executor::with_disk(path.as_os_str().to_str().unwrap())
            .map_err(|e| format!("cannot reopen database after vacuum: {}", e))?;

        *self = new_exec;
        Ok(ResultSet::ok_msg("vacuum executed"))
    }

    fn exec_alter_table(&mut self, stmt: crate::parser::ast::AlterTableStmt) -> Result<ResultSet, String> {
        if !self.catalog.table_exists(&stmt.table) {
            return Err(format!("table '{}' does not exist", stmt.table));
        }
        match stmt.op {
            crate::parser::ast::AlterTableOp::RenameTo(new_name) => {
                self.catalog.rename_table(&stmt.table, &new_name)?;
                Ok(ResultSet::ok_msg("table renamed"))
            }
            crate::parser::ast::AlterTableOp::AddColumn { name, data_type } => {
                use crate::table::schema::{Column, DataType};
                use crate::parser::ast::SqlType;
                let dt = match data_type {
                    SqlType::Integer => DataType::Integer,
                    SqlType::Real    => DataType::Float,
                    SqlType::Text    => DataType::Text,
                    SqlType::Blob    => DataType::Text,
                    SqlType::Boolean => DataType::Boolean,
                    SqlType::Null    => DataType::Text,
                };
                self.catalog.add_column(&stmt.table, &name, Column::new(&name, dt))?;
                Ok(ResultSet::ok_msg("column added"))
            }
        }
    }

    fn exec_pragma(&mut self, name: String, value: Option<crate::parser::ast::Expr>) -> Result<ResultSet, String> {
        let storage = self.storage.lock();
        match name.to_lowercase().as_str() {
            "journal_mode" => {
                let mode = if storage.is_wal() { "wal" } else { "delete" };
                if value.is_some() {
                    return Err("PRAGMA journal_mode cannot be set".to_string());
                }
                Ok(ResultSet { columns: vec!["journal_mode".into()], rows: vec![vec![Value::Text(mode.into())]], affected: 0, lastrowid: None })
            }
            "cache_size" => {
                let size = if let Some(expr) = value {
                    match expr {
                        crate::parser::ast::Expr::LitInt(n) => {
                            self.cache_size = n as usize;
                            n as i64
                        }
                        _ => return Err("cache_size must be an integer".to_string()),
                    }
                } else {
                    self.cache_size as i64
                };
                Ok(ResultSet { columns: vec!["cache_size".into()], rows: vec![vec![Value::Integer(size)]], affected: 0, lastrowid: None })
            }
            "page_size" => {
                let size = storage.page_size();
                Ok(ResultSet { columns: vec!["page_size".into()], rows: vec![vec![Value::Integer(size as i64)]], affected: 0, lastrowid: None })
            }
            "freelist_count" => {
                let count = storage.freelist_count();
                Ok(ResultSet { columns: vec!["freelist_count".into()], rows: vec![vec![Value::Integer(count as i64)]], affected: 0, lastrowid: None })
            }
            "table_info" => {
                if let Some(expr) = value {
                    if let crate::parser::ast::Expr::LitStr(table_name) = expr {
                        let meta = self.catalog.get_table(&table_name)
                            .ok_or_else(|| format!("no such table: {}", table_name))?;
                        let columns = vec!["cid".into(), "name".into(), "type".into(),
                                          "notnull".into(), "dflt_value".into(), "pk".into()];
                        let rows: Vec<Vec<Value>> = meta.schema.columns.iter().enumerate()
                            .map(|(i, col)| {
                                let col_type = match col.data_type {
                                    crate::table::schema::DataType::Integer => "INTEGER",
                                    crate::table::schema::DataType::Float => "REAL",
                                    crate::table::schema::DataType::Text => "TEXT",
                                    crate::table::schema::DataType::Boolean => "BOOLEAN",
                                };
                                vec![
                                    Value::Integer(i as i64),
                                    Value::Text(col.name.clone()),
                                    Value::Text(col_type.to_string()),
                                    Value::Integer(if col.nullable { 0 } else { 1 }),
                                    Value::Null,
                                    Value::Integer(0),
                                ]
                            })
                            .collect();
                        Ok(ResultSet { columns, rows, affected: 0, lastrowid: None })
                    } else {
                        Err("table_info requires a table name".to_string())
                    }
                } else {
                    Err("table_info requires a table name".to_string())
                }
            }
            "index_list" => {
                if let Some(expr) = value {
                    if let crate::parser::ast::Expr::LitStr(table_name) = expr {
                        let columns = vec!["seq".into(), "name".into(), "unique".into(),
                                          "origin".into(), "partial".into()];
                        let mut rows = Vec::new();
                        for (i, idx) in self.catalog.index_names().iter().enumerate() {
                            let idx_meta = self.catalog.get_index(idx).unwrap();
                            if idx_meta.table == *table_name {
                                rows.push(vec![
                                    Value::Integer(i as i64),
                                    Value::Text(idx.to_string()),
                                    Value::Integer(if idx_meta.unique { 1 } else { 0 }),
                                    Value::Text("u".to_string()),
                                    Value::Integer(0),
                                ]);
                            }
                        }
                        Ok(ResultSet { columns, rows, affected: 0, lastrowid: None })
                    } else {
                        Err("index_list requires a table name".to_string())
                    }
                } else {
                    Err("index_list requires a table name".to_string())
                }
            }
            "index_info" => {
                if let Some(expr) = value {
                    if let crate::parser::ast::Expr::LitStr(index_name) = expr {
                        if let Some(idx_meta) = self.catalog.get_index(&index_name) {
                            let columns = vec!["seqno".into(), "cid".into(), "name".into()];
                            let rows: Vec<Vec<Value>> = idx_meta.columns.iter().enumerate()
                                .map(|(i, col)| {
                                    let cid = self.catalog.get_table(&idx_meta.table)
                                        .and_then(|m| m.schema.index_of(col))
                                        .map(|v| v as i64)
                                        .unwrap_or(-1);
                                    vec![
                                        Value::Integer(i as i64),
                                        Value::Integer(cid),
                                        Value::Text(col.clone()),
                                    ]
                                })
                                .collect();
                            Ok(ResultSet { columns, rows, affected: 0, lastrowid: None })
                        } else {
                            Err(format!("no such index: {}", index_name))
                        }
                    } else {
                        Err("index_info requires an index name".to_string())
                    }
                } else {
                    Err("index_info requires an index name".to_string())
                }
            }
            _ => Err(format!("unknown pragma: {}", name)),
        }
    }

    fn exec_explain(&mut self, inner: Plan) -> Result<ResultSet, String> {
        let plan_desc = format!("{:?}", inner);
        Ok(ResultSet {
            columns: vec!["plan".into()],
            rows: vec![vec![Value::Text(plan_desc)]],
            affected: 0,
            lastrowid: None,
        })
    }

    /// 把運算式中的子查詢預先求值為字面值（需要 &mut self）
    fn resolve_expr(&mut self, expr: Expr) -> Result<Expr, String> {
        use crate::parser::ast::*;
        match expr {
            Expr::ScalarSubquery(query) => {
                let plan = crate::planner::planner::Planner::new(&self.catalog).plan(
                    Statement::Select(*query))?;
                let rs = self.execute(plan)?;
                let val = rs.rows.into_iter().next()
                    .and_then(|r| r.into_iter().next())
                    .unwrap_or(Value::Null);
                Ok(expr_from_value(val))
            }
            Expr::InSubquery { expr: inner, query, negated } => {
                let plan = crate::planner::planner::Planner::new(&self.catalog).plan(
                    Statement::Select(*query))?;
                let rs = self.execute(plan)?;
                let list: Vec<Expr> = rs.rows.into_iter()
                    .filter_map(|r| r.into_iter().next())
                    .map(expr_from_value)
                    .collect();
                Ok(Expr::InList { expr: inner, list, negated })
            }
            Expr::Exists { query, negated } => {
                let plan = crate::planner::planner::Planner::new(&self.catalog).plan(
                    Statement::Select(*query))?;
                let rs = self.execute(plan)?;
                let exists = !rs.rows.is_empty();
                Ok(Expr::LitBool(if negated { !exists } else { exists }))
            }
            Expr::ScalarSubquery(query) => {
                let plan = crate::planner::planner::Planner::new(&self.catalog).plan(
                    Statement::Select(*query))?;
                let rs = self.execute(plan)?;
                if rs.rows.is_empty() {
                    Ok(Expr::LitNull)
                } else {
                    let first_row = &rs.rows[0];
                    if first_row.is_empty() {
                        Ok(Expr::LitNull)
                    } else {
                        Ok(expr_from_value(first_row[0].clone()))
                    }
                }
            }
            Expr::BinOp { left, op, right } => {
                let left  = self.resolve_expr(*left)?;
                let right = self.resolve_expr(*right)?;
                Ok(Expr::BinOp { left: Box::new(left), op, right: Box::new(right) })
            }
            other => Ok(other),
        }
    }

    fn exec_subquery_scan(&mut self, plan: Plan, alias: String) -> Result<ResultSet, String> {
        // 執行子查詢，把結果存為暫時「虛擬表」
        let mut result = self.execute(plan)?;
        // 給欄位加上 alias 前綴（alias.col）
        result.columns = result.columns.iter()
            .map(|c| format!("{}.{}", alias, c))
            .collect();
        Ok(result)
    }

    fn exec_cte(&mut self, defs: Vec<(String, Box<Plan>)>, query: Plan) -> Result<ResultSet, String> {
        // 依序執行每個 CTE，暫存為虛擬結果集
        for (name, plan) in defs {
            let rs = self.execute(*plan)?;
            // 把 CTE 結果塞進 cte_cache（以 table name 儲存）
            self.cte_cache.insert(name, rs);
        }
        let result = self.execute(query)?;
        // 清除 CTE 快取（避免污染後續查詢）
        self.cte_cache.clear();
        Ok(result)
    }

    fn exec_set_operation(&mut self, left: Plan, right: Plan, op: crate::planner::plan::SetOp) -> Result<ResultSet, String> {
        let left_rs = self.execute(left)?;
        let right_rs = self.execute(right)?;

        // 合併 column names（假設兩邊 column 結構相同）
        let columns = left_rs.columns.clone();

        let mut rows = left_rs.rows;
        rows.extend(right_rs.rows);

        // UNION 需要去除重複
        if matches!(op, crate::planner::plan::SetOp::Union) {
            let mut seen = std::collections::HashSet::new();
            rows.retain(|row| {
                let key = format!("{:?}", row);
                seen.insert(key)
            });
        }

        Ok(ResultSet { columns, rows, affected: 0, lastrowid: None })
    }

    fn exec_transaction(&mut self, op: TransactionOp) -> Result<ResultSet, String> {
        match op {
            TransactionOp::Begin => {
                // 記錄所有表的 row count snapshot
                let counts: std::collections::HashMap<String, usize> = self.tables.iter()
                    .map(|(name, tbl)| (name.clone(), tbl.len()))
                    .collect();
                self.txn_mgr.begin(counts)?;
                Ok(ResultSet::ok_msg("transaction begun"))
            }
            TransactionOp::Commit => {
                self.txn_mgr.commit()?;
                Ok(ResultSet::ok_msg("committed"))
            }
            TransactionOp::Rollback => {
                let snap = self.txn_mgr.rollback()?;
                // 1. 刪除交易中新建的表（不在 snapshot 中的表）
                let snap_tables: std::collections::HashSet<_> = snap.row_counts.keys().cloned().collect();
                let to_delete: Vec<_> = self.tables.keys()
                    .filter(|name| !snap_tables.contains(*name))
                    .cloned()
                    .collect();
                for name in to_delete {
                    self.tables.remove(&name);
                    self.catalog.drop_table(&name).ok();
                }
                // 2. 還原 snapshot 中的表（截斷多餘的資料）
                for (name, count) in &snap.row_counts {
                    if let Some(tbl) = self.tables.get_mut(name) {
                        let current = tbl.scan();
                        let to_delete: Vec<_> = current.into_iter()
                            .skip(*count)
                            .filter_map(|r| match r.values.first() {
                                Some(crate::table::row::Value::Integer(v)) =>
                                    Some(crate::btree::node::Key::Integer(*v)),
                                Some(crate::table::row::Value::Text(s)) =>
                                    Some(crate::btree::node::Key::Text(s.clone())),
                                _ => None,
                            })
                            .collect();
                        for key in to_delete {
                            tbl.delete(&key);
                        }
                    }
                }
                Ok(ResultSet::ok_msg("rolled back"))
            }
        }
    }

    // ── 輔助 ──────────────────────────────────────────────────────────────

    fn col_names(&self, table: &str) -> Result<Vec<String>, String> {
        if let Some((alias, name)) = table.split_once('.') {
            self.attached.get(alias)
                .and_then(|db| db.catalog.get_table(name))
                .map(|m| m.schema.columns.iter().map(|c| c.name.clone()).collect())
                .ok_or_else(|| format!("table '{}' not found", table))
        } else {
            self.catalog.get_table(table)
                .map(|m| m.schema.columns.iter().map(|c| c.name.clone()).collect())
                .ok_or_else(|| format!("table '{}' not found", table))
        }
    }

fn get_table(&mut self, name: &str) -> Result<&mut Table<SharedStorage>, String> {
        if let Some((alias, table)) = name.split_once('.') {
            let db = self.attached.get_mut(alias)
                .ok_or_else(|| format!("no such attached database: {}", alias))?;
            if !db.tables.contains_key(table) {
                let meta = db.catalog.get_table(table)
                    .ok_or_else(|| format!("table '{}' not found in '{}'", table, alias))?.clone();
                let tbl = if meta.root_page != usize::MAX && meta.root_page > 0 {
                    Table::open(table, meta.schema.clone(), db.storage.clone(), meta.root_page, meta.row_count)
                } else {
                    let tbl = Table::new(table, meta.schema.clone(), db.storage.clone());
                    let root = tbl.root_page();
                    db.catalog.update_table_meta(table, root, 0)?;
                    tbl
                };
                db.tables.insert(table.to_string(), tbl);
            }
            db.tables.get_mut(table).ok_or_else(|| "internal error".to_string())
        } else {
            if !self.tables.contains_key(name) {
                let meta = self.catalog.get_table(name)
                    .ok_or_else(|| format!("table '{}' not found", name))?.clone();
                let tbl = if meta.root_page != usize::MAX && meta.root_page > 0 {
                    Table::open(name, meta.schema.clone(), self.storage.clone(), meta.root_page, meta.row_count)
                } else {
                    let tbl = Table::new(name, meta.schema.clone(), self.storage.clone());
                    let root = tbl.root_page();
                    self.catalog.update_table_meta(name, root, 0)?;
                    tbl
                };
                self.tables.insert(name.to_string(), tbl);
            }
            self.tables.get_mut(name).ok_or_else(|| "internal error".to_string())
        }
    }
}

// ── 運算式求值 ────────────────────────────────────────────────────────────

fn random_i64() -> i64 {
    // 簡單的線性同餘偽隨機（不需要 rand crate）
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now().duration_since(UNIX_EPOCH)
        .unwrap_or_default().subsec_nanos() as i64;
    seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407)
}

fn expr_from_value(v: Value) -> crate::parser::ast::Expr {
    use crate::parser::ast::Expr;
    match v {
        Value::Integer(i) => Expr::LitInt(i),
        Value::Float(f)   => Expr::LitFloat(f),
        Value::Text(s)    => Expr::LitStr(s),
        Value::Boolean(b) => Expr::LitBool(b),
        Value::Null       => Expr::LitNull,
    }
}

pub(crate) fn eval_expr(expr: &Expr, row: &Row, cols: &[String]) -> Result<Value, String> {
    match expr {
        Expr::LitInt(v)   => Ok(Value::Integer(*v)),
        Expr::LitFloat(v) => Ok(Value::Float(*v)),
        Expr::LitStr(s)   => Ok(Value::Text(s.clone())),
        Expr::LitBool(b)  => Ok(Value::Boolean(*b)),
        Expr::LitNull     => Ok(Value::Null),

        Expr::Column { name, .. } => {
            let idx = cols.iter().position(|c| c == name)
                .ok_or_else(|| format!("column '{}' not found", name))?;
            Ok(row.values.get(idx).cloned().unwrap_or(Value::Null))
        }

        Expr::BinOp { left, op, right } => {
            let l = eval_expr(left, row, cols)?;
            let r = eval_expr(right, row, cols)?;
            eval_binop(op, l, r)
        }

        Expr::UnaryOp { op, expr } => match (op, eval_expr(expr, row, cols)?) {
            (UnaryOp::Neg, Value::Integer(i)) => Ok(Value::Integer(-i)),
            (UnaryOp::Neg, Value::Float(f))   => Ok(Value::Float(-f)),
            (UnaryOp::Not, v)                 => Ok(Value::Boolean(!is_truthy(&v))),
            _ => Err("type error in unary op".into()),
        },

        Expr::IsNull { expr, negated } => {
            let is_null = matches!(eval_expr(expr, row, cols)?, Value::Null);
            Ok(Value::Boolean(if *negated { !is_null } else { is_null }))
        }

        Expr::Between { expr, low, high, negated } => {
            let v  = eval_expr(expr, row, cols)?;
            let lo = eval_expr(low,  row, cols)?;
            let hi = eval_expr(high, row, cols)?;
            let between = cmp_val(&v, &lo) != std::cmp::Ordering::Less
                       && cmp_val(&v, &hi) != std::cmp::Ordering::Greater;
            Ok(Value::Boolean(if *negated { !between } else { between }))
        }

        Expr::InList { expr, list, negated } => {
            let v = eval_expr(expr, row, cols)?;
            let found = list.iter().any(|e| {
                eval_expr(e, row, cols).map(|rv| cmp_val(&v, &rv) == std::cmp::Ordering::Equal).unwrap_or(false)
            });
            Ok(Value::Boolean(if *negated { !found } else { found }))
        }

        Expr::Like { expr, pattern, negated } => {
            if let (Value::Text(s), Value::Text(pat)) =
                (eval_expr(expr, row, cols)?, eval_expr(pattern, row, cols)?) {
                let m = sql_like(&s, &pat);
                Ok(Value::Boolean(if *negated { !m } else { m }))
            } else { Ok(Value::Boolean(false)) }
        }

        Expr::Glob { expr, pattern, negated } => {
            if let (Value::Text(s), Value::Text(pat)) =
                (eval_expr(expr, row, cols)?, eval_expr(pattern, row, cols)?) {
                let m = sql_glob(&s, &pat);
                Ok(Value::Boolean(if *negated { !m } else { m }))
            } else { Ok(Value::Boolean(false)) }
        }

        Expr::Cast { expr, to } => {
            let v = eval_expr(expr, row, cols)?;
            match to {
                SqlType::Integer => Ok(match v {
                    Value::Integer(i) => Value::Integer(i),
                    Value::Float(f) => Value::Integer(f as i64),
                    Value::Text(s) => Value::Integer(s.parse::<i64>().unwrap_or(0)),
                    Value::Null => Value::Null,
                    Value::Boolean(b) => Value::Integer(if b { 1 } else { 0 }),
                }),
                SqlType::Real => Ok(match v {
                    Value::Integer(i) => Value::Float(i as f64),
                    Value::Float(f) => Value::Float(f),
                    Value::Text(s) => Value::Float(s.parse::<f64>().unwrap_or(0.0)),
                    Value::Null => Value::Null,
                    Value::Boolean(b) => Value::Float(if b { 1.0 } else { 0.0 }),
                }),
                SqlType::Text => Ok(match v {
                    Value::Text(s) => Value::Text(s),
                    Value::Integer(i) => Value::Text(i.to_string()),
                    Value::Float(f) => Value::Text(f.to_string()),
                    Value::Boolean(b) => Value::Text(if b { "1".to_string() } else { "0".to_string() }),
                    Value::Null => Value::Null,
                }),
                SqlType::Blob => Ok(Value::Text(v.to_string())),
                SqlType::Boolean => Ok(match v {
                    Value::Boolean(b) => Value::Boolean(b),
                    Value::Integer(i) => Value::Boolean(i != 0),
                    Value::Text(s) => Value::Boolean(s == "1" || s.eq_ignore_ascii_case("true")),
                    Value::Null => Value::Null,
                    Value::Float(f) => Value::Boolean(f != 0.0),
                }),
                SqlType::Null => Ok(Value::Null),
            }
        }

        Expr::Function { name, args, .. } => {
            // 先把參數求值為字串（日期函式需要）
            let eval_str_args = || -> Vec<String> {
                args.iter().map(|a| eval_expr(a, row, cols)
                    .unwrap_or(Value::Null).to_string()).collect()
            };
            match name.as_str() {
                "UPPER"    => match eval_expr(&args[0], row, cols)? {
                    Value::Text(s) => Ok(Value::Text(s.to_uppercase())), v => Ok(v) },
                "LOWER"    => match eval_expr(&args[0], row, cols)? {
                    Value::Text(s) => Ok(Value::Text(s.to_lowercase())), v => Ok(v) },
                "LENGTH"   => match eval_expr(&args[0], row, cols)? {
                    Value::Text(s) => Ok(Value::Integer(s.len() as i64)), _ => Ok(Value::Null) },
                "ABS"      => match eval_expr(&args[0], row, cols)? {
                    Value::Integer(i) => Ok(Value::Integer(i.abs())),
                    Value::Float(f)   => Ok(Value::Float(f.abs())),
                    v => Ok(v) },
                "ROUND"    => {
                    let v = eval_expr(&args[0], row, cols)?;
                    let digits = if args.len() > 1 {
                        match eval_expr(&args[1], row, cols)? { Value::Integer(i) => i, _ => 0 }
                    } else { 0 };
                    let factor = 10f64.powi(digits as i32);
                    match v {
                        Value::Float(f)   => Ok(Value::Float((f * factor).round() / factor)),
                        Value::Integer(i) => Ok(Value::Integer(i)),
                        _ => Ok(Value::Null),
                    }
                },
                "CEIL" | "CEILING" => match eval_expr(&args[0], row, cols)? {
                    Value::Float(f)   => Ok(Value::Float(f.ceil())),
                    Value::Integer(i) => Ok(Value::Integer(i)),
                    _ => Ok(Value::Null) },
                "FLOOR"    => match eval_expr(&args[0], row, cols)? {
                    Value::Float(f)   => Ok(Value::Float(f.floor())),
                    Value::Integer(i) => Ok(Value::Integer(i)),
                    _ => Ok(Value::Null) },
                "RANDOM"   => Ok(Value::Integer(random_i64())),
                "TYPEOF"   => {
                    let v = eval_expr(&args[0], row, cols)?;
                    Ok(Value::Text(match v {
                        Value::Integer(_) => "integer", Value::Float(_) => "real",
                        Value::Text(_) => "text", Value::Null => "null",
                        Value::Boolean(_) => "integer",
                    }.to_string()))
                },
                "IFNULL" | "NVL" => {
                    let v = eval_expr(&args[0], row, cols)?;
                    if matches!(v, Value::Null) { eval_expr(&args[1], row, cols) } else { Ok(v) }
                },
                "NULLIF" => {
                    let a = eval_expr(&args[0], row, cols)?;
                    let b = eval_expr(&args[1], row, cols)?;
                    if cmp_val(&a, &b) == std::cmp::Ordering::Equal { Ok(Value::Null) } else { Ok(a) }
                },
                "COALESCE" => {
                    for a in args { let v = eval_expr(a, row, cols)?; if !matches!(v, Value::Null) { return Ok(v); } }
                    Ok(Value::Null)
                },
                "SUBSTR" | "SUBSTRING" => {
                    if let Value::Text(s) = eval_expr(&args[0], row, cols)? {
                        let start = match eval_expr(&args[1], row, cols)? { Value::Integer(i) => (i - 1).max(0) as usize, _ => 0 };
                        let chars: Vec<char> = s.chars().collect();
                        let result: String = if args.len() > 2 {
                            let len = match eval_expr(&args[2], row, cols)? { Value::Integer(i) => i as usize, _ => 0 };
                            chars[start.min(chars.len())..].iter().take(len).collect()
                        } else {
                            chars[start.min(chars.len())..].iter().collect()
                        };
                        Ok(Value::Text(result))
                    } else { Ok(Value::Null) }
                },
                "TRIM"   => match eval_expr(&args[0], row, cols)? {
                    Value::Text(s) => Ok(Value::Text(s.trim().to_string())), v => Ok(v) },
                "LTRIM"  => match eval_expr(&args[0], row, cols)? {
                    Value::Text(s) => Ok(Value::Text(s.trim_start().to_string())), v => Ok(v) },
                "RTRIM"  => match eval_expr(&args[0], row, cols)? {
                    Value::Text(s) => Ok(Value::Text(s.trim_end().to_string())), v => Ok(v) },
                "REPLACE" => {
                    if let (Value::Text(s), Value::Text(from), Value::Text(to)) = (
                        eval_expr(&args[0], row, cols)?,
                        eval_expr(&args[1], row, cols)?,
                        eval_expr(&args[2], row, cols)?) {
                        Ok(Value::Text(s.replace(&from, &to)))
                    } else { Ok(Value::Null) }
                },
                "INSTR" => {
                    if let (Value::Text(s), Value::Text(needle)) = (
                        eval_expr(&args[0], row, cols)?, eval_expr(&args[1], row, cols)?) {
                        Ok(Value::Integer(s.find(&needle).map(|i| i as i64 + 1).unwrap_or(0)))
                    } else { Ok(Value::Null) }
                },
                // ── 日期時間函式 ────────────────────────────────────────
                "DATE"      => {
                    let str_args = eval_str_args();
                    Ok(crate::planner::datetime::fn_date(&str_args)
                        .map(Value::Text).unwrap_or(Value::Null))
                },
                "TIME"      => {
                    let str_args = eval_str_args();
                    Ok(crate::planner::datetime::fn_time(&str_args)
                        .map(Value::Text).unwrap_or(Value::Null))
                },
                "DATETIME"  => {
                    let str_args = eval_str_args();
                    Ok(crate::planner::datetime::fn_datetime(&str_args)
                        .map(Value::Text).unwrap_or(Value::Null))
                },
                "JULIANDAY" => {
                    let str_args = eval_str_args();
                    Ok(crate::planner::datetime::fn_julianday(&str_args)
                        .map(Value::Float).unwrap_or(Value::Null))
                },
                "STRFTIME"  => {
                    let str_args = eval_str_args();
                    Ok(crate::planner::datetime::fn_strftime(&str_args)
                        .map(Value::Text).unwrap_or(Value::Null))
                },
                "NOW"       => Ok(Value::Text(
                    crate::planner::datetime::fn_datetime(&vec!["now".into()])
                        .unwrap_or_default())),
                _ => Ok(Value::Null),
            }
        },

        Expr::ScalarSubquery(_) | Expr::InSubquery { .. } | Expr::Exists { .. } => {
            // 子查詢 expr 需要 Executor，在 exec_subquery_expr 中處理
            // 這裡回傳 sentinel，呼叫端應先透過 resolve_subquery_exprs 預處理
            Err("subquery expressions must be resolved before eval_expr".to_string())
        }

        _ => Err(format!("unsupported expr: {:?}", expr)),
    }
}

fn eval_aggregate(expr: &Expr, rows: &[Row], cols: &[String]) -> Result<Value, String> {
    if let Expr::Function { name, args, distinct } = expr {
        let vals: Vec<Value> = rows.iter().map(|r| {
            if args.is_empty() || matches!(args[0], Expr::Column { name: ref n, .. } if n == "*") {
                Ok(Value::Integer(1))
            } else { eval_expr(&args[0], r, cols) }
        }).collect::<Result<_, String>>()?;

        match name.as_str() {
            "COUNT" => {
                if *distinct {
                    // COUNT(DISTINCT col) - 需要去除重複
                    let mut seen = std::collections::HashSet::new();
                    let count = vals.iter()
                        .filter(|v| !matches!(v, Value::Null))
                        .filter(|v| seen.insert(format!("{:?}", v)))
                        .count();
                    Ok(Value::Integer(count as i64))
                } else {
                    Ok(Value::Integer(vals.iter().filter(|v| !matches!(v, Value::Null)).count() as i64))
                }
            }
            "SUM" => {
                let s: f64 = vals.iter().filter_map(|v| match v {
                    Value::Integer(i) => Some(*i as f64), Value::Float(f) => Some(*f), _ => None }).sum();
                Ok(Value::Float(s))
            }
            "AVG" => {
                let ns: Vec<f64> = vals.iter().filter_map(|v| match v {
                    Value::Integer(i) => Some(*i as f64), Value::Float(f) => Some(*f), _ => None }).collect();
                if ns.is_empty() { Ok(Value::Null) } else { Ok(Value::Float(ns.iter().sum::<f64>() / ns.len() as f64)) }
            }
            "MAX" => Ok(vals.into_iter().filter(|v| !matches!(v, Value::Null)).max_by(cmp_val).unwrap_or(Value::Null)),
            "MIN" => Ok(vals.into_iter().filter(|v| !matches!(v, Value::Null)).min_by(cmp_val).unwrap_or(Value::Null)),
            _ => Ok(Value::Null),
        }
    } else if !rows.is_empty() {
        eval_expr(expr, &rows[0], cols)
    } else {
        Ok(Value::Null)
    }
}

fn eval_binop(op: &BinOp, l: Value, r: Value) -> Result<Value, String> {
    match op {
        BinOp::And => Ok(Value::Boolean(is_truthy(&l) && is_truthy(&r))),
        BinOp::Or  => Ok(Value::Boolean(is_truthy(&l) || is_truthy(&r))),
        BinOp::Eq    => Ok(Value::Boolean(cmp_val(&l, &r) == std::cmp::Ordering::Equal)),
        BinOp::NotEq => Ok(Value::Boolean(cmp_val(&l, &r) != std::cmp::Ordering::Equal)),
        BinOp::Lt    => Ok(Value::Boolean(cmp_val(&l, &r) == std::cmp::Ordering::Less)),
        BinOp::LtEq  => Ok(Value::Boolean(cmp_val(&l, &r) != std::cmp::Ordering::Greater)),
        BinOp::Gt    => Ok(Value::Boolean(cmp_val(&l, &r) == std::cmp::Ordering::Greater)),
        BinOp::GtEq  => Ok(Value::Boolean(cmp_val(&l, &r) != std::cmp::Ordering::Less)),
        BinOp::Add => num_op(l, r, |a,b| a+b, |a,b| a+b),
        BinOp::Sub => num_op(l, r, |a,b| a-b, |a,b| a-b),
        BinOp::Mul => num_op(l, r, |a,b| a*b, |a,b| a*b),
        BinOp::Div => num_op(l, r, |a,b| a/b, |a,b| a/b),
        BinOp::Mod => num_op(l, r, |a,b| a%b, |a,b| a%b),
        BinOp::Concat => match (l, r) {
            (Value::Text(a), Value::Text(b)) => Ok(Value::Text(a + &b)),
            _ => Err("|| requires TEXT".into()),
        },
    }
}

fn num_op(l: Value, r: Value, ii: impl Fn(i64,i64)->i64, ff: impl Fn(f64,f64)->f64) -> Result<Value, String> {
    match (l, r) {
        (Value::Integer(a), Value::Integer(b)) => Ok(Value::Integer(ii(a, b))),
        (Value::Float(a),   Value::Float(b))   => Ok(Value::Float(ff(a, b))),
        (Value::Integer(a), Value::Float(b))   => Ok(Value::Float(ff(a as f64, b))),
        (Value::Float(a),   Value::Integer(b)) => Ok(Value::Float(ff(a, b as f64))),
        _ => Err("type error in arithmetic".into()),
    }
}

fn eval_literal(expr: &Expr) -> Result<Value, String> {
    match expr {
        Expr::LitInt(v)   => Ok(Value::Integer(*v)),
        Expr::LitFloat(v) => Ok(Value::Float(*v)),
        Expr::LitStr(s)   => Ok(Value::Text(s.clone())),
        Expr::LitBool(b)  => Ok(Value::Boolean(*b)),
        Expr::LitNull     => Ok(Value::Null),
        // 負數字面值（parser 會產生 UnaryOp Neg）
        Expr::UnaryOp { op: UnaryOp::Neg, expr } => match eval_literal(expr)? {
            Value::Integer(i) => Ok(Value::Integer(-i)),
            Value::Float(f)   => Ok(Value::Float(-f)),
            v => Err(format!("cannot negate {:?}", v)),
        },
        _ => Err(format!("expected literal, got {:?}", expr)),
    }
}

fn expr_to_key(expr: &Expr) -> Result<Key, String> {
    match expr {
        Expr::LitInt(v) => Ok(Key::Integer(*v)),
        Expr::LitStr(s) => Ok(Key::Text(s.clone())),
        _ => Err("unsupported key expression".into()),
    }
}

fn row_to_key(row: &Row) -> Result<Key, String> {
    match row.values.first() {
        Some(Value::Integer(v)) => Ok(Key::Integer(*v)),
        Some(Value::Text(s))    => Ok(Key::Text(s.clone())),
        _ => Err("cannot extract key from row".into()),
    }
}

pub(crate) fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Boolean(b)  => *b,
        Value::Integer(i)  => *i != 0,
        Value::Float(f)    => *f != 0.0,
        Value::Text(s)     => !s.is_empty(),
        Value::Null        => false,
    }
}

fn cmp_val(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a, b) {
        (Value::Null, Value::Null) => Equal,
        (Value::Null, _) => Less,
        (_, Value::Null) => Greater,
        (Value::Integer(x), Value::Integer(y)) => x.cmp(y),
        (Value::Float(x),   Value::Float(y))   => x.partial_cmp(y).unwrap_or(Equal),
        (Value::Integer(x), Value::Float(y))   => (*x as f64).partial_cmp(y).unwrap_or(Equal),
        (Value::Float(x),   Value::Integer(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Equal),
        (Value::Text(x),    Value::Text(y))    => x.cmp(y),
        (Value::Boolean(x), Value::Boolean(y)) => x.cmp(y),
        _ => Equal,
    }
}

fn sql_like(s: &str, pat: &str) -> bool {
    let s: Vec<char> = s.chars().collect();
    let p: Vec<char> = pat.chars().collect();
    like_match(&s, &p)
}

fn like_match(s: &[char], p: &[char]) -> bool {
    match (s, p) {
        (_, [])              => s.is_empty(),
        (_, ['%', rest @ ..]) => {
            if rest.is_empty() { return true; }
            (0..=s.len()).any(|i| like_match(&s[i..], rest))
        }
        ([], _) => false,
        ([sc, sr @ ..], [pc, pr @ ..]) => {
            (*pc == '_' || pc.to_uppercase().eq(sc.to_uppercase())) && like_match(sr, pr)
        }
    }
}

fn sql_glob(s: &str, pat: &str) -> bool {
    glob_match(s, pat)
}

fn glob_match(s: &str, p: &str) -> bool {
    let s_chars: Vec<char> = s.chars().collect();
    let p_chars: Vec<char> = p.chars().collect();
    let s_len = s_chars.len();
    let p_len = p_chars.len();

    let mut dp = vec![vec![false; p_len + 1]; s_len + 1];
    dp[0][0] = true;

    for j in 1..=p_len {
        if p_chars[j-1] == '*' {
            dp[0][j] = dp[0][j-1];
        }
    }

    for i in 1..=s_len {
        for j in 1..=p_len {
            if p_chars[j-1] == '*' {
                dp[i][j] = dp[i-1][j] || dp[i][j-1];
            } else if p_chars[j-1] == '?' || s_chars[i-1] == p_chars[j-1] {
                dp[i][j] = dp[i-1][j-1];
            } else {
                dp[i][j] = false;
            }
        }
    }

    dp[s_len][p_len]
}

fn expr_name(expr: &Expr) -> String {
    match expr {
        Expr::Column { name, .. }   => name.clone(),
        Expr::Function { name, .. } => name.clone(),
        _ => "?".into(),
    }
}

// ── 測試 ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;
    use crate::planner::planner::Planner;

    fn run(exec: &mut Executor, sql: &str) -> ResultSet {
        let stmts = parse(sql).unwrap_or_else(|e| panic!("parse: {}", e));
        let mut last = ResultSet::empty();
        for stmt in stmts {
            let plan = Planner::new(exec.catalog()).plan(stmt)
                .unwrap_or_else(|e| panic!("plan: {}", e));
            last = exec.execute(plan).unwrap_or_else(|e| panic!("exec: {}", e));
        }
        last
    }

    fn setup() -> Executor {
        let mut e = Executor::new();
        run(&mut e, "CREATE TABLE users (id INTEGER, name TEXT, age INTEGER)");
        run(&mut e, "INSERT INTO users VALUES (1, 'Alice', 30)");
        run(&mut e, "INSERT INTO users VALUES (2, 'Bob',   25)");
        run(&mut e, "INSERT INTO users VALUES (3, 'Carol', 35)");
        e
    }

    #[test]
    fn create_and_select_all() {
        let mut e = setup();
        let r = run(&mut e, "SELECT * FROM users");
        assert_eq!(r.row_count(), 3);
        assert_eq!(r.columns, vec!["id", "name", "age"]);
    }

    #[test]
    fn select_where_eq() {
        let mut e = setup();
        let r = run(&mut e, "SELECT * FROM users WHERE id = 2");
        assert_eq!(r.row_count(), 1);
        assert_eq!(r.rows[0][1], Value::Text("Bob".into()));
    }

    #[test]
    fn select_projection() {
        let mut e = setup();
        let r = run(&mut e, "SELECT name, age FROM users");
        assert_eq!(r.columns, vec!["name", "age"]);
        assert_eq!(r.row_count(), 3);
    }

    #[test]
    fn select_order_by() {
        let mut e = setup();
        let r = run(&mut e, "SELECT * FROM users ORDER BY age ASC");
        assert_eq!(r.rows[0][1], Value::Text("Bob".into()));
        assert_eq!(r.rows[2][1], Value::Text("Carol".into()));
    }

    #[test]
    fn select_limit_offset() {
        let mut e = setup();
        let r = run(&mut e, "SELECT * FROM users ORDER BY id ASC LIMIT 2 OFFSET 1");
        assert_eq!(r.row_count(), 2);
        assert_eq!(r.rows[0][0], Value::Integer(2));
    }

    #[test]
    fn select_where_like() {
        let mut e = setup();
        let r = run(&mut e, "SELECT * FROM users WHERE name LIKE 'A%'");
        assert_eq!(r.row_count(), 1);
        assert_eq!(r.rows[0][1], Value::Text("Alice".into()));
    }

    #[test]
    fn select_where_between() {
        let mut e = setup();
        let r = run(&mut e, "SELECT * FROM users WHERE age BETWEEN 25 AND 32");
        assert_eq!(r.row_count(), 2);
    }

    #[test]
    fn select_where_in() {
        let mut e = setup();
        let r = run(&mut e, "SELECT * FROM users WHERE id IN (1, 3)");
        assert_eq!(r.row_count(), 2);
    }

    #[test]
    fn select_count() {
        let mut e = setup();
        let r = run(&mut e, "SELECT COUNT(*) FROM users");
        assert_eq!(r.rows[0][0], Value::Integer(3));
    }

    #[test]
    fn select_max_min() {
        let mut e = setup();
        let r = run(&mut e, "SELECT MAX(age), MIN(age) FROM users");
        assert_eq!(r.rows[0][0], Value::Integer(35));
        assert_eq!(r.rows[0][1], Value::Integer(25));
    }

    #[test]
    fn select_max_empty_group() {
        let mut e = setup();
        // GROUP BY + empty matches -> 0 rows
        let r = run(&mut e, "SELECT MAX(age) FROM users WHERE age > 100 GROUP BY name");
        assert_eq!(r.row_count(), 0);
        
        // NO GROUP BY + empty matches -> 1 row with NULL
        let r2 = run(&mut e, "SELECT MAX(age) FROM users WHERE age > 100");
        assert_eq!(r2.row_count(), 1);
        assert_eq!(r2.rows[0][0], Value::Null);
    }

    #[test]
    fn update_row() {
        let mut e = setup();
        run(&mut e, "UPDATE users SET age = 99 WHERE id = 1");
        let r = run(&mut e, "SELECT age FROM users WHERE id = 1");
        assert_eq!(r.rows[0][0], Value::Integer(99));
    }

    #[test]
    fn delete_row() {
        let mut e = setup();
        run(&mut e, "DELETE FROM users WHERE id = 2");
        let r = run(&mut e, "SELECT * FROM users");
        assert_eq!(r.row_count(), 2);
    }

    #[test]
    fn drop_table() {
        let mut e = setup();
        run(&mut e, "DROP TABLE users");
        assert!(!e.catalog().table_exists("users"));
    }

    #[test]
    fn transaction_stmts() {
        let mut e = Executor::new();
        run(&mut e, "BEGIN");
        run(&mut e, "CREATE TABLE t (id INTEGER)");
        run(&mut e, "COMMIT");
    }

    #[test]
    fn inner_join() {
        let mut e = Executor::new();
        run(&mut e, "CREATE TABLE orders (order_id INTEGER, user_id INTEGER, amount REAL)");
        run(&mut e, "INSERT INTO orders VALUES (1, 1, 99.9)");
        run(&mut e, "INSERT INTO orders VALUES (2, 2, 50.0)");
        let mut e2 = setup();
        // 重建 orders 在同一個 executor
        run(&mut e2, "CREATE TABLE orders (order_id INTEGER, user_id INTEGER, amount REAL)");
        run(&mut e2, "INSERT INTO orders VALUES (1, 1, 99.9)");
        run(&mut e2, "INSERT INTO orders VALUES (2, 2, 50.0)");
        let r = run(&mut e2, "SELECT * FROM users JOIN orders ON users.id = orders.user_id");
        assert_eq!(r.row_count(), 2);
    }

    #[test]
    fn string_functions() {
        let mut e = setup();
        let r = run(&mut e, "SELECT UPPER(name) FROM users WHERE id = 1");
        assert_eq!(r.rows[0][0], Value::Text("ALICE".into()));
    }

    #[test]
    fn math_functions() {
        let mut e = Executor::new();
        run(&mut e, "CREATE TABLE t (id INTEGER, val REAL)");
        run(&mut e, "INSERT INTO t VALUES (1, -3.7)");
        let r = run(&mut e, "SELECT ABS(val), ROUND(val, 1), CEIL(val), FLOOR(val) FROM t");
        assert_eq!(r.rows[0][0], Value::Float(3.7));
        assert_eq!(r.rows[0][1], Value::Float(-3.7));
        assert_eq!(r.rows[0][2], Value::Float(-3.0));
        assert_eq!(r.rows[0][3], Value::Float(-4.0));
    }

    #[test]
    fn string_functions_extended() {
        let mut e = setup();
        let r = run(&mut e, "SELECT SUBSTR(name, 1, 3) FROM users WHERE id = 1");
        assert_eq!(r.rows[0][0], Value::Text("Ali".into()));
        let r = run(&mut e, "SELECT TRIM('  hello  ')");
        assert_eq!(r.rows[0][0], Value::Text("hello".into()));
        let r = run(&mut e, "SELECT REPLACE(name, 'Alice', 'Alicia') FROM users WHERE id = 1");
        assert_eq!(r.rows[0][0], Value::Text("Alicia".into()));
        let r = run(&mut e, "SELECT LENGTH(name) FROM users WHERE id = 1");
        assert_eq!(r.rows[0][0], Value::Integer(5));
    }

    #[test]
    fn datetime_functions() {
        let mut e = Executor::new();
        run(&mut e, "CREATE TABLE events (id INTEGER, dt TEXT)");
        run(&mut e, "INSERT INTO events VALUES (1, '2024-03-15 10:30:00')");
        let r = run(&mut e, "SELECT DATE(dt), TIME(dt) FROM events WHERE id = 1");
        assert_eq!(r.rows[0][0], Value::Text("2024-03-15".into()));
        assert_eq!(r.rows[0][1], Value::Text("10:30:00".into()));
        let r = run(&mut e, "SELECT DATE('2024-03-15', '+5 days') FROM events WHERE id = 1");
        assert_eq!(r.rows[0][0], Value::Text("2024-03-20".into()));
        let r = run(&mut e, "SELECT STRFTIME('%Y/%m/%d', dt) FROM events WHERE id = 1");
        assert_eq!(r.rows[0][0], Value::Text("2024/03/15".into()));
    }

    #[test]
    fn not_null_constraint() {
        let mut e = Executor::new();
        run(&mut e, "CREATE TABLE t (id INTEGER, name TEXT NOT NULL)");
        let stmts = crate::parser::parse("INSERT INTO t VALUES (1, NULL)").unwrap();
        let plan = crate::planner::planner::Planner::new(e.catalog()).plan(stmts.into_iter().next().unwrap()).unwrap();
        assert!(e.execute(plan).is_err(), "NOT NULL should reject NULL");
        // non-null should succeed
        run(&mut e, "INSERT INTO t VALUES (2, 'Alice')");
        let r = run(&mut e, "SELECT * FROM t");
        assert_eq!(r.row_count(), 1);
    }

    #[test]
    fn unique_constraint() {
        let mut e = Executor::new();
        run(&mut e, "CREATE TABLE t (id INTEGER, email TEXT UNIQUE)");
        run(&mut e, "INSERT INTO t VALUES (1, 'alice@test.com')");
        let stmts = crate::parser::parse("INSERT INTO t VALUES (2, 'alice@test.com')").unwrap();
        let plan = crate::planner::planner::Planner::new(e.catalog()).plan(stmts.into_iter().next().unwrap()).unwrap();
        assert!(e.execute(plan).is_err(), "UNIQUE should reject duplicate");
    }

    #[test]
    fn subquery_in_where() {
        let mut e = setup();
        // IN subquery
        let r = run(&mut e, "SELECT * FROM users WHERE id IN (SELECT id FROM users WHERE age > 28)");
        // Alice(30) and Carol(35) qualify
        assert_eq!(r.row_count(), 2);
    }

    #[test]
    fn scalar_subquery() {
        let mut e = setup();
        let r = run(&mut e, "SELECT name FROM users WHERE age = (SELECT MAX(age) FROM users)");
        assert_eq!(r.row_count(), 1);
        assert_eq!(r.rows[0][0], Value::Text("Carol".into()));
    }

    #[test]
    fn cte_basic() {
        let mut e = setup();
        let r = run(&mut e, "WITH old_users AS (SELECT * FROM users WHERE age >= 30) SELECT name FROM old_users");
        // Alice(30) and Carol(35)
        assert_eq!(r.row_count(), 2);
    }

    #[test]
    fn cte_chained() {
        let mut e = setup();
        let r = run(&mut e,
            "WITH u AS (SELECT * FROM users WHERE age > 20),                   old AS (SELECT * FROM u WHERE age >= 30)              SELECT name FROM old ORDER BY name ASC");
        assert_eq!(r.row_count(), 2);
        assert_eq!(r.rows[0][0], Value::Text("Alice".into()));
    }

    #[test]
    fn ifnull_coalesce() {
        let mut e = Executor::new();
        run(&mut e, "CREATE TABLE t (id INTEGER, val TEXT)");
        run(&mut e, "INSERT INTO t VALUES (1, NULL)");
        let r = run(&mut e, "SELECT IFNULL(val, 'default') FROM t");
        assert_eq!(r.rows[0][0], Value::Text("default".into()));
        let r = run(&mut e, "SELECT COALESCE(val, 'fallback') FROM t");
        assert_eq!(r.rows[0][0], Value::Text("fallback".into()));
    }

    #[test]
    fn nullif_test() {
        let mut e = setup();
        let r = run(&mut e, "SELECT NULLIF(age, 30) FROM users WHERE id = 1");
        assert_eq!(r.rows[0][0], Value::Null);
        let r = run(&mut e, "SELECT NULLIF(age, 99) FROM users WHERE id = 1");
        assert_eq!(r.rows[0][0], Value::Integer(30));
    }

    #[test]
    fn create_index_stmt() {
        let mut e = Executor::new();
        run(&mut e, "CREATE TABLE users (id INTEGER, name TEXT, age INTEGER)");
        let r = run(&mut e, "CREATE INDEX idx_name ON users (name)");
        assert!(r.rows[0][0].to_string().contains("index created"));
        assert!(e.catalog().index_exists("idx_name"));
    }

    #[test]
    fn create_unique_index() {
        let mut e = Executor::new();
        run(&mut e, "CREATE TABLE t (id INTEGER, email TEXT)");
        let r = run(&mut e, "CREATE UNIQUE INDEX idx_email ON t (email)");
        assert!(r.rows[0][0].to_string().contains("index created"));
    }

    #[test]
    fn drop_index_stmt() {
        let mut e = Executor::new();
        run(&mut e, "CREATE TABLE t (id INTEGER, name TEXT)");
        run(&mut e, "CREATE INDEX idx_name ON t (name)");
        let r = run(&mut e, "DROP INDEX idx_name");
        assert!(r.rows[0][0].to_string().contains("index dropped"));
        assert!(!e.catalog().index_exists("idx_name"));
    }

    #[test]
    fn drop_index_if_exists() {
        let mut e = Executor::new();
        let r = run(&mut e, "DROP INDEX IF EXISTS idx_nonexistent");
        assert!(r.rows[0][0].to_string().contains("index does not exist"));
    }

    #[test]
    fn alter_table_rename() {
        let mut e = Executor::new();
        run(&mut e, "CREATE TABLE users (id INTEGER, name TEXT)");
        let r = run(&mut e, "ALTER TABLE users RENAME TO users_old");
        assert!(r.rows[0][0].to_string().contains("table renamed"));
        assert!(e.catalog().table_exists("users_old"));
        assert!(!e.catalog().table_exists("users"));
    }

    #[test]
    fn alter_table_add_column() {
        let mut e = Executor::new();
        run(&mut e, "CREATE TABLE users (id INTEGER, name TEXT)");
        let r = run(&mut e, "ALTER TABLE users ADD COLUMN email TEXT");
        assert!(r.rows[0][0].to_string().contains("column added"));
        let meta = e.catalog().get_table("users").unwrap();
        assert_eq!(meta.schema.columns.len(), 3);
        assert_eq!(meta.schema.columns[2].name, "email");
    }

    #[test]
    fn pragma_journal_mode() {
        let mut e = Executor::new();
        let r = run(&mut e, "PRAGMA journal_mode");
        assert_eq!(r.rows[0][0].to_string(), "delete");
    }

    #[test]
    fn pragma_cache_size() {
        let mut e = Executor::new();
        let r = run(&mut e, "PRAGMA cache_size");
        match &r.rows[0][0] {
            Value::Integer(size) => assert_eq!(*size, 256),
            _ => panic!("expected Integer"),
        }
    }

    #[test]
    fn pragma_page_size() {
        let mut e = Executor::new();
        let r = run(&mut e, "PRAGMA page_size");
        match &r.rows[0][0] {
            Value::Integer(size) => assert_eq!(*size, 4096),
            _ => panic!("expected Integer"),
        }
    }

    #[test]
    fn explain_stmt() {
        let mut e = Executor::new();
        run(&mut e, "CREATE TABLE users (id INTEGER, name TEXT)");
        let r = run(&mut e, "EXPLAIN SELECT * FROM users WHERE id = 1");
        assert!(r.rows[0][0].to_string().contains("SeqScan") || r.rows[0][0].to_string().contains("IndexScan"));
    }
}
