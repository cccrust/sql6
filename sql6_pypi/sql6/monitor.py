# sql6 效能監控模組

import time
import threading
import json
from typing import Dict, List, Any, Optional, Callable
from collections import deque
from datetime import datetime, timedelta
from .client import Connection

class QueryRecord:
    """查詢記錄"""
    def __init__(self, sql: str, duration_ms: float, timestamp: float):
        self.sql = sql
        self.duration_ms = duration_ms
        self.timestamp = timestamp

    def to_dict(self) -> dict:
        return {
            "sql": self.sql[:100],  # 截斷過長的 SQL
            "duration_ms": self.duration_ms,
            "timestamp": datetime.fromtimestamp(self.timestamp).isoformat(),
        }

class PerformanceMetrics:
    """效能指標"""
    def __init__(self):
        self.queries_count = 0
        self.queries_total_time_ms = 0.0
        self.cache_hits = 0
        self.cache_misses = 0
        self.memory_usage_bytes = 0
        self.disk_usage_bytes = 0
        self.active_connections = 0
        self.start_time = time.time()

    @property
    def queries_per_second(self) -> float:
        elapsed = time.time() - self.start_time
        return self.queries_count / max(elapsed, 1)

    @property
    def avg_query_time_ms(self) -> float:
        return self.queries_total_time_ms / max(self.queries_count, 1)

    @property
    def cache_hit_rate(self) -> float:
        total = self.cache_hits + self.cache_misses
        return self.cache_hits / max(total, 1)

    def to_dict(self) -> dict:
        return {
            "queries_count": self.queries_count,
            "queries_per_second": round(self.queries_per_second, 2),
            "avg_query_time_ms": round(self.avg_query_time_ms, 2),
            "cache_hit_rate": round(self.cache_hit_rate * 100, 2),
            "memory_usage_mb": round(self.memory_usage_bytes / 1024 / 1024, 2),
            "active_connections": self.active_connections,
            "uptime_seconds": round(time.time() - self.start_time, 0),
        }

class DatabaseMonitor:
    """資料庫監控器"""

    def __init__(self, path: str = ":memory:", enable_logging: bool = False):
        self.path = path
        self._conn: Optional[Connection] = None
        self._metrics = PerformanceMetrics()
        self._slow_queries: deque = deque(maxlen=1000)
        self._query_history: deque = deque(maxlen=10000)
        self._logging_enabled = enable_logging
        self._lock = threading.Lock()
        self._running = False
        self._monitor_thread: Optional[threading.Thread] = None
        self._alerts: List[Dict[str, Any]] = []

    def connect(self):
        """連接到資料庫"""
        if self._conn is None:
            self._conn = Connection(self.path)

    def start_monitoring(self):
        """啟動監控"""
        self.connect()
        self._running = True
        self._monitor_thread = threading.Thread(target=self._monitor_loop)
        self._monitor_thread.daemon = True
        self._monitor_thread.start()

    def stop_monitoring(self):
        """停止監控"""
        self._running = False
        if self._monitor_thread:
            self._monitor_thread.join(timeout=1)

    def _monitor_loop(self):
        """監控迴圈"""
        while self._running:
            try:
                # 更新連線數
                # （需要 Rust 端支援）
                self._metrics.active_connections = 1
                time.sleep(1)
            except:
                pass

    def execute(self, sql: str, params=()) -> Any:
        """執行 SQL 並記錄效能"""
        start = time.time()
        try:
            result = self._conn.execute(sql, params)
            duration_ms = (time.time() - start) * 1000

            # 記錄查詢
            record = QueryRecord(sql, duration_ms, time.time())
            with self._lock:
                self._query_history.append(record)
                self._metrics.queries_count += 1
                self._metrics.queries_total_time_ms += duration_ms

                # 慢查詢記錄
                if duration_ms > 1000:  # 超過 1 秒
                    self._slow_queries.append(record)

                # 檢查告警
                self._check_alerts(sql, duration_ms)

            return result
        except Exception as e:
            duration_ms = (time.time() - start) * 1000
            record = QueryRecord(f"ERROR: {sql}", duration_ms, time.time())
            with self._lock:
                self._query_history.append(record)
            raise e

    def _check_alerts(self, sql: str, duration_ms: float):
        """檢查告警條件"""
        for alert in self._alerts:
            if alert["type"] == "slow_query" and duration_ms > alert["threshold"]:
                if "callback" in alert:
                    alert["callback"]({
                        "sql": sql,
                        "duration_ms": duration_ms,
                        "timestamp": time.time(),
                    })

    def get_current_metrics(self) -> dict:
        """取得目前效能指標"""
        with self._lock:
            return self._metrics.to_dict()

    def get_slow_queries(self, limit: int = 10) -> List[dict]:
        """取得慢查詢"""
        with self._lock:
            return [q.to_dict() for q in list(self._slow_queries)[-limit:]]

    def get_query_history(self, limit: int = 100) -> List[dict]:
        """取得查詢歷史"""
        with self._lock:
            return [q.to_dict() for q in list(self._query_history)[-limit:]]

    def get_trend(self, metric: str, last: str = "1h") -> List[dict]:
        """取得趨勢資料（需要長時間收集）"""
        # 簡化實現
        return [
            {"timestamp": datetime.now().isoformat(), "value": self.get_current_metrics().get(metric, 0)}
        ]

    def set_alert(self, alert_type: str, threshold: Any = None, callback: Optional[Callable] = None):
        """設定告警"""
        self._alerts.append({
            "type": alert_type,
            "threshold": threshold,
            "callback": callback,
        })

    def generate_report(self) -> Dict[str, Any]:
        """生成效能報告"""
        metrics = self.get_current_metrics()
        slow_queries = self.get_slow_queries(20)

        return {
            "generated_at": datetime.now().isoformat(),
            "summary": metrics,
            "slow_queries_count": len(slow_queries),
            "slow_queries": slow_queries[:10],
            "recommendations": self._generate_recommendations(metrics, slow_queries),
        }

    def _generate_recommendations(self, metrics: dict, slow_queries: List[dict]) -> List[str]:
        """生成優化建議"""
        recommendations = []

        if metrics.get("avg_query_time_ms", 0) > 100:
            recommendations.append("Average query time is high - consider adding indexes")

        if metrics.get("cache_hit_rate", 0) < 50:
            recommendations.append("Low cache hit rate - consider increasing cache size")

        if len(slow_queries) > 10:
            recommendations.append("Many slow queries detected - review and optimize")

        if not recommendations:
            recommendations.append("Performance looks good!")

        return recommendations

    def close(self):
        """關閉監控"""
        self.stop_monitoring()
        if self._conn:
            self._conn.close()
            self._conn = None

# 便捷函數：建立監控連線
def monitor(path: str = ":memory:", enable_logging: bool = False) -> DatabaseMonitor:
    """建立監控連線"""
    monitor = DatabaseMonitor(path, enable_logging)
    monitor.start_monitoring()
    return monitor