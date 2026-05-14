# sql6 進階功能模組

import time
from typing import List, Dict, Any, Optional
from collections import defaultdict
from .client import Connection

class MaterializedView:
    """物化視圖"""
    
    def __init__(self, name: str, query: str, conn: Connection):
        self.name = name
        self.query = query
        self.conn = conn
        self._table_name = f"_mv_{name}"
        self.created_at = time.time()
        self.last_refresh: Optional[float] = None

    def refresh(self):
        """重新整理視圖"""
        self.conn.execute(f"DROP TABLE IF EXISTS {self._table_name}")
        
        # 先取得查詢結果
        cursor = self.conn.execute(self.query)
        rows = cursor.rows or []
        
        if not rows:
            # 建立空表
            self.conn.execute(f"CREATE TABLE {self._table_name} (id INTEGER)")
            self.last_refresh = time.time()
            return
        
        # 建立表結構（根據第一行推斷）
        columns = cursor.columns or [f"col{i}" for i in range(len(rows[0]))]
        col_defs = ", ".join([f"{col} TEXT" for col in columns])
        self.conn.execute(f"CREATE TABLE {self._table_name} ({col_defs})")
        
        # 逐行插入（sql6 不支援 INSERT SELECT）
        placeholders = ",".join(["?"] * len(columns))
        insert_sql = f"INSERT INTO {self._table_name} VALUES ({placeholders})"
        for row in rows:
            self.conn.execute(insert_sql, list(row))
        
        self.last_refresh = time.time()

    def drop(self):
        """刪除視圖"""
        self.conn.execute(f"DROP TABLE IF EXISTS {self._table_name}")

    def __iter__(self):
        """如同普通表迭代"""
        cursor = self.conn.execute(f"SELECT * FROM {self._table_name}")
        return iter(cursor.rows)

    def __len__(self):
        cursor = self.conn.execute(f"SELECT COUNT(*) FROM {self._table_name}")
        return cursor.rows[0][0] if cursor.rows else 0


class MaterializedViewManager:
    """物化視圖管理器"""

    def __init__(self, conn: Connection):
        self.conn = conn
        self.views: Dict[str, MaterializedView] = {}
        self._load_existing_views()

    def _load_existing_views(self):
        """載入現有視圖"""
        cursor = self.conn.execute("""
            SELECT name FROM sqlite_master
            WHERE type='table' AND name LIKE '_mv_%'
        """)
        for row in (cursor.rows or []):
            view_name = row[0][4:]  # 移除 _mv_ 前綴
            self.views[view_name] = MaterializedView(view_name, "", self.conn)

    def create(self, name: str, query: str) -> MaterializedView:
        """建立物化視圖"""
        view = MaterializedView(name, query, self.conn)
        view.refresh()
        self.views[name] = view
        return view

    def refresh(self, name: str):
        """重新整理視圖"""
        if name in self.views:
            self.views[name].refresh()

    def refresh_all(self):
        """重新整理所有視圖"""
        for view in self.views.values():
            view.refresh()

    def drop(self, name: str):
        """刪除視圖"""
        if name in self.views:
            self.views[name].drop()
            del self.views[name]

    def list_views(self) -> List[str]:
        """列出所有物化視圖"""
        return list(self.views.keys())

    def get(self, name: str) -> Optional[MaterializedView]:
        """取得視圖"""
        return self.views.get(name)


class BatchOperation:
    """批次操作"""

    def __init__(self, conn: Connection, batch_size: int = 100):
        self.conn = conn
        self.batch_size = batch_size
        self._inserts: List[tuple] = []
        self._updates: List[dict] = []
        self._deletes: List[str] = []

    def insert(self, table: str, rows: List[dict]):
        """批次插入"""
        self._inserts.append((table, rows))

    def update(self, table: str, values: dict, where: str):
        """批次更新"""
        self._updates.append((table, values, where))

    def delete(self, table: str, where: str):
        """批次刪除"""
        self._deletes.append((table, where))

    def execute(self) -> Dict[str, int]:
        """執行所有批次操作"""
        results = {"inserted": 0, "updated": 0, "deleted": 0}

        # 執行插入
        for table, rows in self._inserts:
            for batch in [rows[i:i+self.batch_size] for i in range(0, len(rows), self.batch_size)]:
                if batch:
                    columns = list(batch[0].keys())
                    placeholders = ",".join(["?"] * len(columns))
                    sql = f"INSERT INTO {table} ({','.join(columns)}) VALUES ({placeholders})"
                    for row in batch:
                        self.conn.execute(sql, list(row.values()))
                    results["inserted"] += len(batch)

        # 執行更新
        for table, values, where in self._updates:
            set_clause = ",".join([f"{k} = ?" for k in values.keys()])
            sql = f"UPDATE {table} SET {set_clause} WHERE {where}"
            self.conn.execute(sql, list(values.values()))
            results["updated"] += 1

        # 執行刪除
        for table, where in self._deletes:
            sql = f"DELETE FROM {table} WHERE {where}"
            self.conn.execute(sql)
            results["deleted"] += 1

        # 清空批次
        self._inserts.clear()
        self._updates.clear()
        self._deletes.clear()

        return results

    def clear(self):
        """清空所有待執行的操作"""
        self._inserts.clear()
        self._updates.clear()
        self._deletes.clear()


