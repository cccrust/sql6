"""
sql6 與 sqlite3 API 比較測試

本測試模組驗證 sql6 Python 客戶端與原生 sqlite3 模組的
API 相容性，確保兩者返回相同的資料結構。

主要測試項目：
- cursor.description（欄位描述）
- cursor.columns（sql6 擴充屬性）
- rows 類型（tuple vs list）
- fetchone/fetchall 返回值
- rowcount 屬性
- lastrowid 屬性
- affected rows
"""

import os
import sys
import tempfile
import pytest

script_dir = os.path.dirname(os.path.abspath(__file__))
project_dir = os.path.dirname(script_dir)

def get_local_binary():
    paths_to_check = [
        os.path.join(project_dir, "target", "release", "sql6"),
        os.path.join(project_dir, "..", "target", "release", "sql6"),
        os.path.join(os.path.dirname(project_dir), "target", "release", "sql6"),
    ]
    for p in paths_to_check:
        if os.path.exists(p):
            return p
    return None

local_binary = get_local_binary()
if local_binary:
    os.environ["SQL6_BINARY"] = local_binary

sys.path.insert(0, project_dir)

import sqlite3
import sql6
from sql6 import connect, Error


def execute_sqlite(sql, params=()):
    """使用原生 sqlite3 執行 SQL"""
    conn = sqlite3.connect(':memory:')
    cursor = conn.cursor()
    if params:
        cursor.execute(sql, params)
    else:
        cursor.execute(sql)
    return cursor


def execute_sql6(sql, params=()):
    """使用 sql6 執行 SQL"""
    conn = sql6.connect()  # Use default (no path) instead of :memory:
    cursor = conn.execute(sql, params)
    return cursor


class TestCursorDescription:
    """測試 cursor.description 屬性"""

    def test_select_single_column_description(self):
        """sqlite3: (name, ...)"""
        sc = execute_sqlite("SELECT id FROM (SELECT 1 AS id)")
        sql6_c = execute_sql6("SELECT id FROM (SELECT 1 AS id)")

        print(f"sqlite3 description: {sc.description}")
        print(f"sql6 columns: {sql6_c.columns}")

        assert sc.description is not None
        assert len(sc.description) == 1
        assert sc.description[0][0] == "id"

    def test_select_multiple_columns_description(self):
        """多欄位的 description"""
        sc = execute_sqlite("SELECT 1 AS a, 2 AS b, 3 AS c")
        sql6_c = execute_sql6("SELECT 1 AS a, 2 AS b, 3 AS c")

        print(f"sqlite3 description: {sc.description}")
        print(f"sql6 columns: {sql6_c.columns}")

        assert sc.description is not None
        assert len(sc.description) == 3
        assert sc.description[0][0] == "a"
        assert sc.description[1][0] == "b"
        assert sc.description[2][0] == "c"

    def test_description_after_insert(self):
        """INSERT 之後的 description（應該為空）"""
        sqlite_conn = sqlite3.connect(':memory:')
        sqlite_conn.execute("CREATE TABLE t (id INTEGER, name TEXT)")
        sc = sqlite_conn.execute("INSERT INTO t VALUES (1, 'alice')")

        sql6_conn = sql6.connect()
        sql6_conn.execute("CREATE TABLE t (id INTEGER, name TEXT)")
        sql6_c = sql6_conn.execute("INSERT INTO t VALUES (1, 'alice')")

        print(f"sqlite3 INSERT description: {sc.description}")
        print(f"sql5 INSERT columns: {sql6_c.columns}")

        assert sc.description is None or len(sc.description) == 0


