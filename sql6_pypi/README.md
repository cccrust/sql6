# sql5

SQLite-compatible database with native CJK FTS5 support. Built with Rust.

## v3.0 - Client-Server Architecture with WebSocket Support

sql5 v3.0 consists of:
- **Python package** (`sql5` on PyPI): Pure Python client
- **Rust binary**: Server process providing all SQL functionality

The Python client communicates with the Rust server via:
- **Subprocess mode** (default): JSON over stdin/stdout
- **WebSocket mode** (v3.0): WebSocket protocol for multi-client support

## Installation

```bash
pip install sql5
```

## Python API

```python
import sql5

# Subprocess mode (default, v2.0 compatible)
db = sql5.connect("mydb.db")

# Or use WebSocket mode (v3.0, multi-client support)
db = sql5.connect(
    path="mydb.db",
    transport="websocket",
    host="127.0.0.1",
    port=8080
)

# Execute SQL
db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")
db.execute("INSERT INTO users VALUES (1, 'Alice', 30)")
db.execute("INSERT INTO users VALUES (2, 'Bob', 25)")
db.execute("INSERT INTO users VALUES (3, 'Charlie', 35)")

# Query with parameters
db.execute("INSERT INTO users VALUES (?, ?, ?)", (4, "David", 28))

# Fetch results
cursor = db.execute("SELECT * FROM users WHERE age > ?", (25,))
for row in cursor:
    print(row)
# (1, 'Alice', 30)
# (2, 'Bob', 25)
# (3, 'Charlie', 35)
# (4, 'David', 28)

# Fetch as list
cursor = db.execute("SELECT name, age FROM users ORDER BY age")
rows = cursor.fetchall()
print(rows)
# [('Bob', 25), ('David', 28), ('Alice', 30), ('Charlie', 35)]

# Fetch one
cursor = db.execute("SELECT * FROM users WHERE id = ?", (1,))
row = cursor.fetchone()
print(row)
# (1, 'Alice', 30)

# Transactions
db.execute("BEGIN")
db.execute("INSERT INTO users VALUES (5, 'Eve', 40)")
db.execute("COMMIT")

# Or rollback
db.execute("BEGIN")
db.execute("INSERT INTO users VALUES (6, 'Frank', 45)")
db.execute("ROLLBACK")

# Full-text search (FTS5)
db.execute("CREATE VIRTUAL TABLE articles USING fts5(title, body)")
db.execute("INSERT INTO articles VALUES ('Hello World', 'The quick brown fox')")
db.execute("INSERT INTO articles VALUES ('Rust Guide', 'Memory safety without GC')")
db.execute("INSERT INTO articles VALUES ('中文測試', '繁體中文全文檢索')")

cursor = db.execute("SELECT * FROM articles WHERE articles MATCH ?", ("rust",))
print(cursor.fetchall())
# [('Rust Guide', 'Memory safety without GC')]

cursor = db.execute("SELECT * FROM articles WHERE articles MATCH ?", ("中文",))
print(cursor.fetchall())
# [('中文測試', '繁體中文全文檢索')]

# Close database
db.close()
```

### Connection Parameters

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `path` | str | None | Database file path |
| `transport` | str | "subprocess" | "subprocess" or "websocket" |
| `host` | str | "127.0.0.1" | WebSocket server host |
| `port` | int | 8080 | WebSocket server port |

## CLI Usage

```bash
# Run the REPL
sql5

# Open a database file
sql5 /path/to/database.db

# Execute single query
echo "SELECT 1 + 1;" | sql5
```

## Features

- Full SQL support (SELECT, INSERT, UPDATE, DELETE, CREATE, DROP)
- ACID transactions (BEGIN, COMMIT, ROLLBACK)
- WAL mode
- Foreign keys
- Views
- Triggers
- Full-text search (FTS5) with CJK bigram tokenization
- Multiple database attachment (ATTACH DATABASE)
- Window functions (ROW_NUMBER, RANK, LAG, LEAD, etc.)
- String functions (UPPER, LOWER, SUBSTR, REPLACE, etc.)
- Date/time functions (DATE, TIME, DATETIME, STRFTIME)
- JSON functions (JSON, JSON_EXTRACT, JSON_SET, etc.)
- WebSocket server mode (v3.0, multi-client support)
- Subprocess server mode (v2.0, backward compatible)

## Requirements

- Python 3.8+
- For WebSocket mode: `pip install websocket-client` (auto-installed as dependency)

## Development

To use a local Rust binary instead of downloading from GitHub:

```bash
export SQL5_BINARY=/path/to/local/sql5
python -c "import sql5; print(sql5.__version__)"
```

## License

MIT