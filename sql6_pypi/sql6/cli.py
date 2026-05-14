# sql6 命令列介面 (Pure Python REPL)
#
# 提供互動式命令列介面，等效於 Rust REPL。
# 使用方式：
#   python -m sql6
#   python -m sql6 mydb.db
#   python -m sql6 -c "SELECT 1"

import sys
import os
import json
import cmd
from typing import Optional, List

from sql6.client import connect, Connection, Cursor, Error as Sql6Error
from sql6._binary import get_binary_path


class Sql6Cli(cmd.Cmd):
    """sql6 互動式命令列介面"""

    intro = "sql6 interactive mode (Python)\nType '.help' for available commands."
    prompt = "sql6> "

    def __init__(self, db_path: Optional[str] = None, transport: str = "subprocess"):
        super().__init__()
        self.db_path = db_path
        self.transport = transport
        self.conn: Optional[Connection] = None
        self.mode = "column"
        self._connect()

    def _connect(self):
        """連線到 sql6 server"""
        try:
            if self.transport == "websocket":
                self.conn = connect(self.db_path, transport="websocket")
            else:
                self.conn = connect(self.db_path)

            if self.conn:
                print(f"Connected to {'memory' if not self.db_path else self.db_path}")
        except Sql5Error as e:
            print(f"Error: {e}")
            sys.exit(1)

    def _call_method(self, method: str, **kwargs) -> Cursor:
        """呼叫 server 方法"""
        request = {"method": method}
        request.update(kwargs)
        return self.conn.execute({"": ""})  # Dummy to keep connection alive

    def do_tables(self, args: str) -> bool:
        """列出所有表格：.tables"""
        if not self.conn:
            return True
        try:
            cursor = self.conn.tables()
            rows = cursor.fetchall()
            if not rows:
                print("(no tables)")
            else:
                for row in rows:
                    print(row[0])
        except Sql5Error as e:
            print(f"Error: {e}")
        return True

    def do_schema(self, args: str) -> bool:
        """顯示表格結構：.schema [TABLE]"""
        if not self.conn:
            return True
        try:
            cursor = self.conn.schema(args.strip())
            if cursor.ok:
                rows = cursor.fetchall()
                if not rows:
                    table = args.strip()
                    print(f"Table '{table}' not found" if table else "(no tables)")
                else:
                    for row in rows:
                        print(row[0])
            else:
                print(f"Error: {cursor.error}")
        except Sql5Error as e:
            print(f"Error: {e}")
        return True

    def do_indexes(self, args: str) -> bool:
        """列出所有索引：.indexes"""
        if not self.conn:
            return True
        try:
            cursor = self.conn.execute(
                "SELECT name, tbl_name FROM sqlite_master WHERE type='index' AND name NOT LIKE 'sqlite_%' ORDER BY name"
            )
            rows = cursor.fetchall()
            if not rows:
                print("(no indexes)")
            else:
                for row in rows:
                    print(f"{row[0]} ({row[1]})")
        except Sql5Error as e:
            print(f"Error: {e}")
        return True

    def do_mode(self, args: str) -> bool:
        """設定輸出格式：.mode [csv|column]"""
        mode = args.strip().lower()
        if mode in ("csv", "column"):
            self.mode = mode
            print(f"Mode set to {mode}")
        else:
            print("Usage: .mode [csv|column]")
        return True

    def do_quit(self, args: str) -> bool:
        """退出：.quit 或 .exit"""
        print("Goodbye!")
        return False

    def do_exit(self, args: str) -> bool:
        """退出：.exit 或 .quit"""
        return self.do_quit(args)

    def do_EOF(self, args: str) -> bool:
        """Ctrl+D 退出"""
        print()
        return self.do_quit(args)

    def do_help(self, args: str) -> bool:
        """顯示說明：.help"""
        print("""
Available commands:
  .tables          List all tables
  .schema [TABLE]  Show CREATE statement for table
  .indexes         List all indexes
  .mode [csv|column]  Set output mode
  .quit           Exit REPL
  .help           Show this help

SQL statements are executed directly.
Examples:
  SELECT * FROM users
  CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)
  INSERT INTO test VALUES (1, 'hello')
""")
        return True

    def default(self, line: str) -> bool:
        """執行 SQL 語句或 dot 命令"""
        line = line.strip()
        if not line:
            return True

        if line.startswith("."):
            rest = line[1:]
            parts = rest.split(maxsplit=1)
            cmd = "do_" + parts[0]
            args = parts[1] if len(parts) > 1 else ""
            if hasattr(self, cmd):
                return getattr(self, cmd)(args)
            print(f"Unknown command: {line}")
            return True

        if not self.conn:
            return True

        try:
            cursor = self.conn.execute(line)
            self._print_result(cursor)
        except Sql5Error as e:
            print(f"Error: {e}")
        return True

    def _print_result(self, cursor: Cursor):
        """格式化輸出查詢結果"""
        if not cursor.columns and not cursor.rows:
            if cursor.affected is not None:
                print(f"({cursor.affected} rows affected)")
            return

        if self.mode == "csv":
            if cursor.columns:
                print(",".join(cursor.columns))
            for row in cursor.rows:
                print(",".join(str(v) for v in row))
        else:
            for row in cursor.rows:
                print(" | ".join(str(v) for v in row))

    def postloop(self):
        """退出時關閉連線"""
        if self.conn:
            self.conn.close()


