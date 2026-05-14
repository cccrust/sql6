# sql6 叢集模組

import socket
import threading
import time
import json
from typing import Optional, List, Dict, Any
from enum import Enum
from .client import Connection

class NodeRole(Enum):
    """節點角色"""
    PRIMARY = "primary"
    REPLICA = "replica"
    CANDIDATE = "candidate"

class NodeStatus(Enum):
    """節點狀態"""
    HEALTHY = "healthy"
    UNHEALTHY = "unhealthy"
    RECOVERING = "recovering"

class NodeInfo:
    """節點資訊"""
    def __init__(
        self,
        node_id: str,
        host: str,
        port: int,
        role: NodeRole = NodeRole.PRIMARY,
    ):
        self.node_id = node_id
        self.host = host
        self.port = port
        self.role = role
        self.status = NodeStatus.HEALTHY
        self.last_heartbeat = time.time()
        self.latency_ms = 0

    def is_healthy(self, timeout: float = 5.0) -> bool:
        """健康檢查"""
        try:
            start = time.time()
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            sock.settimeout(timeout)
            sock.connect((self.host, self.port))
            sock.close()
            self.latency_ms = (time.time() - start) * 1000
            self.status = NodeStatus.HEALTHY
            self.last_heartbeat = time.time()
            return True
        except:
            self.status = NodeStatus.UNHEALTHY
            return False

    def to_dict(self) -> dict:
        return {
            "node_id": self.node_id,
            "host": self.host,
            "port": self.port,
            "role": self.role.value,
            "status": self.status.value,
            "latency_ms": self.latency_ms,
            "last_heartbeat": self.last_heartbeat,
        }

class NodeClient:
    """節點客戶端"""
    def __init__(self, node: NodeInfo):
        self.node = node
        self._conn: Optional[Connection] = None
        self._lock = threading.Lock()

    def connect(self):
        """連接到節點"""
        with self._lock:
            if self._conn is None:
                self._conn = Connection(self.node.host)
                self._conn.execute(f"SET node_id = '{self.node.node_id}'")

    def execute(self, sql: str, params=()) -> Any:
        """執行 SQL"""
        with self._lock:
            if self._conn is None:
                self.connect()
            return self._conn.execute(sql, params)

    def close(self):
        """關閉連線"""
        with self._lock:
            if self._conn:
                self._conn.close()
                self._conn = None

class HealthChecker:
    """健康檢查器"""
    def __init__(self, check_interval: float = 5.0):
        self.check_interval = check_interval
        self._running = False
        self._thread: Optional[threading.Thread] = None

    def start(self, nodes: List[NodeInfo]):
        """啟動健康檢查"""
        self._running = True
        self._thread = threading.Thread(target=self._check_loop, args=(nodes,))
        self._thread.daemon = True
        self._thread.start()

    def stop(self):
        """停止健康檢查"""
        self._running = False
        if self._thread:
            self._thread.join(timeout=1)

    def _check_loop(self, nodes: List[NodeInfo]):
        while self._running:
            for node in nodes:
                node.is_healthy()
            time.sleep(self.check_interval)

    def get_healthy_nodes(self) -> List[NodeInfo]:
        """取得健康節點"""
        return [n for n in self.nodes if n.status == NodeStatus.HEALTHY]

    nodes: List[NodeInfo] = []

class ShardInfo:
    """分片資訊"""
    def __init__(
        self,
        shard_id: str,
        key_range_start: Optional[Any],
        key_range_end: Optional[Any],
        primary_node: NodeInfo,
        replica_nodes: List[NodeInfo],
    ):
        self.shard_id = shard_id
        self.key_range_start = key_range_start
        self.key_range_end = key_range_end
        self.primary_node = primary_node
        self.replica_nodes = replica_nodes

    def contains_key(self, key: Any) -> bool:
        """檢查 key 是否在此分片範圍內"""
        if self.key_range_start is not None and key < self.key_range_start:
            return False
        if self.key_range_end is not None and key >= self.key_range_end:
            return False
        return True

class ShardManager:
    """分片管理器"""
    def __init__(self, shards: List[ShardInfo]):
        self.shards = shards

    def find_shard(self, key: Any) -> ShardInfo:
        """根據 key 找到分片"""
        for shard in self.shards:
            if shard.contains_key(key):
                return shard
        # 預設返回第一個分片
        return self.shards[0]

    def add_shard(self, shard: ShardInfo):
        """新增分片"""
        self.shards.append(shard)

class Cluster:
    """sql6 叢集"""

    def __init__(self, name: str):
        self.name = name
        self.nodes: List[NodeInfo] = []
        self.shard_manager: Optional[ShardManager] = None
        self.health_checker = HealthChecker()
        self._lock = threading.Lock()

    def add_node(self, node: NodeInfo):
        """新增節點"""
        with self._lock:
            self.nodes.append(node)

    def remove_node(self, node_id: str):
        """移除節點"""
        with self._lock:
            self.nodes = [n for n in self.nodes if n.node_id != node_id]

    def connect_all(self):
        """連接所有節點"""
        for node in self.nodes:
            client = NodeClient(node)
            client.connect()

    def status(self) -> dict:
        """取得叢集狀態"""
        return {
            "name": self.name,
            "node_count": len(self.nodes),
            "healthy_count": len([n for n in self.nodes if n.status == NodeStatus.HEALTHY]),
            "shard_count": len(self.shard_manager.shards) if self.shard_manager else 0,
            "nodes": [n.to_dict() for n in self.nodes],
        }

    def execute(self, sql: str, shard_key: Optional[Any] = None) -> Any:
        """執行 SQL（自動路由）"""
        if self.shard_manager and shard_key is not None:
            # 分散式執行
            shard = self.shard_manager.find_shard(shard_key)
            client = NodeClient(shard.primary_node)
            return client.execute(sql)
        else:
            # 單節點執行（使用第一個健康節點）
            healthy = [n for n in self.nodes if n.status == NodeStatus.HEALTHY]
            if not healthy:
                raise RuntimeError("No healthy nodes available")
            client = NodeClient(healthy[0])
            return client.execute(sql)

    def shutdown(self):
        """關閉叢集"""
        self.health_checker.stop()
        for node in self.nodes:
            client = NodeClient(node)
            client.close()

# 便捷函數
def create_cluster(name: str, nodes: List[tuple]) -> Cluster:
    """
    建立叢集

    參數:
        name: 叢集名稱
        nodes: [(node_id, host, port), ...]

    範例:
        cluster = create_cluster("my_cluster", [
            ("node1", "192.168.1.1", 8080),
            ("node2", "192.168.1.2", 8080),
            ("node3", "192.168.1.3", 8080),
        ])
    """
    cluster = Cluster(name)
    for node_id, host, port in nodes:
        node = NodeInfo(node_id, host, port)
        cluster.add_node(node)
    cluster.connect_all()
    return cluster