# sql6 安全與 RBAC 模組

import hashlib
import hmac
import secrets
import time
from typing import Optional, List, Dict, Any, Set
from enum import Enum

class PermissionType(Enum):
    """權限類型"""
    SELECT = "SELECT"
    INSERT = "INSERT"
    UPDATE = "UPDATE"
    DELETE = "DELETE"
    CREATE = "CREATE"
    DROP = "DROP"
    ALL = "ALL"

class Role:
    """角色"""
    def __init__(self, name: str, description: str = ""):
        self.name = name
        self.description = description
        self.permissions: Set[tuple] = set()  # (table, permission)
        self.granted_roles: Set[str] = set()  # 繼承的角色

    def grant(self, table: str, permission: str):
        """授與權限"""
        self.permissions.add((table, permission.upper()))

    def revoke(self, table: str, permission: str):
        """撤銷權限"""
        self.permissions.discard((table, permission.upper()))

    def has_permission(self, table: str, permission: str) -> bool:
        """檢查是否有權限"""
        perm = permission.upper()
        # 檢查直接權限
        if (table, perm) in self.permissions:
            return True
        if (table, "ALL") in self.permissions:
            return True
        if ("*", "ALL") in self.permissions:
            return True
        # 檢查萬用權限
        for t, p in self.permissions:
            if t == "*" and p == perm:
                return True
        return False

    def to_dict(self) -> dict:
        return {
            "name": self.name,
            "description": self.description,
            "permissions": list(self.permissions),
        }

class User:
    """使用者"""
    def __init__(self, username: str, password: str, roles: List[str] = None):
        self.username = username
        self.password_hash = self._hash_password(password)
        self.roles: Set[str] = set(roles) if roles else set()
        self.created_at = time.time()
        self.last_login: Optional[float] = None
        self.is_active = True

    def _hash_password(self, password: str) -> str:
        """密碼雜湊"""
        return hashlib.sha256(password.encode()).hexdigest()

    def verify_password(self, password: str) -> bool:
        return self._hash_password(password) == self.password_hash

    def to_dict(self) -> dict:
        return {
            "username": self.username,
            "roles": list(self.roles),
            "created_at": self.created_at,
            "last_login": self.last_login,
            "is_active": self.is_active,
        }

class RBAC:
    """角色型存取控制"""

    def __init__(self):
        self.users: Dict[str, User] = {}
        self.roles: Dict[str, Role] = {}
        self.current_user: Optional[str] = None

    def create_role(self, name: str, description: str = "") -> Role:
        """建立角色"""
        role = Role(name, description)
        self.roles[name] = role
        return role

    def drop_role(self, name: str):
        """刪除角色"""
        if name in self.roles:
            # 移除角色的所有授權
            role = self.roles[name]
            role.permissions.clear()
            role.granted_roles.clear()
            # 從使用者中移除
            for user in self.users.values():
                user.roles.discard(name)
            del self.roles[name]

    def grant_permission(self, role: Role, table: str, permission: str):
        """授與權限給角色"""
        role.grant(table, permission)

    def revoke_permission(self, role: Role, table: str, permission: str):
        """撤銷角色權限"""
        role.revoke(table, permission)

    def create_user(self, username: str, password: str, roles: List[str] = None) -> User:
        """建立使用者"""
        user = User(username, password, roles)
        self.users[username] = user
        # 自動建立同名角色
        if username not in self.roles:
            self.create_role(username, f"Role for user {username}")
        return user

    def drop_user(self, username: str):
        """刪除使用者"""
        if username in self.users:
            del self.users[username]

    def grant_role(self, user: User, role_name: str):
        """授與角色給使用者"""
        if role_name in self.roles:
            user.roles.add(role_name)

    def revoke_role(self, user: User, role_name: str):
        """撤銷使用者角色"""
        user.roles.discard(role_name)

    def authenticate(self, username: str, password: str) -> Optional[User]:
        """驗證使用者"""
        user = self.users.get(username)
        if user and user.is_active and user.verify_password(password):
            user.last_login = time.time()
            self.current_user = username
            return user
        return None

    def check_permission(self, username: str, table: str, permission: str) -> bool:
        """檢查使用者是否有權限"""
        user = self.users.get(username)
        if not user:
            return False

        # 檢查每個角色
        for role_name in user.roles:
            if role_name in self.roles:
                role = self.roles[role_name]
                if role.has_permission(table, permission):
                    return True
                # 檢查繼承的角色
                for inherited in role.granted_roles:
                    if inherited in self.roles and self.roles[inherited].has_permission(table, permission):
                        return True

        return False

    def get_user_info(self, username: str) -> Optional[dict]:
        """取得使用者資訊"""
        user = self.users.get(username)
        if user:
            info = user.to_dict()
            # 加入角色詳細資訊
            info["role_details"] = []
            for role_name in user.roles:
                if role_name in self.roles:
                    info["role_details"].append(self.roles[role_name].to_dict())
            return info
        return None

    def list_roles(self) -> List[dict]:
        """列出所有角色"""
        return [role.to_dict() for role in self.roles.values()]

    def list_users(self) -> List[dict]:
        """列出所有使用者"""
        return [user.to_dict() for user in self.users.values()]

