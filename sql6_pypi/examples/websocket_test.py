import sql6
import subprocess
import time
import os
import signal
import sys

BINARY = os.environ.get("SQL6_BINARY", "../../target/release/sql6")
PORT = 18080

proc = subprocess.Popen(
    [BINARY, "--websocket", str(PORT), "ws_test.db"],
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
)

time.sleep(1)

try:
    db = sql6.connect(
        path="ws_test.db",
        transport="websocket",
        host="127.0.0.1",
        port=PORT
    )

    db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")
    db.execute("INSERT INTO users VALUES (1, 'Alice', 30)")
    db.execute("INSERT INTO users VALUES (2, 'Bob', 25)")
    db.execute("INSERT INTO users VALUES (3, 'Charlie', 35)")

    # Test parameterized insert
    db.execute("INSERT INTO users VALUES (?, ?, ?)", (4, "David", 28))

    # Verify all 4 users
    cursor = db.execute("SELECT * FROM users ORDER BY id")
    all_users = cursor.fetchall()
    print("All users:", all_users)
    assert len(all_users) == 4, f"Expected 4 users, got {len(all_users)}"

    cursor = db.execute("SELECT * FROM users WHERE age > ?", (25,))
    rows = cursor.fetchall()
    print("Users with age > 25:", rows)
    assert len(rows) == 3, f"Expected 3 rows (Alice, Charlie, David), got {len(rows)}"

    cursor = db.execute("SELECT name, age FROM users ORDER BY age")
    rows = cursor.fetchall()
    print(rows)
    assert rows[0] == ['Bob', 25], f"First row should be Bob, got {rows[0]}"

    db.execute("CREATE VIRTUAL TABLE articles USING fts5(title, body)")
    db.execute("INSERT INTO articles VALUES ('Rust Guide', 'Memory safety without GC')")
    db.execute("INSERT INTO articles VALUES ('中文測試', '繁體中文全文檢索')")

    cursor = db.execute("SELECT * FROM articles WHERE articles MATCH ?", ("rust",))
    rows = cursor.fetchall()
    print(rows)
    assert len(rows) == 1, f"Expected 1 row, got {len(rows)}"

    cursor = db.execute("SELECT * FROM articles WHERE articles MATCH ?", ("中文",))
    rows = cursor.fetchall()
    print(rows)
    assert len(rows) == 1, f"Expected 1 row, got {len(rows)}"

    db.close()
    print("WebSocket test PASSED")

finally:
    proc.terminate()
    proc.wait()
    try:
        os.remove("ws_test.db")
    except:
        pass
    try:
        os.remove("ws_test.db-wal")
    except:
        pass
    try:
        os.remove("ws_test.db-shm")
    except:
        pass