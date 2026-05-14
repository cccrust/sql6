# Python Client - 客戶端函式庫理論

`sql5_pypi/sql5/__init__.py`

## 套件概述

這是 sql5 的 Python 客戶端，提供與 DB-API 2.0 相容的介面。

## 設計理念

### DB-API 2.0 標準

Python 資料庫 API 規範（PEP 249）定義：

```python
import sql5

conn = sql5.connect("mydb.db")
cursor = conn.execute("SELECT * FROM users")
for row in cursor:
    print(row)
conn.close()
```

### 透明化

使用者無需理會：
- 底層傳輸機制（subprocess 或 WebSocket）
- 與 Rust server 的通訊協定
- 程序的啟動/管理

## 導出的公開 API

| 符號 | 說明 |
|------|------|
| `connect` | 建立連線工廠函數 |
| `Connection` | 連線物件（subprocess 模式） |
| `WsConnection` | 連線物件（WebSocket 模式） |
| `Cursor` | 資料指標 |
| `Error` | 例外類別 |

## 理論參考

- PEP 249: Python Database API Specification v2.0
- DB-API 2.0 實現模式