class Encryption:
    """簡易加密（欄位級）"""

    def __init__(self):
        self._key: Optional[bytes] = None

    def set_key(self, key: str):
        """設定金鑰"""
        self._key = hashlib.sha256(key.encode()).digest()

    def encrypt(self, data: str) -> str:
        """加密資料"""
        if not self._key:
            raise ValueError("Encryption key not set")
        # 簡單的 XOR 加密（生產環境應使用 AES）
        data_bytes = data.encode()
        encrypted = bytes(a ^ b for a, b in zip(data_bytes, self._key * len(data_bytes)))
        return encrypted.hex()

    def decrypt(self, encrypted_hex: str) -> str:
        """解密資料"""
        if not self._key:
            raise ValueError("Encryption key not set")
        encrypted = bytes.fromhex(encrypted_hex)
        decrypted = bytes(a ^ b for a, b in zip(encrypted, self._key * len(encrypted)))
        return decrypted.decode()

class AuditLogger:
    """審計日誌"""

    def __init__(self, enabled: bool = True):
        self.enabled = enabled
        self.logs: List[dict] = []
        self._filter_actions: Set[str] = set()
        self._filter_tables: Set[str] = set()

    def add_filter(self, action: List[str] = None, table: List[str] = None):
        """新增過濾"""
        if action:
            self._filter_actions.update(action)
        if table:
            self._filter_tables.update(table)

    def log(self, user: str, action: str, table: str, sql: str = "", result: str = "SUCCESS"):
        """記錄審計日誌"""
        if not self.enabled:
            return
        # 檢查過濾
        if self._filter_actions and action.upper() not in self._filter_actions:
            return
        if self._filter_tables and table not in self._filter_tables:
            return

        entry = {
            "timestamp": time.time(),
            "user": user,
            "action": action,
            "table": table,
            "sql": sql[:500] if sql else "",
            "result": result,
        }
        self.logs.append(entry)

    def query(self, user: str = None, action: str = None, 
              table: str = None, limit: int = 100) -> List[dict]:
        """查詢審計日誌"""
        results = self.logs
        if user:
            results = [r for r in results if r["user"] == user]
        if action:
            results = [r for r in results if r["action"] == action]
        if table:
            results = [r for r in results if r["table"] == table]
        return results[-limit:]

    def generate_report(self, start: float = None, end: float = None) -> dict:
        """產生審計報告"""
        logs = self.logs
        if start:
            logs = [r for r in logs if r["timestamp"] >= start]
        if end:
            logs = [r for r in logs if r["timestamp"] <= end]

        # 統計
        actions = {}
        users = {}
        tables = {}

        for log in logs:
            actions[log["action"]] = actions.get(log["action"], 0) + 1
            users[log["user"]] = users.get(log["user"], 0) + 1
            tables[log["table"]] = tables.get(log["table"], 0) + 1

        return {
            "total_events": len(logs),
            "by_action": actions,
            "by_user": users,
            "by_table": tables,
            "start_time": start,
            "end_time": end,
        }

# 便捷函數
def create_rbac() -> RBAC:
    """建立 RBAC 系統"""
    return RBAC()

def create_encryption(key: str) -> Encryption:
    """建立加密器"""
    enc = Encryption()
    enc.set_key(key)
    return enc

def create_audit_logger(enabled: bool = True) -> AuditLogger:
    """建立審計日誌"""
    return AuditLogger(enabled)