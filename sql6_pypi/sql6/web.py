# sql6 Web Server (FastAPI)

# 提供 Web 介面管理 sql6 資料庫

# 使用方式：
#   python -m sql6 --http 8080
#   python -m sql6 --http 8080 mydb.db

import os
import sys
import json
import subprocess
from typing import Optional, List, Dict, Any

from fastapi import FastAPI, HTTPException, Request, Response
from fastapi.staticfiles import StaticFiles
from fastapi.responses import HTMLResponse, JSONResponse
from pydantic import BaseModel
import re

# Helper: split SQL statements by semicolon, handling comments and strings
def split_sql_statements(sql: str) -> List[str]:
    """Split SQL by semicolons, skipping comments and string contents"""
    statements = []
    current = ""
    in_string = False
    string_char = ""
    in_line_comment = False
    in_block_comment = False

    for c in sql:
        # Handle escape
        if c == "\\" and not in_line_comment and not in_block_comment:
            current += c
            continue

        # Line comment
        if c == "-" and not in_string and not in_block_comment and not in_line_comment:
            if len(current) >= 1 and current[-1] == "-":
                current = current[:-1]
                in_line_comment = True
                continue
        if c == "\n" and in_line_comment:
            in_line_comment = False
            current += c
            continue
        if in_line_comment:
            continue

        # Block comment
        if c == "/" and not in_string and not in_line_comment:
            if len(current) >= 1 and current[-1] == "*":
                current = current[:-1]
                in_block_comment = True
                continue
        if c == "*" and in_block_comment:
            if len(current) >= 1 and current[-1] == "/":
                in_block_comment = False
                continue
        if in_block_comment:
            continue

        # String handling
        if c in ("'", '"') and not in_string:
            in_string = True
            string_char = c
        elif c == string_char and in_string:
            in_string = False
            string_char = ""
        elif c == ";" and not in_string:
            if current.strip() and not current.strip().startswith("--"):
                statements.append(current.strip())
            current = ""
            continue

        current += c

    if current.strip() and not current.strip().startswith("--"):
        statements.append(current.strip())

    return statements


app = FastAPI(title="sql6 Web Admin")

    """取得 sql6 二進位檔路徑"""

    """啟動 sql6 server 子程序"""
    global PROCESS, DB_PATH
    binary = get_binary_path()
    args = [binary, "--server"]
    if db_path:
        args.append(db_path)
        DB_PATH = db_path

    PROCESS = subprocess.Popen(
        args,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )

    line = PROCESS.stdout.readline()
    if not line:
        raise RuntimeError("Server failed to start")
    data = json.loads(line)
    if not data.get("ok"):
        raise RuntimeError(f"Server error: {data}")


def send_request(method: str, sql: Optional[str] = None, table: Optional[str] = None) -> Dict[str, Any]:
    """發送請求到 server"""
    if not PROCESS:
        raise HTTPException(500, "Server not running")

    request = {"method": method}
    if sql:
        request["sql"] = sql
    if table:
        request["table"] = table

    PROCESS.stdin.write(json.dumps(request) + "\n")
    PROCESS.stdin.flush()

    line = PROCESS.stdout.readline()
    if not line:
        raise HTTPException(500, "Server timeout")

    return json.loads(line)


def close_server():
    """關閉 server"""
    global PROCESS
    if PROCESS:
        try:
            PROCESS.stdin.write(json.dumps({"method": "close"}) + "\n")
            PROCESS.stdin.flush()
            PROCESS.terminate()
            PROCESS.wait(timeout=5)
        except:
            PROCESS.kill()
        PROCESS = None


class ExecuteRequest(BaseModel):
    sql: str
    params: Optional[List[Any]] = []


class InsertRequest(BaseModel):
    data: Dict[str, Any]


@app.on_event("startup")
def startup():
    """啟動時初始化 server"""
    start_server(DB_PATH)


@app.on_event("shutdown")
def shutdown():
    """關閉時清理"""
    close_server()