class TestRowTypes:
    """測試 rows 的類型（tuple vs list）"""

    def test_select_returns_list(self):
        """sql5 應該返回 list，sqlite3 返回 tuple"""
        sc = execute_sqlite("SELECT 1 AS id, 'alice' AS name")
        sql6_c = execute_sql6("SELECT 1 AS id, 'alice' AS name")

        sqlite_row = sc.fetchone()
        sql6_row = sql6_c.fetchone()

        print(f"sqlite3 row type: {type(sqlite_row)}, value: {sqlite_row}")
        print(f"sql5 row type: {type(sql6_row)}, value: {sql6_row}")

        assert isinstance(sqlite_row, tuple)
        assert isinstance(sql6_row, list)

    def test_fetchall_returns_similar_structure(self):
        """fetchall 返回的結構應該相似"""
        sqlite_conn = sqlite3.connect(':memory:')
        sqlite_conn.execute("CREATE TABLE t (id, name)")
        sqlite_conn.execute("INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c')")
        sc = sqlite_conn.execute("SELECT * FROM t")

        sql6_conn = sql6.connect()
        sql6_conn.execute("CREATE TABLE t (id INTEGER, name TEXT)")
        sql6_conn.execute("INSERT INTO t VALUES (1, 'a')")
        sql6_conn.execute("INSERT INTO t VALUES (2, 'b')")
        sql6_conn.execute("INSERT INTO t VALUES (3, 'c')")
        sql6_c = sql6_conn.execute("SELECT * FROM t")

        sqlite_rows = sc.fetchall()
        sql6_rows = sql6_c.fetchall()

        print(f"sqlite3 rows: {sqlite_rows}")
        print(f"sql5 rows: {sql6_rows}")

        assert len(sqlite_rows) == len(sql6_rows)
        assert sqlite_rows[0][0] == sql6_rows[0][0]
        assert sqlite_rows[1][0] == sql6_rows[1][0]


class TestRowCount:
    """測試 cursor.rowcount 屬性"""

    def test_select_rowcount(self):
        """SELECT 的 rowcount"""
        sc = execute_sqlite("SELECT 1 UNION SELECT 2")
        sql6_c = execute_sql6("SELECT 1 UNION SELECT 2")

        print(f"sqlite3 rowcount after SELECT: {sc.rowcount}")
        print(f"sql5 rows count: {len(sql6_c.rows)}")

    def test_insert_rowcount(self):
        """INSERT 的 rowcount"""
        sqlite_conn = sqlite3.connect(':memory:')
        sqlite_conn.execute("CREATE TABLE t (id INTEGER)")
        sqlite_conn.execute("INSERT INTO t VALUES (1)")
        sqlite_conn.execute("INSERT INTO t VALUES (2)")
        sc = sqlite_conn.execute("INSERT INTO t VALUES (3)")

        sql6_conn = sql6.connect()
        sql6_conn.execute("CREATE TABLE t (id INTEGER)")
        sql6_conn.execute("INSERT INTO t VALUES (1)")
        sql6_conn.execute("INSERT INTO t VALUES (2)")
        sql6_c = sql6_conn.execute("INSERT INTO t VALUES (3)")

        print(f"sqlite3 rowcount after INSERT: {sc.rowcount}")
        print(f"sql5 affected: {sql6_c.affected}")

        assert sc.rowcount == 1
        assert sql6_c.affected == 1


class TestLastRowId:
    """測試 cursor.lastrowid 屬性"""

    def test_insert_lastrowid(self):
        """INSERT 的 lastrowid"""
        sqlite_conn = sqlite3.connect(':memory:')
        sqlite_conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
        sc = sqlite_conn.execute("INSERT INTO t VALUES (NULL, 'alice')")

        sql6_conn = sql6.connect()
        sql6_conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
        sql6_conn.execute("INSERT INTO t VALUES (NULL, 'alice')")
        sql6_c = sql6_conn.execute("SELECT last_insert_rowid()")

        print(f"sqlite3 lastrowid: {sc.lastrowid}")
        print(f"sql5 last_insert_rowid: {sql6_c.fetchone()}")

    def test_insert_with_autoincrement(self):
        """AUTOINCREMENT 的 lastrowid"""
        sqlite_conn = sqlite3.connect(':memory:')
        sqlite_conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)")
        sc = sqlite_conn.execute("INSERT INTO t VALUES (NULL, 'alice')")

        sql6_conn = sql6.connect()
        sql6_conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)")
        sql6_c = sql6_conn.execute("INSERT INTO t VALUES (NULL, 'alice')")

        print(f"sqlite3 lastrowid: {sc.lastrowid}")
        print(f"sql5 affected: {sql6_c.affected}")


