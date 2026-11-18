"""Пароли и защита входа. Только стандартная библиотека."""

from __future__ import annotations

import base64
import hashlib
import hmac
import secrets
import time
from collections import defaultdict, deque

_N, _R, _P = 2**14, 8, 1


def hash_password(password: str) -> str:
    salt = secrets.token_bytes(16)
    digest = hashlib.scrypt(password.encode(), salt=salt, n=_N, r=_R, p=_P, dklen=32)
    b64 = lambda b: base64.b64encode(b).decode()  # noqa: E731
    return f"scrypt${_N}${_R}${_P}${b64(salt)}${b64(digest)}"


def verify_password(password: str, stored: str) -> bool:
    try:
        scheme, n, r, p, salt_b64, digest_b64 = stored.split("$")
        if scheme != "scrypt":
            return False
        salt, expected = base64.b64decode(salt_b64), base64.b64decode(digest_b64)
        digest = hashlib.scrypt(password.encode(), salt=salt, n=int(n), r=int(r), p=int(p), dklen=len(expected))
    except (ValueError, TypeError):
        return False
    return hmac.compare_digest(digest, expected)


def password_problem(password: str) -> str | None:
    """Простая проверка нового пароля; None — пароль подходит."""
    if len(password) < 10:
        return "Пароль должен быть не короче 10 символов"
    if password.isdigit() or password.isalpha():
        return "Смешайте буквы с цифрами или символами"
    return None


def new_token(nbytes: int = 16) -> str:
    return secrets.token_urlsafe(nbytes)


class LoginThrottle:
    """Не больше `attempts` неудачных входов за `window` секунд с одного адреса."""

    def __init__(self, attempts: int = 5, window: float = 300.0) -> None:
        self.attempts = attempts
        self.window = window
        self._fails: dict[str, deque[float]] = defaultdict(deque)

    def _prune(self, key: str, now: float) -> deque[float]:
        q = self._fails[key]
        while q and now - q[0] > self.window:
            q.popleft()
        return q

    def retry_after(self, key: str) -> int:
        """Сколько секунд ждать; 0 — можно пробовать."""
        now = time.monotonic()
        q = self._prune(key, now)
        if len(q) < self.attempts:
            return 0
        return max(1, int(self.window - (now - q[0])) + 1)

    def fail(self, key: str) -> None:
        self._fails[key].append(time.monotonic())

    def reset(self, key: str) -> None:
        self._fails.pop(key, None)
