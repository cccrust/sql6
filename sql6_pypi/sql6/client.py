# sql6 DB-API 2.0 客戶端實作
#
# 支援兩種傳輸模式：
# 1. subprocess（預設）：啟動 Rust 程序的 stdin/stdout 作為通訊管道
# 2. websocket：連線到 Rust WebSocket 伺服器
#
# JSON 通訊格式：
#   請求：{"method": "execute", "sql": "...", "params": [...]}
#   回應：{"ok": true, "columns": [...], "rows": [...], "affected": N}
#        {"ok": false, "error": "..."}

import os
import sys
import json
import subprocess
import tempfile
from typing import Optional, List, Any, Union

# ============================================================================
# 例外類別
# ============================================================================

class Error(Exception):
    """sql6 操作期間發生的錯誤"""
    pass

# ============================================================================
# Cursor：查詢結果指標
# ============================================================================

class Cursor:
    """
    查詢結果指標。

    使用方式：
        cursor = conn.execute("SELECT * FROM users")
        row = cursor.fetchone()    # 取一列
        rows = cursor.fetchall()   # 取全部
        for row in cursor:         # 直接迭代
            print(row)
    """

    def __init__(self, data: dict):
        self.ok = data.get("ok", False)
        self.error = data.get("error")
        self.columns = data.get("columns", [])
        self.rows = data.get("rows", [])
        self.affected = data.get("affected", 0)
        self.lastrowid = data.get("lastrowid")

        if self.columns:
            self.description = tuple((name, None, None, None, None, None, None) for name in self.columns)
            self.rowcount = -1
        else:
            self.description = None
            self.rowcount = self.affected if self.affected > 0 else -1

    def fetchone(self):
        """取回下一列，若無更多則返回 None"""
        if self.rows:
            return self.rows[0]
        return None

    def fetchall(self):
        """取回所有剩餘列"""
        return self.rows

    def __iter__(self):
        return iter(self.rows)

    @property
    def arraysize(self):
        """DB-API 2.0: 返回 fetchmany 的預設大小"""
        return 1

# ============================================================================
# Connection（subprocess 模式）
# ============================================================================

class Connection:
    """
    資料庫連線（subprocess 模式）。

    啟動 Rust 程序的 stdin/stdout 作為伺服器，
    透過 JSON 進行程序間通訊。

    使用方式：
        with sql6.connect("mydb.db") as conn:
            cursor = conn.execute("SELECT * FROM users")
            print(cursor.fetchall())
    """

    def __init__(self, path: Optional[str] = None):
        self.path = path
        self._process = None
        self._start_server()

    def _start_server(self):
        """啟動 Rust server 子程序"""
        binary_path = self._find_binary()
        args = [binary_path, "--server"]
        if self.path and self.path != ":memory:":
            args.append(self.path)

        # 啟動子程序，設定文字模式緩衝
        self._process = subprocess.Popen(
            args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )

        # 讀取 server 就緒信號
        line = self._process.stdout.readline()
        if not line:
            stderr = self._process.stderr.read()
            raise Error(f"伺服器啟動失敗：{stderr}")

        resp = json.loads(line)
        if not resp.get("ok"):
            raise Error(f"伺服器初始化錯誤：{resp}")

    def _find_binary(self) -> str:
        """尋找 sql6 二進位檔"""
        if "SQL6_BINARY" in os.environ:
            return os.environ["SQL6_BINARY"]
        import sql6._binary as _binary
        return _binary.get_binary_path()

    def execute(self, sql: str, params: tuple = ()) -> Cursor:
        """
        執行 SQL 語句並返回指標。

        參數使用 ? 作為佔位符，自動替換以防止 SQL 注入：
            cursor = conn.execute("SELECT * FROM users WHERE id = ?", (1,))
        """
        sql_with_params = self._substitute_params(sql, params)
        request = {"method": "execute", "sql": sql_with_params}
        return self._send_request(request)

    def _send_request(self, request: dict) -> Cursor:
        """發送請求到 server 並返回結果"""
        json_bytes = (json.dumps(request) + "\n").encode('utf-8')
        self._process.stdin.buffer.write(json_bytes)
        self._process.stdin.buffer.flush()

        line_bytes = self._process.stdout.buffer.readline()
        if not line_bytes:
            stderr = self._process.stderr.read()
            raise Error(f"伺服器錯誤：{stderr}")

        data = json.loads(line_bytes.decode('utf-8'))
        return Cursor(data)

    def tables(self) -> Cursor:
        """取得所有表格名稱"""
        return self._send_request({"method": "tables"})

    def schema(self, table: str = "") -> Cursor:
        """取得表格結構"""
        return self._send_request({"method": "schema", "table": table})

    def _substitute_params(self, sql: str, params: tuple) -> str:
        """
        將 ? 佔位符替換為實際參數值。

        會自動處理：
        - 整數：直接轉換
        - 字串：單引號包圍，單引號跳脫
        - None：轉換為 NULL
        - 浮點數：直接轉換
        """
        if not params:
            return sql
        result = sql
        for p in params:
            if isinstance(p, int):
                replacement = str(p)
            elif isinstance(p, str):
                replacement = "'" + p.replace("'", "''") + "'"
            elif p is None:
                replacement = "NULL"
            elif isinstance(p, float):
                replacement = str(p)
            else:
                replacement = "'" + str(p).replace("'", "''") + "'"
            result = result.replace("?", replacement, 1)
        return result

    def close(self):
        """關閉連線並終止子程序"""
        if self._process:
            try:
                request = {"method": "close"}
                self._process.stdin.write(json.dumps(request) + "\n")
                self._process.stdin.flush()
                self._process.terminate()
                self._process.wait(timeout=5)
            except:
                self._process.kill()
            self._process = None

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.close()
        return False

