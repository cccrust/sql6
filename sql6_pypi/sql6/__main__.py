# sql6 命令列入口點

# 支援三種模式：
# 1. CLI 模式：python -m sql6 (pure Python REPL)
# 2. Web 模式：python -m sql6 --http (FastAPI Web 介面)
# 3. Binary 模式：python -m sql6 --rust ... (執行 Rust 二進位檔)

# 使用範例：
#   python -m sql6                    # CLI 模式
#   python -m sql6 mydb.db            # 打開資料庫
#   python -m sql6 -c "SELECT 1"      # 執行查詢
#   python -m sql6 --http 8080        # Web 介面
#   python -m sql6 --rust             # 使用 Rust REPL

import sys
import os

def main():
    args = sys.argv[1:]

    if args and args[0] == "--rust":
        from sql6._binary import get_binary_path
        binary = get_binary_path()
        os.execv(binary, [binary] + args[1:])
    elif args and args[0] == "--http":
        from sql6.web import run_server
        filtered = [a for a in args[1:] if a != "--http"]
        sys.argv = filtered
        run_server()
    else:
        from sql6.cli import main as cli_main
        cli_main()

if __name__ == "__main__":
    main()