@app.get("/")
def index() -> HTMLResponse:
    """首頁"""
    html = """<!DOCTYPE html>
<html lang="zh-TW">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>sql6 Web Admin</title>
    <link rel="stylesheet" href="/static/styles.css">
</head>
<body>
    <div class="app">
        <header class="header">
            <h1>sql6 Web Admin</h1>
            <span class="db-path">""" + (DB_PATH or "in-memory") + """</span>
        </header>
        
        <div class="main">
            <aside class="sidebar">
                <div class="sidebar-section">
                    <h3>Tables</h3>
                    <ul id="table-list" class="nav-list">
                        <li class="loading">Loading...</li>
                    </ul>
                </div>
            </aside>
            
            <main class="content">
                <div class="tabs">
                    <button class="tab active" data-tab="query">SQL Query</button>
                    <button class="tab" data-tab="structure">Structure</button>
                </div>
                
                <div class="tab-content active" id="tab-query">
                    <div class="editor-container">
                        <textarea id="sql-editor" class="sql-editor" placeholder="SELECT * FROM users..."></textarea>
                        <div class="editor-actions">
                            <button id="execute-btn" class="btn primary">Execute (Ctrl+Enter)</button>
                            <button id="clear-btn" class="btn">Clear</button>
                        </div>
                    </div>
                    <div id="results" class="results">
                        <div class="placeholder">Execute a query to see results</div>
                    </div>
                </div>
                
                <div class="tab-content" id="tab-structure">
                    <div id="table-structure" class="placeholder">
                        Select a table from the sidebar
                    </div>
                </div>
            </main>
        </div>
    </div>
    
    <script src="/static/app.js"></script>
</body>
</html>"""
    return HTMLResponse(html)


@app.get("/api/tables")
def get_tables() -> JSONResponse:
    """取得所有表格"""
    result = send_request("tables")
    if result.get("ok"):
        return JSONResponse(result)
    raise HTTPException(500, result.get("error", "Unknown error"))


@app.get("/api/tables/{table_name}")
def get_table_schema(table_name: str) -> JSONResponse:
    """取得表格結構"""
    result = send_request("schema", table=table_name)
    if result.get("ok"):
        return JSONResponse(result)
    raise HTTPException(500, result.get("error", "Unknown error"))


@app.get("/api/table/{table_name}")
def get_table_info(table_name: str) -> JSONResponse:
    """取得表格完整資訊（結構 + 資料）"""
    # Get schema first
    schema_result = send_request("schema", table=table_name)
    if not schema_result.get("ok"):
        raise HTTPException(404, f"Table {table_name} not found")
    
    # Get data
    data_result = send_request("execute", sql=f"SELECT * FROM {table_name}")
    
    return JSONResponse({
        "ok": True,
        "schema": schema_result.get("rows", []),
        "data": data_result.get("rows", []),
        "columns": data_result.get("columns", []),
        "affected": data_result.get("affected", 0)
    })


@app.post("/api/execute")
def execute_query(req: ExecuteRequest) -> JSONResponse:
    """執行 SQL"""
    sql = req.sql.strip()
    if not sql:
        raise HTTPException(400, "Empty query")

    # Check for multiple statements
    stmts = split_sql_statements(sql)
    
    if len(stmts) == 0:
        raise HTTPException(400, "No valid SQL statements")
    
    if len(stmts) == 1:
        result = send_request("execute", sql=stmts[0])
    else:
        # Execute multiple and return all results
        all_results = []
        last_result = None
        for stmt in stmts:
            last_result = send_request("execute", sql=stmt)
            if last_result.get("ok"):
                all_results.append(last_result)
            else:
                return JSONResponse(last_result)
        result = last_result

    if result.get("ok"):
        return JSONResponse(result)
    raise HTTPException(400, result.get("error", "Query error"))


@app.get("/api/health")
def health() -> JSONResponse:
    """健康檢查"""
    return JSONResponse({"status": "ok", "db": DB_PATH or "memory"})


@app.get("/static/{file_path:path}")
def static_file(file_path: str) -> Response:
    """靜態檔案"""
    base_dir = os.path.dirname(os.path.abspath(__file__))
    file_path = os.path.join(base_dir, "static", file_path)
    if os.path.exists(file_path):
        with open(file_path, "r") as f:
            content = f.read()
        if file_path.endswith(".js"):
            return Response(content, media_type="application/javascript")
        elif file_path.endswith(".css"):
            return Response(content, media_type="text/css")
        return Response(content)
    raise HTTPException(404, "File not found")


def run_server():
    """啟動 FastAPI server"""
    import uvicorn
    import argparse
    global DB_PATH
    
    parser = argparse.ArgumentParser(description="sql6 Web Server")
    parser.add_argument("--port", type=int, default=8080, help="Port (default: 8080)")
    parser.add_argument("--binary", help="Path to sql6 binary")
    
    args = parser.parse_args()
    
    if args.binary:
        os.environ["SQL6_BINARY"] = args.binary
    
    start_server(None)
    DB_PATH = None
    
    print(f"Starting sql6 Web Admin at http://127.0.0.1:{args.port}")
    print("Press Ctrl+C to stop")
    
    try:
        uvicorn.run(app, host="127.0.0.1", port=args.port, log_level="info")
    finally:
        close_server()


if __name__ == "__main__":
    run_server()