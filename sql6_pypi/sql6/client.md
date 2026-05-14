# DB-API Client - 資料庫客戶端理論

`sql5_pypi/sql5/client.py`

## 架構設計

本客戶端支援兩種傳輸模式：

```
                ┌─────────────┐
                │   Python    │
                │   Client    │
                └──────┬──────┘
                       │
            ┌──────────┼──────────┐
            │                     │
    transport=subprocess   transport=websocket
            │                     │
      ┌─────▼─────┐         ┌─────▼─────┐
      │  subprocess │        │  WebSocket │
      │   Popen     │        │   Client   │
      └─────┬─────┘         └─────┬─────┘
            │                     │
      ┌─────▼─────┐         ┌─────▼─────┐
      │  JSON RPC  │         │  JSON RPC  │
      │  (stdio)   │         │ (ws://)    │
      └─────┬─────┘         └─────┬─────┘
            │                     │
      ┌─────▼────────────────────▼─────┐
      │        Rust sql5 Server        │
      └────────────────────────────────┘
```

## DB-API 2.0 實現

### Connection 物件

管理資料庫連線的生命週期：

```python
with sql5.connect("mydb.db") as conn:
    cursor = conn.execute("SELECT * FROM users")
```

生命週期：
1. `__init__` - 啟動 Rust server 子程序
2. `execute` - 傳送 SQL，接收結果
3. `close` - 終止子程序

### Cursor 物件

代表查詢結果的指標：

```python
cursor.execute("SELECT * FROM users")
row = cursor.fetchone()      # 取一列
rows = cursor.fetchall()     # 取全部
```

## Subprocess 模式

### 程序間通訊 (IPC)

使用標準輸入/輸出進行程序間通訊：

```
Python                  Rust
  │                       │
  │── json request ──────>│
  │                       │
  │<─── json response ─────│
  │                       │
```

### 優點

| 特性 | 說明 |
|------|------|
| 簡單 | 不需網路設定 |
| 低延遲 | 同一機器，記憶體傳輸 |
| 隔離 | server 崩潰不影響 client |

### 缺點

- 只能單一客戶端
- 程序啟動有開銷

## WebSocket 模式

### 為何需要 WebSocket？

當有多個 Python 程序需要同時存取同一資料庫時：
- Subprocess 模式：每個程序啟動獨立的 server
- WebSocket 模式：共用一個 server

### 連線流程

```python
conn = sql5.connect(
    path="mydb.db",
    transport="websocket",
    host="127.0.0.1",
    port=8080
)
```

1. 啟動 Rust WebSocket server
2. 建立 WebSocket 連線
3. 交換 JSON 訊息

## 參數替換

防止 SQL 注入攻擊：

```python
# 不安全（請勿這樣做）
cursor.execute(f"SELECT * FROM users WHERE id = {user_id}")

# 安全方式
cursor.execute("SELECT * FROM users WHERE id = ?", (user_id,))
```

本客戶端實現簡單的 `?` 參數替換：
- `?` 替換為實際值
- 字串值自動轉義單引號

## 理論參考

- PEP 249: Python Database API Specification v2.0
- RFC 6455: The WebSocket Protocol
- 防止 SQL 注入：OWASP SQL Injection