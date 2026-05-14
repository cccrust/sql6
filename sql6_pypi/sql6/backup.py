# sql6 備份與還原模組

import os
import gzip
import json
import time
from typing import Optional, Dict, Any
from .client import Connection

class BackupOptions:
    """備份選項"""
    def __init__(
        self,
        compression: str = "none",  # none, gzip
        include_schema: bool = True,
        include_data: bool = True,
    ):
        self.compression = compression
        self.include_schema = include_schema
        self.include_data = include_data

class BackupManager:
    """備份管理器"""

    def __init__(self, conn: Connection):
        self.conn = conn

    def backup(self, output_path: str, options: Optional[BackupOptions] = None):
        """執行備份"""
        options = options or BackupOptions()

        # 收集所有 schema
        schemas = []
        if options.include_schema:
            # Tables
            cursor = self.conn.execute(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
            )
            for row in (cursor.rows or []):
                if row and row[0]:
                    schemas.append(row[0])

            # Indexes
            cursor = self.conn.execute(
                "SELECT sql FROM sqlite_master WHERE type='index' AND sql IS NOT NULL"
            )
            for row in (cursor.rows or []):
                if row and row[0]:
                    schemas.append(row[0])

        # 收集所有資料
        tables_data = {}
        if options.include_data:
            # 取得所有表格
            cursor = self.conn.execute(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
            )
            table_names = [row[0] for row in (cursor.rows or [])] if cursor.rows else []

            for table_name in table_names:
                rows = self.conn.execute(f"SELECT * FROM {table_name}")
                tables_data[table_name] = rows.rows if rows.rows else []

        # 建立備份資料
        backup_data = {
            "version": "6.6.0",
            "created_at": time.time(),
            "schemas": schemas,
            "tables": tables_data,
        }

        # 寫入備份檔
        json_data = json.dumps(backup_data, ensure_ascii=False, default=str)

        if options.compression == "gzip":
            with gzip.open(output_path, "wt", encoding="utf-8") as f:
                f.write(json_data)
        else:
            with open(output_path, "w", encoding="utf-8") as f:
                f.write(json_data)

        return output_path

    def restore(self, backup_path: str, target_conn: Optional[Connection] = None):
        """還原備份"""
        target_conn = target_conn or self.conn

        # 讀取備份資料
        if backup_path.endswith(".gz"):
            with gzip.open(backup_path, "rt", encoding="utf-8") as f:
                json_data = f.read()
        else:
            with open(backup_path, "r", encoding="utf-8") as f:
                json_data = f.read()

        backup_data = json.loads(json_data)

        # 執行 schema
        for schema in backup_data.get("schemas", []):
            if schema:
                target_conn.execute(schema)

        # 插入資料
        for table_name, rows in backup_data.get("tables", {}).items():
            if rows and len(rows) > 0:
                columns = list(rows[0].keys()) if isinstance(rows[0], dict) else None
                if columns:
                    placeholders = ",".join(["?"] * len(columns))
                    insert_sql = f"INSERT INTO {table_name} ({','.join(columns)}) VALUES ({placeholders})"

                    for row in rows:
                        if isinstance(row, dict):
                            target_conn.execute(insert_sql, list(row.values()))

        return True

    def verify_backup(self, backup_path: str) -> Dict[str, Any]:
        """驗證備份檔"""
        try:
            if backup_path.endswith(".gz"):
                with gzip.open(backup_path, "rt", encoding="utf-8") as f:
                    json_data = f.read()
            else:
                with open(backup_path, "r", encoding="utf-8") as f:
                    json_data = f.read()

            backup_data = json.loads(json_data)

            return {
                "valid": True,
                "version": backup_data.get("version"),
                "created_at": backup_data.get("created_at"),
                "schema_count": len(backup_data.get("schemas", [])),
                "table_count": len(backup_data.get("tables", {})),
            }
        except Exception as e:
            return {
                "valid": False,
                "error": str(e),
            }

# 便捷函數
def backup(conn: Connection, output_path: str, **options) -> str:
    """快速備份"""
    manager = BackupManager(conn)
    return manager.backup(output_path, BackupOptions(**options))

def restore(conn: Connection, backup_path: str) -> bool:
    """快速還原"""
    manager = BackupManager(conn)
    return manager.restore(backup_path, conn)

def verify(backup_path: str) -> dict:
    """驗證備份"""
    # 只需驗證檔案格式
    try:
        if backup_path.endswith(".gz"):
            with gzip.open(backup_path, "rt", encoding="utf-8") as f:
                json_data = f.read()
        else:
            with open(backup_path, "r", encoding="utf-8") as f:
                json_data = f.read()
        
        backup_data = json.loads(json_data)
        return {
            "valid": True,
            "version": backup_data.get("version"),
            "tables": list(backup_data.get("tables", {}).keys()),
        }
    except Exception as e:
        return {"valid": False, "error": str(e)}