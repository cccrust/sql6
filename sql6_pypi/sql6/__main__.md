# CLI Entry Point - 命令列入口點理論

`sql5_pypi/sql5/__main__.py`

## 設計目的

讓 `python -m sql5` 可以直接執行 Rust 二進位檔：

```bash
$ python -m sql5 --version
sql5 version 3.1.3
```

## 原理

當執行 `python -m sql5` 時：
1. Python 直譯器載入 `sql5/__main__.py`
2. 執行 `main()` 函數
3. `main()` 取得 Rust 二進位檔路徑
4. 使用 `os.execv` 置換當前程序

## os.execv 的行為

```python
os.execv(binary, [binary] + sys.argv[1:])
```

- **替換程序映像**：Python 行程被 Rust 行程完全替換
- **保留參數**：原來的命令列參數傳給 Rust 程式
- **無返回**：此調用之後，Python 代碼不再執行

## 使用場景

### 1. 開發調試

```bash
# 使用本地的 Rust 編譯
$ SQL5_BINARY=./target/debug/sql5 python -m sql5
```

### 2. 無法使用 pip 安裝時

```bash
$ pip install sql5
$ python -m sql5 mydb.db
```

## 與直接執行二進位檔的比較

| 方式 | 說明 |
|------|------|
| `sql5 my.db` | 直接執行 Rust 二進位檔 |
| `python -m sql5 my.db` | Python 啟動，調用 Rust 二進位檔 |

兩者行為相同，後者提供 Python 環境的靈活性。

## 理論參考

- PEP 338: Executing Modules as Scripts
- Unix process replacement: `exec` family of functions