class TestAffectedRows:
    """測試受影響的行數"""

    def test_update_affected_count(self):
        """UPDATE 受影響的行數"""
        sqlite_conn = sqlite3.connect(':memory:')
        sqlite_conn.execute("CREATE TABLE t (id INTEGER, val TEXT)")
        sqlite_conn.execute("INSERT INTO t VALUES (1, 'a')")
        sqlite_conn.execute("INSERT INTO t VALUES (2, 'b')")
        sqlite_conn.execute("INSERT INTO t VALUES (3, 'c')")
        sc = sqlite_conn.execute("UPDATE t SET val = 'x' WHERE id > 1")

        sql6_conn = sql6.connect()
        sql6_conn.execute("CREATE TABLE t (id INTEGER, val TEXT)")
        sql6_conn.execute("INSERT INTO t VALUES (1, 'a')")
        sql6_conn.execute("INSERT INTO t VALUES (2, 'b')")
        sql6_conn.execute("INSERT INTO t VALUES (3, 'c')")
        sql6_c = sql6_conn.execute("UPDATE t SET val = 'x' WHERE id > 1")

        print(f"sqlite3 rowcount after UPDATE: {sc.rowcount}")
        print(f"sql5 affected: {sql6_c.affected}")

        assert sc.rowcount == 2
        assert sql6_c.affected == 2

    def test_delete_affected_count(self):
        """DELETE 受影響的行數"""
        sqlite_conn = sqlite3.connect(':memory:')
        sqlite_conn.execute("CREATE TABLE t (id INTEGER)")
        sqlite_conn.execute("INSERT INTO t VALUES (1)")
        sqlite_conn.execute("INSERT INTO t VALUES (2)")
        sqlite_conn.execute("INSERT INTO t VALUES (3)")
        sqlite_conn.execute("INSERT INTO t VALUES (4)")
        sqlite_conn.execute("INSERT INTO t VALUES (5)")
        sc = sqlite_conn.execute("DELETE FROM t WHERE id > 3")

        sql6_conn = sql6.connect()
        sql6_conn.execute("CREATE TABLE t (id INTEGER)")
        sql6_conn.execute("INSERT INTO t VALUES (1)")
        sql6_conn.execute("INSERT INTO t VALUES (2)")
        sql6_conn.execute("INSERT INTO t VALUES (3)")
        sql6_conn.execute("INSERT INTO t VALUES (4)")
        sql6_conn.execute("INSERT INTO t VALUES (5)")
        sql6_c = sql6_conn.execute("DELETE FROM t WHERE id > 3")

        print(f"sqlite3 rowcount after DELETE: {sc.rowcount}")
        print(f"sql5 affected: {sql6_c.affected}")

        assert sc.rowcount == 2
        assert sql6_c.affected == 2


class TestNullHandling:
    """測試 NULL 值處理"""

    def test_null_in_select(self):
        """SELECT 中的 NULL"""
        sc = execute_sqlite("SELECT NULL AS val")
        sql6_c = execute_sql6("SELECT NULL AS val")

        sqlite_row = sc.fetchone()
        sql6_row = sql6_c.fetchone()

        print(f"sqlite3 NULL row: {sqlite_row}, type: {type(sqlite_row[0])}")
        print(f"sql5 NULL row: {sql6_row}, type: {type(sql6_row[0])}")

        assert sqlite_row[0] is None
        assert sql6_row[0] is None

    def test_null_in_columns(self):
        """NULL 值在各類型欄位"""
        sqlite_conn = sqlite3.connect(':memory:')
        sqlite_conn.execute("CREATE TABLE t (i INTEGER, t TEXT, f REAL)")
        sqlite_conn.execute("INSERT INTO t VALUES (NULL, NULL, NULL)")
        sc = sqlite_conn.execute("SELECT * FROM t")

        sql6_conn = sql6.connect()
        sql6_conn.execute("CREATE TABLE t (i INTEGER, t TEXT, f FLOAT)")
        sql6_conn.execute("INSERT INTO t VALUES (NULL, NULL, NULL)")
        sql6_c = sql6_conn.execute("SELECT * FROM t")

        sqlite_row = sc.fetchone()
        sql6_row = sql6_c.fetchone()

        print(f"sqlite3 NULLs: {sqlite_row}")
        print(f"sql5 NULLs: {sql6_row}")


