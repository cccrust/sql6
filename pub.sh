#!/bin/bash
# pub.sh — sql6 發布腳本
#
# 用法:
#   ./pub.sh <version> pypi     上傳到 PyPI（會更新版本號）
#   ./pub.sh <version> github   建立 GitHub tag（會更新版本號並觸發 CI）
#   ./pub.sh                    顯示幫助

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
RESET='\033[0m'

PROJECT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$PROJECT_DIR"

function version_compare() {
    local v1=$1
    local v2=$2

    local v1_major=$(echo $v1 | cut -d. -f1)
    local v1_minor=$(echo $v1 | cut -d. -f2)
    local v1_patch=$(echo $v1 | cut -d. -f3)
    v1_patch=${v1_patch:-0}

    local v2_major=$(echo $v2 | cut -d. -f1)
    local v2_minor=$(echo $v2 | cut -d. -f2)
    local v2_patch=$(echo $v2 | cut -d. -f3)
    v2_patch=${v2_patch:-0}

    if [ "$v1_major" -gt "$v2_major" ]; then return 0; fi
    if [ "$v1_major" -lt "$v2_major" ]; then return 1; fi
    if [ "$v1_minor" -gt "$v2_minor" ]; then return 0; fi
    if [ "$v1_minor" -lt "$v2_minor" ]; then return 1; fi
    if [ "$v1_patch" -gt "$v2_patch" ]; then return 0; fi
    return 1
}

function get_current_version() {
    grep '^version = ' Cargo.toml | sed 's/version = "\(.*\)"/\1/'
}

function update_version() {
    local NEW_VERSION=$1

    echo "更新版本號為 $NEW_VERSION..."

    # Update Cargo.toml
    sed -i.bak "s/^version = \".*\"/version = \"$NEW_VERSION\"/" Cargo.toml
    rm -f Cargo.toml.bak

    # Update pyproject.toml
    sed -i.bak "s/^version = \".*\"/version = \"$NEW_VERSION\"/" sql6_pypi/pyproject.toml
    rm -f sql6_pypi/pyproject.toml.bak

    # Update __init__.py
    sed -i.bak "s/__version__ = \".*\"/__version__ = \"$NEW_VERSION\"/" sql6_pypi/sql6/__init__.py
    rm -f sql6_pypi/sql6/__init__.py.bak

    echo -e "${GREEN}版本已更新${RESET}"
}

function do_crates() {
    local VERSION=$1

    echo -e "${GREEN}=== 上傳到 crates.io (v$VERSION) ===${RESET}"
    echo ""

    echo "清理系統檔案..."
    find . -name ".DS_Store" -delete

    echo "建立 package..."
    cargo package --allow-dirty

    echo "上傳到 crates.io..."
    cargo publish --allow-dirty

    echo ""
    echo -e "${GREEN}完成！已上傳 sql6-$VERSION 到 crates.io${RESET}"
    echo "安裝: cargo install sql6"

    cd "$PROJECT_DIR"
}