def _split_statements(command: str) -> List[str]:
    """Split command string by semicolons, handling quoted strings"""
    statements = []
    current = ""
    in_string = False
    string_char = ""

    for c in command:
        if c == "\\" and not in_string:
            current += c
            continue
        if c in ("'", '"') and not in_string:
            in_string = True
            string_char = c
        elif c == string_char and in_string:
            in_string = False
            string_char = ""
        elif c == ";" and not in_string:
            if current.strip():
                statements.append(current.strip())
            current = ""
        else:
            current += c

    if current.strip():
        statements.append(current.strip())

    return statements


def run_cli(
    db_path: Optional[str] = None,
    command: Optional[str] = None,
    script: Optional[str] = None,
    transport: str = "subprocess"
):
    """執行 CLI"""
    cli = Sql5Cli(db_path, transport)

    if command:
        statements = _split_statements(command)
        for stmt in statements:
            stmt = stmt.strip()
            if not stmt:
                continue
            if stmt.startswith("."):
                cli.default(stmt)
            else:
                try:
                    cursor = cli.conn.execute(stmt)
                    cli._print_result(cursor)
                except Sql5Error as e:
                    print(f"Error: {e}")
                    sys.exit(1)
        cli.postloop()
        return

    if script:
        try:
            with open(script, "r") as f:
                for line in f:
                    line = line.strip()
                    if not line or line.startswith("--"):
                        continue
                    if line.startswith("."):
                        parts = line.split()
                        cmd = parts[0]
                        args = " ".join(parts[1:])
                        if cmd == ".tables":
                            cli.do_tables(args)
                        elif cmd == ".schema":
                            cli.do_schema(args)
                        elif cmd == ".indexes":
                            cli.do_indexes(args)
                        elif cmd == ".mode":
                            cli.do_mode(args)
                        elif cmd == ".quit":
                            break
                    else:
                        cli.default(line)
        except Sql5Error as e:
            print(f"Error: {e}")
            sys.exit(1)
        except FileNotFoundError:
            print(f"Error: script file '{script}' not found")
            sys.exit(1)
        return

    cli.cmdloop()


def main():
    """命令列入口點"""
    import argparse

    parser = argparse.ArgumentParser(description="sql6 CLI")
    parser.add_argument("db", nargs="?", help="Database file path")
    parser.add_argument("-c", "--command", help="Execute SQL and exit")
    parser.add_argument("-s", "--script", help="Execute SQL script file")
    parser.add_argument(
        "--transport",
        default="subprocess",
        choices=["subprocess", "websocket"],
        help="Transport mode"
    )
    parser.add_argument(
        "--host",
        default="127.0.0.1",
        help="WebSocket host (default: 127.0.0.1)"
    )
    parser.add_argument(
        "--port",
        type=int,
        default=8080,
        help="WebSocket port (default: 8080)"
    )
    parser.add_argument(
        "--binary",
        help="Path to sql6 binary (overrides SQL6_BINARY)"
    )

    args = parser.parse_args()

    if args.binary:
        os.environ["SQL5_BINARY"] = args.binary

    if args.transport == "websocket":
        os.environ["SQL5_WEBSOCKET_HOST"] = args.host
        os.environ["SQL5_WEBSOCKET_PORT"] = str(args.port)

    run_cli(
        db_path=args.db,
        command=args.command,
        script=args.script,
        transport=args.transport
    )


if __name__ == "__main__":
    main()