class TestDataTypes:
    """測試不同資料類型"""

    def test_integer_type(self):
        """INTEGER 類型"""
        sc = execute_sqlite("SELECT 42 AS num")
        sql6_c = execute_sql6("SELECT 42 AS num")

        sqlite_row = sc.fetchone()
        sql6_row = sql6_c.fetchone()

        print(f"sqlite3 INTEGER: {sqlite_row[0]}, type: {type(sqlite_row[0])}")
        print(f"sql5 INTEGER: {sql6_row[0]}, type: {type(sql6_row[0])}")

        assert isinstance(sqlite_row[0], int)
        assert isinstance(sql6_row[0], int)
        assert sqlite_row[0] == sql6_row[0]

    def test_text_type(self):
        """TEXT 類型"""
        sc = execute_sqlite("SELECT 'hello' AS msg")
        sql6_c = execute_sql6("SELECT 'hello' AS msg")

        sqlite_row = sc.fetchone()
        sql6_row = sql6_c.fetchone()

        print(f"sqlite3 TEXT: {sqlite_row[0]}, type: {type(sqlite_row[0])}")
        print(f"sql5 TEXT: {sql6_row[0]}, type: {type(sql6_row[0])}")

        assert isinstance(sqlite_row[0], str)
        assert isinstance(sql6_row[0], str)
        assert sqlite_row[0] == sql6_row[0]

    def test_real_type(self):
        """REAL/FLOAT 類型"""
        sc = execute_sqlite("SELECT 3.14 AS pi")
        sql6_c = execute_sql6("SELECT 3.14 AS pi")

        sqlite_row = sc.fetchone()
        sql6_row = sql6_c.fetchone()

        print(f"sqlite3 REAL: {sqlite_row[0]}, type: {type(sqlite_row[0])}")
        print(f"sql5 FLOAT: {sql6_row[0]}, type: {type(sql6_row[0])}")

        assert isinstance(sqlite_row[0], float)
        assert isinstance(sql6_row[0], float)
        assert abs(sqlite_row[0] - sql6_row[0]) < 0.0001


class TestEmptyResults:
    """測試空結果集"""

    def test_no_rows_returned(self):
        """無資料時返回空陣列"""
        sqlite_conn = sqlite3.connect(':memory:')
        sqlite_conn.execute("CREATE TABLE t (id INTEGER)")
        sc = sqlite_conn.execute("SELECT * FROM t WHERE 1=0")

        sql6_conn = sql6.connect()
        sql6_conn.execute("CREATE TABLE t (id INTEGER)")
        sql6_c = sql6_conn.execute("SELECT * FROM t WHERE 1=0")

        sqlite_rows = sc.fetchall()
        sql6_rows = sql6_c.fetchall()

        print(f"sqlite3 empty rows: {sqlite_rows}")
        print(f"sql5 empty rows: {sql6_rows}")

        assert sqlite_rows == []
        assert sql6_rows == []


class TestCursorAttributes:
    """測試 Cursor 屬性"""

    def test_sqlite3_cursor_has_standard_attrs(self):
        """sqlite3 Cursor 標準屬性"""
        sc = execute_sqlite("SELECT 1 AS id, 'alice' AS name")

        print(f"sqlite3 cursor attributes:")
        print(f"  description: {sc.description}")
        print(f"  rowcount: {sc.rowcount}")

        assert hasattr(sc, 'description')
        assert hasattr(sc, 'rowcount')
        assert hasattr(sc, 'lastrowid')
        assert hasattr(sc, 'arraysize')

    def test_sql6_cursor_columns(self):
        """sql5 Cursor 有 columns 屬性"""
        sql6_c = execute_sql6("SELECT 1 AS id, 'alice' AS name")

        print(f"sql5 cursor attributes:")
        print(f"  columns: {sql6_c.columns}")
        print(f"  rows: {sql6_c.rows}")
        print(f"  affected: {sql6_c.affected}")

        assert hasattr(sql6_c, 'columns')
        assert hasattr(sql6_c, 'rows')
        assert hasattr(sql6_c, 'affected')
        assert sql6_c.columns == ["id", "name"]


