# sql6 二進位檔下載管理
#
# 負責自動下載對應平台的 Rust 編譯版本。
#
# 運作流程：
# 1. 偵測目前平台（macOS/Linux/Windows, arm64/x86_64）
# 2. 檢查本地快取（~/.cache/sql6/）
# 3. 若無或版本過舊，從 GitHub Releases 下載
# 4. 自動設定執行權限

import os
import sys
import platform
import stat
import urllib.request
import json

# GitHub 發布資訊
OWNER = "cccrust"
REPO = "sql6"

# 各平台的二進位檔名稱
BINARY_NAMES = {
    "macos-arm64": "sql6-macos-arm64",
    "macos-x86_64": "sql6-macos-x86_64",
    "linux-x86_64": "sql6-linux-x86_64",
    "windows-x86_64": "sql6-windows.exe",
}

# URL 對應（與 BINARY_NAMES 相同）
BINARY_URL_NAMES = {
    "macos-arm64": "sql6-macos-arm64",
    "macos-x86_64": "sql6-macos-x86_64",
    "linux-x86_64": "sql6-linux-x86_64",
    "windows-x86_64": "sql6-windows.exe",
}

# 校驗和（目前設為 None，正式發布時應設定）
CHECKSUMS = {
    "macos-arm64": None,
    "macos-x86_64": None,
    "linux-x86_64": None,
    "windows-x86_64": None,
}

# ============================================================================
# 平台偵測
# ============================================================================

def get_platform():
    """
    偵測目前平台的識別名稱。

    返回：
        "macos-arm64"、"macos-x86_64"、"linux-x86_64" 或 "windows-x86_64"
    """
    system = platform.system().lower()
    machine = platform.machine().lower()

    if system == "darwin" and machine in ("arm64", "aarch64"):
        return "macos-arm64"
    elif system == "darwin":
        return "macos-x86_64"
    elif system == "linux":
        return "linux-x86_64"
    elif system == "windows":
        return "windows-x86_64"
    else:
        raise RuntimeError(f"不支援的平台：{system}/{machine}")

def get_cache_dir():
    """
    取得二進位檔快取目錄。

    - macOS/Linux：~/.cache/sql6/
    - Windows：%LOCALAPPDATA%\sql6\cache\
    """
    if sys.platform == "darwin" or sys.platform == "linux":
        base = os.path.expanduser("~/.cache/sql6")
    else:
        base = os.path.join(os.environ.get("LOCALAPPDATA", ""), "sql6", "cache")
    os.makedirs(base, exist_ok=True)
    return base

# ============================================================================
# GitHub API
# ============================================================================

def get_releases():
    """從 GitHub API 取得所有發布版本"""
    url = f"https://api.github.com/repos/{OWNER}/{REPO}/releases"
    try:
        with urllib.request.urlopen(url, timeout=10) as response:
            return json.loads(response.read())
    except Exception as e:
        raise RuntimeError(
            f"無法從 GitHub 取得發布列表：{e}\n"
            "請檢查網路連線或手動設定 SQL6_BINARY 環境變數。"
        )

def find_release_with_binary():
    """
    找到第一個包含目標平台二進位檔的發布版本。

    返回：(版本號, 下載 URL)
    """
    releases = get_releases()
    platform_name = get_platform()
    binary_name = BINARY_NAMES[platform_name]

    for release in releases:
        tag = release.get("tag_name", "")
        if tag.startswith("v"):
            version = tag[1:]
        else:
            version = tag

        # 檢查此發布是否包含目標平台的二進位檔
        assets = release.get("assets", [])
        for asset in assets:
            if asset.get("name") == binary_name:
                return version, asset.get("browser_download_url")

    return None, None

# ============================================================================
# 下載與快取管理
# ============================================================================

def get_binary_path():
    """
    取得 sql6 二進位檔的路徑。

    若快取中沒有或版本過舊，會自動下載。

    返回：二進位檔的完整路徑
    """
    cache_dir = get_cache_dir()
    platform_name = get_platform()
    binary_name = BINARY_NAMES[platform_name]
    binary_path = os.path.join(cache_dir, binary_name)

    version, url = find_release_with_binary()
    if not version or not url:
        raise RuntimeError(
            f"找不到平台 {platform_name} 的發布版本。\n"
            "請確認 GitHub 上有包含二進位檔的發布，"
            "或手動設定 SQL6_BINARY 環境變數。"
        )

    # 檢查快取的版本是否正確
    version_file = os.path.join(cache_dir, f"{binary_name}.version")
    cached_version = None
    if os.path.exists(version_file):
        cached_version = open(version_file).read().strip()

    if os.path.exists(binary_path) and cached_version == version:
        return binary_path

    # 下載新版本
    _download_binary(platform_name, binary_name, binary_path, version, url)
    return binary_path

def _download_binary(platform_name, binary_name, destination, version, url):
    """
    下載二進位檔並儲存到目的地。

    參數：
        platform_name：平台名稱
        binary_name：二進位檔名稱
        destination：儲存路徑
        version：版本號
        url：下載 URL
    """
    print(f"正在下載 sql6 v{version}（{platform_name}）...", file=sys.stderr)

    try:
        urllib.request.urlretrieve(url, destination)
    except Exception as e:
        raise RuntimeError(
            f"下載失敗：{url}\n"
            f"請確認發布 v{version} 存在。\n"
            f"錯誤：{e}"
        )

    # 儲存版本資訊
    cache_dir = get_cache_dir()
    version_file = os.path.join(cache_dir, f"{binary_name}.version")
    with open(version_file, "w") as f:
        f.write(version)

    # 設定執行權限（Unix 系統）
    os.chmod(destination, os.stat(destination).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    print(f"已下載至 {destination}", file=sys.stderr)