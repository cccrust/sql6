# sql6 連接池

import threading
import queue
import time
from typing import Optional
from .client import Connection

class PoolConfig:
    """連接池配置"""
    def __init__(
        self,
        max_connections: int = 10,
        min_idle: int = 2,
        connection_timeout: float = 30.0,
        idle_timeout: float = 300.0,
    ):
        self.max_connections = max_connections
        self.min_idle = min_idle
        self.connection_timeout = connection_timeout
        self.idle_timeout = idle_timeout

class PooledConnection:
    """池化的連線"""
    def __init__(self, conn, pool: 'Sql6Pool'):
        self.conn = conn
        self.pool = pool
        self.created_at = time.time()
        self.last_used = time.time()

    def execute(self, sql: str, params=()):
        self.last_used = time.time()
        return self.conn.execute(sql, params)

    def close(self):
        self.pool._return_connection(self.conn)

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.close()

class Sql6Pool:
    """sql6 連接池"""

    def __init__(self, path: str, config: Optional[PoolConfig] = None, **kwargs):
        self.path = path
        self.config = config or PoolConfig()
        self.transport = kwargs.get("transport", "subprocess")
        self.host = kwargs.get("host", "127.0.0.1")
        self.port = kwargs.get("port", 8080)

        self._lock = threading.Lock()
        self._pool = queue.Queue()
        self._created_count = 0

        # 預先建立最小空閒連線
        for _ in range(self.config.min_idle):
            conn = self._create_connection()
            self._pool.put(PooledConnection(conn, self))
            self._created_count += 1

    def _create_connection(self):
        """建立新連線"""
        if self.transport == "websocket":
            from .client import WebSocketConnection
            return WebSocketConnection(self.path, self.host, self.port)
        else:
            return Connection(self.path)

    def get(self, timeout: Optional[float] = None) -> PooledConnection:
        """取得連線"""
        timeout = timeout or self.config.connection_timeout

        try:
            pooled = self._pool.get(timeout=timeout)
            # 檢查是否逾時
            if time.time() - pooled.last_used > self.config.idle_timeout:
                pooled.conn.close()
                pooled = self._create_pooled_connection()
            return pooled
        except queue.Empty:
            with self._lock:
                if self._created_count < self.config.max_connections:
                    self._created_count += 1
                    return self._create_pooled_connection()
            raise TimeoutError("連接池逾時")

    def _create_pooled_connection(self) -> PooledConnection:
        conn = self._create_connection()
        return PooledConnection(conn, self)

    def _return_connection(self, conn):
        """歸還連線"""
        try:
            self._pool.put_nowait(PooledConnection(conn, self))
        except queue.Full:
            # 池已滿，關閉連線
            conn.close()
            with self._lock:
                self._created_count -= 1

    def close_all(self):
        """關閉所有連線"""
        while True:
            try:
                pooled = self._pool.get_nowait()
                pooled.conn.close()
            except queue.Empty:
                break
        with self._lock:
            self._created_count = 0

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.close_all()

# 別名
ConnectionPool = Sql6Pool