class TestIterateCursor:
    """測試 cursor 迭代"""

    def test_for_loop_iteration(self):
        """for 迴圈迭代"""
        sqlite_conn = sqlite3.connect(':memory:')
        sqlite_conn.execute("CREATE TABLE t (id INTEGER)")
        for i in range(1, 4):
            sqlite_conn.execute(f"INSERT INTO t VALUES ({i})")
        sc = sqlite_conn.execute("SELECT * FROM t")

        sql6_conn = sql6.connect()
        sql6_conn.execute("CREATE TABLE t (id INTEGER)")
        for i in range(1, 4):
            sql6_conn.execute(f"INSERT INTO t VALUES ({i})")
        sql6_c = sql6_conn.execute("SELECT * FROM t")

        sqlite_items = list(sc)
        sql6_items = list(sql6_c)

        print(f"sqlite3 iteration: {sqlite_items}")
        print(f"sql5 iteration: {sql6_items}")

        assert len(sqlite_items) == len(sql6_items)


class TestFTS5Format:
    """測試 FTS5 格式與 sqlite3 一致"""

    def test_fts5_basic_query(self):
        """FTS5 基本查詢結構與 sqlite 相同"""
        sqlite_conn = sqlite3.connect(':memory:')
        sqlite_conn.execute('CREATE VIRTUAL TABLE docs USING fts5(content)')
        sqlite_conn.execute("INSERT INTO docs VALUES ('Python is great')")
        sqlite_conn.execute("INSERT INTO docs VALUES ('Rust is fast')")
        sc = sqlite_conn.execute("SELECT * FROM docs WHERE docs MATCH 'Python'")

        sql6_conn = sql6.connect()
        sql6_conn.execute("CREATE VIRTUAL TABLE docs USING fts5(content)")
        sql6_conn.execute("INSERT INTO docs VALUES ('Python is great')")
        sql6_conn.execute("INSERT INTO docs VALUES ('Rust is fast')")
        sql6_c = sql6_conn.execute("SELECT * FROM docs WHERE docs MATCH 'Python'")

        print(f"sqlite3 FTS5 columns: {sc.description}")
        print(f"sql5 FTS5 columns: {sql6_c.columns}")

        # 欄位名稱應該相同
        sqlite_cols = [d[0] for d in sc.description] if sc.description else []
        assert sqlite_cols == sql6_c.columns

        # rows 結構應該相同（都是 list of lists/tuples）
        sqlite_rows = sc.fetchall()
        assert len(sqlite_rows) == len(sql6_c.rows)

    def test_fts5_with_rowid(self):
        """FTS5 明確查詢 rowid 時的結構（SELECT * 不包含 rowid）"""
        sqlite_conn = sqlite3.connect(':memory:')
        sqlite_conn.execute('CREATE VIRTUAL TABLE articles USING fts5(title, body)')
        sqlite_conn.execute("INSERT INTO articles VALUES ('Rust Guide', 'Memory safety')")
        # sqlite: SELECT * 不包含 rowid
        sc = sqlite_conn.execute("SELECT * FROM articles WHERE articles MATCH 'rust'")

        sql6_conn = sql6.connect()
        sql6_conn.execute("CREATE VIRTUAL TABLE articles USING fts5(title, body)")
        sql6_conn.execute("INSERT INTO articles VALUES ('Rust Guide', 'Memory safety')")
        sql6_c = sql6_conn.execute("SELECT * FROM articles WHERE articles MATCH 'rust'")

        print(f"sqlite3 FTS5 SELECT * columns: {[d[0] for d in sc.description]}")
        print(f"sql5 FTS5 SELECT * columns: {sql6_c.columns}")

        # 欄位名稱應該相同（SELECT * 都不包含 rowid）
        sqlite_cols = [d[0] for d in sc.description] if sc.description else []
        assert sqlite_cols == sql6_c.columns

        # rows 結構應該相同
        sqlite_rows = sc.fetchall()
        assert len(sqlite_rows) == len(sql6_c.rows)


if __name__ == "__main__":
    pytest.main([__file__, "-v", "-s"])