# ============================================================================
# WsConnection（WebSocket 模式）
# ============================================================================

class WsConnection:
    """
    資料庫連線（WebSocket 模式）。

    連線到 Rust WebSocket 伺服器，支援多客戶端並發。
    適用於需要多個 Python 程序共享同一資料庫的場景。

    使用方式：
        with sql6.connect("mydb.db", transport="websocket", port=8080) as conn:
            cursor = conn.execute("SELECT * FROM users")
            print(cursor.fetchall())
    """

    def __init__(self, path: Optional[str] = None, host: str = "127.0.0.1", port: int = 8080):
        self.path = path
        self.host = host
        self.port = port
        self._process = None
        self._ws = None

        # 檢查 websocket-client 是否已安裝
        try:
            import websocket
        except ImportError:
            raise Error("WebSocket 模式需要 websocket-client：pip install websocket-client")

        self._start_server()
        self._connect_websocket()

    def _start_server(self):
        """啟動 Rust WebSocket 伺服器"""
        binary_path = self._find_binary()
        args = [binary_path, "--websocket", str(self.port)]
        if self.path:
            args.append(self.path)

        self._process = subprocess.Popen(
            args,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

    def _find_binary(self) -> str:
        """尋找 sql6 二進位檔"""
        if "SQL6_BINARY" in os.environ:
            return os.environ["SQL6_BINARY"]
        import sql6._binary as _binary
        return _binary.get_binary_path()

    def _connect_websocket(self):
        """連線到 WebSocket 伺服器"""
        import websocket
        url = f"ws://{self.host}:{self.port}"
        self._ws = websocket.create_connection(url, timeout=10)

        resp = self._ws.recv()
        data = json.loads(resp)
        if not data.get("ok"):
            raise Error(f"伺服器初始化錯誤：{data}")

    def execute(self, sql: str, params: tuple = ()) -> Cursor:
        """執行 SQL 語句並返回指標"""
        sql_with_params = self._substitute_params(sql, params)
        request = {"method": "execute", "sql": sql_with_params}
        self._ws.send(json.dumps(request))

        resp = self._ws.recv()
        data = json.loads(resp)
        return Cursor(data)

    def _substitute_params(self, sql: str, params: tuple) -> str:
        """將 ? 佔位符替換為實際參數值"""
        if not params:
            return sql
        result = sql
        for p in params:
            if isinstance(p, int):
                replacement = str(p)
            elif isinstance(p, str):
                replacement = "'" + p.replace("'", "''") + "'"
            elif p is None:
                replacement = "NULL"
            elif isinstance(p, float):
                replacement = str(p)
            else:
                replacement = "'" + str(p).replace("'", "''") + "'"
            result = result.replace("?", replacement, 1)
        return result

    def close(self):
        """關閉連線並終止伺服器"""
        if self._ws:
            try:
                self._ws.send(json.dumps({"method": "close"}))
                self._ws.close()
            except:
                pass
            self._ws = None
        if self._process:
            try:
                self._process.terminate()
                self._process.wait(timeout=5)
            except:
                self._process.kill()
            self._process = None

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.close()
        return False

# ============================================================================
# 連線工廠函數
# ============================================================================

def connect(
    path: Optional[str] = None,
    transport: str = "subprocess",
    host: str = "127.0.0.1",
    port: int = 8080
) -> Union[Connection, WsConnection]:
    """
    建立資料庫連線。

    參數：
        path：資料庫檔案路徑（記憶體模式為 None）
        transport：傳輸模式，"subprocess"（預設）或 "websocket"
        host：WebSocket 模式的主機位址
        port：WebSocket 模式的連接埠

    返回：
        Connection 或 WsConnection 物件
    """
    if transport == "websocket":
        return WsConnection(path=path, host=host, port=port)
    return Connection(path)