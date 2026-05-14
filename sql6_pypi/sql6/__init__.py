# sql6 Python 客戶端套件
#
# 提供與 DB-API 2.0 相容的介面，用於連接 sql6 資料庫。
#
# 使用範例：
#   import sql6
#   conn = sql6.connect("mydb.db")
#   cursor = conn.execute("SELECT * FROM users")
#   for row in cursor:
#       print(row)
#   conn.close()

__version__ = "6.9.5"
__all__ = [
    "connect", "Connection", "Cursor", "Error",
    "Sql6Pool", "PoolConfig", "QueryCache",
    "Cluster", "NodeInfo", "NodeRole", "ShardManager", "create_cluster",
    "Transaction", "IsolationLevel",
    "DatabaseMonitor", "backup", "restore", "verify",
    "RBAC", "User", "Role", "PermissionType",
    "Encryption", "AuditLogger",
    "create_rbac", "create_encryption", "create_audit_logger",
    "MaterializedView", "MaterializedViewManager", "BatchOperation", "QueryHint",
    "create_materialized_view", "create_batch", "execute_with_hints",
]

from .client import connect, Connection, Cursor, Error, QueryCache, Transaction, IsolationLevel
from .pool import Sql6Pool, PoolConfig
from .cluster import Cluster, NodeInfo, NodeRole, ShardManager, create_cluster
from .monitor import DatabaseMonitor
from .backup import BackupManager, backup, restore, verify
from .security import RBAC, User, Role, PermissionType, Encryption, AuditLogger
from .security import create_rbac, create_encryption, create_audit_logger
from .advanced import (
    MaterializedView, MaterializedViewManager, BatchOperation, QueryHint, AdvancedWindowFunctions,
    create_materialized_view, create_batch, execute_with_hints
)