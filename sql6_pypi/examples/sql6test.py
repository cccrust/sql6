import sql6

# In-memory database
db = sql6.connect()

# Or open a file
db = sql6.connect("mydb.db")

# Or open a file
# db = sql6.connect("mydb.db")

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