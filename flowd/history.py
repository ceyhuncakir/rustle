"""Local history of dictations, and the profile learned from it.

Nothing is written unless learning is switched on. With it off, Flow keeps no
record of anything you say - the transcript exists only long enough to be
pasted. Everything here is a local SQLite file; nothing leaves the machine.
"""

from __future__ import annotations

import json
import sqlite3
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path

from .config import DATA_DIR

DB_PATH = DATA_DIR / "history.db"

SCHEMA = """
CREATE TABLE IF NOT EXISTS dictation (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    at     TEXT    NOT NULL,
    app    TEXT    NOT NULL DEFAULT '',
    title  TEXT    NOT NULL DEFAULT '',
    raw    TEXT    NOT NULL,
    clean  TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS dictation_at ON dictation(at);

CREATE TABLE IF NOT EXISTS profile (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    samples    INTEGER NOT NULL DEFAULT 0
);
"""


class History:
    def __init__(self, path: Path | None = None) -> None:
        self.path = path or DB_PATH
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with self._connect() as db:
            db.executescript(SCHEMA)

    @contextmanager
    def _connect(self):
        # A new connection per operation: the daemon writes from its main loop
        # and reads from a worker, and sqlite connections are not shareable.
        db = sqlite3.connect(self.path, timeout=5.0)
        db.row_factory = sqlite3.Row
        try:
            yield db
            db.commit()
        finally:
            db.close()

    # -- dictations ---------------------------------------------------------

    def record(self, raw: str, clean: str, context: dict[str, str] | None = None) -> None:
        context = context or {}
        with self._connect() as db:
            db.execute(
                "INSERT INTO dictation (at, app, title, raw, clean) VALUES (?,?,?,?,?)",
                (
                    datetime.now(timezone.utc).isoformat(timespec="seconds"),
                    context.get("app", ""),
                    context.get("title", ""),
                    raw,
                    clean,
                ),
            )

    def count(self) -> int:
        with self._connect() as db:
            return db.execute("SELECT COUNT(*) FROM dictation").fetchone()[0]

    def recent(self, limit: int = 50) -> list[sqlite3.Row]:
        with self._connect() as db:
            return db.execute(
                "SELECT * FROM dictation ORDER BY id DESC LIMIT ?", (limit,)
            ).fetchall()

    def samples_for_learning(self, limit: int = 200) -> list[str]:
        """Cleaned text of recent dictations - what the speaker actually meant,
        rather than what the recogniser first guessed."""
        with self._connect() as db:
            rows = db.execute(
                "SELECT clean FROM dictation ORDER BY id DESC LIMIT ?", (limit,)
            ).fetchall()
        return [r["clean"] for r in rows if r["clean"].strip()]

    def clear(self) -> int:
        with self._connect() as db:
            removed = db.execute("SELECT COUNT(*) FROM dictation").fetchone()[0]
            db.execute("DELETE FROM dictation")
            db.execute("DELETE FROM profile")
        return removed

    # -- learned profile ----------------------------------------------------

    def set_profile(self, key: str, value, samples: int = 0) -> None:
        with self._connect() as db:
            db.execute(
                "INSERT INTO profile (key, value, updated_at, samples) VALUES (?,?,?,?) "
                "ON CONFLICT(key) DO UPDATE SET value=excluded.value, "
                "updated_at=excluded.updated_at, samples=excluded.samples",
                (
                    key,
                    json.dumps(value),
                    datetime.now(timezone.utc).isoformat(timespec="seconds"),
                    samples,
                ),
            )

    def get_profile(self, key: str, default=None):
        with self._connect() as db:
            row = db.execute("SELECT value FROM profile WHERE key = ?", (key,)).fetchone()
        return json.loads(row["value"]) if row else default

    def profile_meta(self, key: str) -> dict | None:
        with self._connect() as db:
            row = db.execute(
                "SELECT updated_at, samples FROM profile WHERE key = ?", (key,)
            ).fetchone()
        return dict(row) if row else None
