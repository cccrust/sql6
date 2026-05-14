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

__version__ = "6.5.0"
__all__ = [
    "connect", "Connection", "Cursor", "Error",
    "Sql6Pool", "PoolConfig", "QueryCache",
    "Cluster", "NodeInfo", "NodeRole", "ShardManager", "create_cluster",
]

from .client import connect, Connection, Cursor, Error, QueryCache
from .pool import Sql6Pool, PoolConfig
from .cluster import Cluster, NodeInfo, NodeRole, ShardManager, create_cluster