class QueryHint:
    """查詢提示"""

    INDEX = "INDEX"
    NO_INDEX = "NO_INDEX"
    USE_LIMIT = "USE_LIMIT"
    ORDER_BY = "ORDER_BY"

    @staticmethod
    def apply(sql: str, hints: List[str]) -> str:
        """應用查詢提示"""
        # 簡單實現：將提示作為註解加入
        hint_comment = f"/*+ {', '.join(hints)} */ "
        return hint_comment + sql


class AdvancedWindowFunctions:
    """進階視窗函數（客戶端計算）"""

    @staticmethod
    def moving_average(data: List[dict], field: str, window: int = 3) -> List[dict]:
        """計算移動平均"""
        result = []
        for i, row in enumerate(data):
            start = max(0, i - window + 1)
            values = [float(r[field]) for r in data[start:i+1] if field in r]
            row[f"{field}_ma{window}"] = sum(values) / len(values) if values else None
            result.append(row)
        return result

    @staticmethod
    def first_value(data: List[dict], field: str, partition_by: str = None) -> List[dict]:
        """計算分區首值"""
        if not partition_by:
            first_val = data[0].get(field) if data else None
            for row in data:
                row[f"first_{field}"] = first_val
        else:
            # 分區計算
            groups = defaultdict(list)
            for row in data:
                groups[row.get(partition_by)].append(row)
            for rows in groups.values():
                first_val = rows[0].get(field) if rows else None
                for row in rows:
                    row[f"first_{field}"] = first_val
        return data

    @staticmethod
    def ntile(data: List[dict], field: str, buckets: int = 4) -> List[dict]:
        """NTILE 分桶"""
        if not data:
            return data
        
        # 排序並分桶
        sorted_data = sorted(data, key=lambda x: x.get(field, 0))
        bucket_size = len(sorted_data) / buckets
        
        for i, row in enumerate(sorted_data):
            row[f"ntile_{buckets}"] = min(int(i / bucket_size) + 1, buckets)
        
        return sorted_data


class RecursiveCTE:
    """遞迴 CTE（客戶端模擬）"""

    @staticmethod
    def execute_recursive(
        conn: Connection,
        initial_sql: str,
        recursive_sql: str,
        max_depth: int = 100
    ) -> List[dict]:
        """執行遞迴 CTE"""
        results = []
        seen = set()
        
        # 初始查詢
        cursor = conn.execute(initial_sql)
        rows = cursor.rows or []
        
        for row in rows:
            results.append(list(row))
            seen.add(tuple(row))
        
        # 遞迴查詢
        for depth in range(max_depth):
            found_new = False
            
            cursor = conn.execute(recursive_sql)
            new_rows = cursor.rows or []
            
            for row in new_rows:
                row_tuple = tuple(row)
                if row_tuple not in seen:
                    results.append(list(row))
                    seen.add(row_tuple)
                    found_new = True
            
            if not found_new:
                break
        
        return results


# 便捷函數
def create_materialized_view(conn: Connection, name: str, query: str) -> MaterializedView:
    """建立物化視圖"""
    manager = MaterializedViewManager(conn)
    return manager.create(name, query)

def create_batch(conn: Connection, batch_size: int = 100) -> BatchOperation:
    """建立批次操作"""
    return BatchOperation(conn, batch_size)

def execute_with_hints(conn: Connection, sql: str, hints: List[str]) -> Any:
    """使用提示執行查詢"""
    optimized_sql = QueryHint.apply(sql, hints)
    return conn.execute(optimized_sql)