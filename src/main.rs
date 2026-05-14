//! sql6 主程式入口
//!
//! 支援三種執行模式：
//! - REPL（互動式命令列）
//! - Server 模式（stdio JSON RPC，透過 --server 啟動）
//! - WebSocket 模式（支援多客戶端，透過 --websocket 啟動）

#![allow(dead_code, unused)]

// 引入各模組
mod btree;    // B+Tree 索引實作
mod catalog;  // 系統目錄管理
mod fts;      // FTS5 全文檢索
mod interface; // 使用者介面（REPL/Server）
mod pager;    // 分頁管理與儲存引擎
mod parser;   // SQL 語法解析
mod planner;  // 查詢規劃與執行
mod table;    // 表格管理
mod vector;   // 向量搜尋 (v4.0)

use interface::{Repl, Server, WsServer};
use std::env::{self, Args};
use std::io::{self, Write};

// ============================================================================
// 主程式入口
// ============================================================================

fn main() {
    // 收集命令列參數
    let args: Vec<String> = env::args().collect();

    // 檢查是否為 WebSocket 模式（支援多客戶端）
    if let Some(idx) = args.iter().position(|s| s == "--websocket") {
        // 解析連接埠號（預設 8080）
        let port: u16 = args.get(idx + 1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(8080);
        // 解析資料庫路徑（可選）
        let db_path = args.get(idx + 2).map(|s| s.as_str());

        // 根據是否指定資料庫，建立對應的伺服器
        let mut server = if let Some(path) = db_path {
            eprintln!("啟動 WebSocket 伺服器，連接埠 {}，資料庫：{}", port, path);
            match WsServer::open(path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("無法開啟資料庫：{}", e);
                    std::process::exit(1);
                }
            }
        } else {
            eprintln!("啟動 WebSocket 伺服器，連接埠 {}（記憶體模式）", port);
            WsServer::new()
        };

        // 建立 tokio 非同步執行環境
        let runtime = tokio::runtime::Runtime::new().expect("無法建立 tokio 執行環境");
        // 執行非同步 WebSocket 伺服器
        runtime.block_on(async {
            server.run(port).await.expect("WebSocket 伺服器錯誤");
        });
        // 關閉伺服器
        server.shutdown();
        return;
    }

    // 檢查是否為 Server 模式（stdio JSON RPC）
    if args.contains(&"--server".to_string()) {
        // 解析資料庫路徑（--server 後的第一個參數）
        let db_path = args.iter().skip_while(|s| *s != "--server").nth(1);
        let mut server = if let Some(path) = db_path {
            // :memory: 視為記憶體模式，不建立磁碟檔案
            if path == ":memory:" {
                eprintln!("啟動 stdio 伺服器（記憶體模式）");
                Server::new()
            } else {
                eprintln!("啟動 stdio 伺服器，資料庫：{}", path);
                match Server::open(&path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("無法開啟資料庫：{}", e);
                        std::process::exit(1);
                    }
                }
            }
        } else {
            eprintln!("啟動 stdio 伺服器（記憶體模式）");
            Server::new()
        };
        // 執行伺服器主迴圈
        server.run();
        server.close();
        return;
    }

    // 預設為 REPL 互動模式
    let mut repl = if args.len() > 1 {
        // 有第二個參數，視為資料庫檔案路徑
        let db_path = &args[1];
        println!("開啟資料庫：{}", db_path);
        match Repl::open(db_path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("無法開啟資料庫：{}", e);
                std::process::exit(1);
            }
        }
    } else {
        // 無參數，建立記憶體資料庫
        Repl::new()
    };

    // 執行 REPL 主迴圈
    repl.run();
    repl.close();
}
