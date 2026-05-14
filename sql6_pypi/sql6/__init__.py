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

__version__ = "4.0.2"
__all__ = ["connect", "Connection", "Cursor", "Error"]

from .client import connect, Connection, Cursor, Error