# Binary Downloader - 二進位檔案下載理論

`sql5_pypi/sql5/_binary.py`

## 設計動機

SQLite 的 Python 包通常只包含 pure Python 程式碼，但 sql5：
- 核心使用 Rust 編寫
- 需跨平台分發已編譯的二進位檔

因此需要一個動態下載二進位檔的機制。

## 發布模式比較

| 模式 | 優點 | 缺點 |
|------|------|------|
| PyPI wheels | 官方支援 | 需為每平台建構上傳 |
| GitHub Releases | 靈活 | 需手動管理 |
| 本專案 | 簡單，與 CI 整合 | 需網路下載 |

## 快取策略

```
首次使用：
  檢查快取 → 沒有 → 下載並快取 → 使用

後續使用：
  檢查快取 → 有且版本正確 → 直接使用
                      ↓ 版本過期
                重新下載
```

### 快取目錄

| 作業系統 | 路徑 |
|----------|------|
| macOS/Linux | `~/.cache/sql5/` |
| Windows | `%LOCALAPPDATA%\sql5\cache\` |

## 平台偵測

```python
import platform

platform.system()  # 'Darwin', 'Linux', 'Windows'
platform.machine() # 'arm64', 'x86_64'
```

### 支援的平台

| 平台 | 二進位檔名 |
|------|-----------|
| macOS ARM64 | `sql5-macos-arm64` |
| macOS x86_64 | `sql5-macos-x86_64` |
| Linux x86_64 | `sql5-linux-x86_64` |
| Windows x86_64 | `sql5-windows.exe` |

## 版本管理

每個二進位檔有對應的版本檔：

```bash
~/.cache/sql5/
├── sql5-macos-arm64      # 二進位檔
└── sql5-macos-arm64.version  # 版本號
```

## 安全性考量

### 為何不校驗 Checksum？

當前版本 `CHECKSUMS = {...}` 設為 `None`：
- 發布嚴格管控（私人 repo）
- 簡化下載流程

生產環境建議：
- 啟用 SHA256 校驗
- 使用 HTTPS 下載
- 驗證 GPG 簽名

## GitHub API 使用

```python
url = f"https://api.github.com/repos/{OWNER}/{REPO}/releases"
```

遍歷 releases 尋找包含目標平台二進位檔的發布。

## 理論參考

- Semantic Versioning: https://semver.org
- Python packaging: https://packaging.python.org/
- 跨平台二進位分發策略