function do_pypi() {
    local VERSION=$1

    echo -e "${GREEN}=== 上傳到 PyPI (v$VERSION) ===${RESET}"
    echo ""

    cd "$PROJECT_DIR/sql6_pypi"

    echo "清理舊 build..."
    rm -rf dist build *.egg-info

    echo "Build Python package..."
    cd "$PROJECT_DIR/sql6_pypi"
    uv pip install build
    uv run python -m build

    echo "上傳到 PyPI..."
    if [[ -n "${PYPI_TOKEN:-}" ]]; then
        twine upload dist/* --user __token__ --password "$PYPI_TOKEN"
    else
        twine upload dist/*
    fi

    echo ""
    echo -e "${GREEN}完成！已上傳 sql6-$VERSION 到 PyPI${RESET}"
    echo "安裝: pip install sql6==$VERSION"

    cd "$PROJECT_DIR"
}

function do_github() {
    local VERSION=$1
    local TAG="v$VERSION"

    echo -e "${GREEN}=== 建立 GitHub Release (v$VERSION) ===${RESET}"
    echo ""

    # Update version first
    update_version "$VERSION"

    # Auto-stage all tracked files (respects .gitignore)
    echo "Staging files..."
    git add -A

    # Check if there are changes to commit
    if git diff --cached --quiet; then
        echo -e "${YELLOW}沒有需要提交的更改${RESET}"
    else
        echo "有文件已更新"
    fi

    echo "將會:"
    echo "  1. 建立 tag: $TAG"
    echo "  2. 推送到 GitHub"
    echo "  3. GitHub Actions 會 build 4 平台 (macOS arm64/x86_64, Linux, Windows)"
    echo "     並上傳到 GitHub Release"
    echo ""
    echo "注意: GitHub Actions 不會自動上傳到 PyPI"
    echo "      如需上傳 PyPI，請另外執行: ./pub.sh $VERSION pypi"
    echo ""

    read -p "確認發布? (y/N) " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        echo "已取消"
        exit 1
    fi

    # Stage and commit version changes
    echo "提交版本更新..."
    git add Cargo.toml sql6_pypi/pyproject.toml sql6_pypi/sql6/__init__.py
    git commit -m "Bump version to $VERSION"

    # Create and push tag
    echo "建立 tag..."
    git tag "$TAG"

    echo "推送到 GitHub..."
    git push origin main "$TAG"

    echo ""
    echo -e "${GREEN}完成！已推送到 GitHub${RESET}"
    echo ""
    echo "GitHub Actions 將在約 2-3 分鐘後完成"
    echo "查看: https://github.com/cccrust/sql6/actions"
    echo ""
    echo "完成後可上傳 PyPI: ./pub.sh $VERSION pypi"
}

function usage() {
    echo "=============================================="
    echo "sql6 發布工具"
    echo "=============================================="
    echo ""
    echo "用法:"
    echo "  ./pub.sh <version> pypi     上傳到 PyPI"
    echo "  ./pub.sh <version> github   建立 GitHub tag（觸發 CI 自動發布）"
    echo "  ./pub.sh <version> all      同時上傳到 PyPI + GitHub"
    echo ""
    echo "範例:"
    echo "  ./pub.sh 2.0.1 pypi"
    echo "  ./pub.sh 2.0.1 github"
    echo "  ./pub.sh 2.0.1 all"
    echo ""
    echo "當前版本: $(get_current_version)"
    echo ""
    echo "發布前請確認:"
    echo "  1. 所有測試通過 (cargo test && ./rutest.sh && ./pytest.sh)"
    echo "  2. _doc/vX.X.md 版本文件已更新"
}

# Main
if [[ $# -eq 0 ]]; then
    usage
    exit 0
fi

if [[ $# -lt 1 ]]; then
    echo -e "${RED}錯誤: 缺少版本號${RESET}"
    echo ""
    usage
    exit 1
fi

NEW_VERSION="$1"
TARGET="${2:-all}"

# Validate version format
if ! [[ "$NEW_VERSION" =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]]; then
    echo -e "${RED}錯誤: 版本格式錯誤: $NEW_VERSION${RESET}"
    echo "正確格式: 1.22 或 1.22.3"
    exit 1
fi

# Get current version and compare
CURRENT_VERSION=$(get_current_version)
echo "當前版本: $CURRENT_VERSION"
echo "新版本: $NEW_VERSION"

if [[ "$NEW_VERSION" == "$CURRENT_VERSION" ]]; then
    echo -e "${YELLOW}版本相同，略過更新${RESET}"
elif ! version_compare "$NEW_VERSION" "$CURRENT_VERSION"; then
    echo -e "${RED}錯誤: 新版本不能低於當前版本 ($CURRENT_VERSION)${RESET}"
    exit 1
fi

# Execute
case "$TARGET" in
    pypi)
        do_pypi "$NEW_VERSION"
        ;;
    github)
        do_github "$NEW_VERSION"
        ;;
    all)
        echo -e "${GREEN}=== 執行完整發布 ===${RESET}"
        echo ""
        echo "Step 1: 更新版本號..."
        update_version "$NEW_VERSION"
        echo ""
        echo "Step 2: 上傳到 crates.io..."
        do_crates "$NEW_VERSION"
        echo ""
        echo "Step 3: 上傳到 PyPI..."
        do_pypi "$NEW_VERSION"
        echo ""
        echo "Step 4: 提交版本更新..."
git add Cargo.toml sql6_pypi/pyproject.toml sql6_pypi/sql6/__init__.py
        git commit -m "Bump version to $NEW_VERSION"
        echo ""
        echo "Step 5: 推送到 GitHub..."
        git push origin main
        echo ""
        echo "Step 6: 建立 GitHub Release (觸發 CI build)..."
        do_github "$NEW_VERSION"
        echo ""
        echo -e "${GREEN}完成！${RESET}"
        echo "- crates.io: cargo install sql6"
        echo "- PyPI: pip install sql6==$NEW_VERSION"
        echo "- GitHub: CI 正在 build binary，完成後會自動可下載"
        ;;
    *)
        echo -e "${RED}錯誤: 未知目標: $TARGET${RESET}"
        echo "請使用 pypi、crates、github 或 all"
        exit 1
        ;;
esac