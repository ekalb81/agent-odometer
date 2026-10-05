"""Check synthetic SQLite rows against two pinned OpenCode generated schemas.

Download packages/core/src/database/schema.gen.ts from the commits named below,
then pass both local source paths. This never reads an OpenCode user database.
"""

import argparse
import hashlib
import json
import re
import sqlite3
import tempfile
from pathlib import Path


SCHEMA_SHA256 = "dc485d31e4d2f4d47f0c0e4243854b04817dc88a42ab9b1bec34b77d9f52ab27"


def verify(version: str, source: Path) -> None:
    raw = source.read_bytes()
    assert hashlib.sha256(raw).hexdigest() == SCHEMA_SHA256, f"unexpected {version} schema source"
    text = raw.decode("utf-8").replace("\\`", "`")
    with tempfile.TemporaryDirectory(prefix="opencode-schema-evidence-") as temp:
        path = Path(temp) / "synthetic.db"
        writer = sqlite3.connect(path)
        for table in ("session", "message", "session_message"):
            ddl = re.search(r"CREATE TABLE `" + table + r"` \((.*?)\);", text, re.S)
            assert ddl is not None, f"missing {table} DDL"
            writer.execute("CREATE TABLE `" + table + "` (" + ddl.group(1) + ")")
        assert "CREATE UNIQUE INDEX `session_message_session_seq_idx`" in text
        writer.execute(
            "CREATE UNIQUE INDEX session_message_session_seq_idx ON session_message(session_id,seq)"
        )
        writer.execute(
            "INSERT INTO session(id,project_id,parent_id,slug,directory,title,version,time_created,time_updated) "
            "VALUES(?,?,?,?,?,?,?,?,?)",
            ("ses-a", "project-a", "parent-a", "task-a", "/synthetic/project", "synthetic", version, 1000, 4000),
        )
        for message_id, seq, created, model, provider, tokens in (
            ("msg-zero", 1, 1000, "model-a", "provider-a", (0, 0)),
            ("msg-used", 2, 3000, "model-b", "provider-b", (4, 2)),
        ):
            data = {
                "model": {"id": model, "providerID": provider},
                "time": {"created": created, "completed": created + 1000},
                "finish": "stop",
                "tokens": {"input": tokens[0], "output": 0, "reasoning": 0, "cache": {"read": tokens[1], "write": 0}},
                "cost": 0,
            }
            writer.execute(
                "INSERT INTO session_message(id,session_id,type,seq,time_created,time_updated,data) "
                "VALUES(?,?,?,?,?,?,?)",
                (message_id, "ses-a", "assistant", seq, created, created, json.dumps(data)),
            )
        try:
            writer.execute(
                "INSERT INTO session_message(id,session_id,type,seq,time_created,time_updated,data) "
                "VALUES(?,?,?,?,?,?,?)",
                ("duplicate", "ses-a", "assistant", 2, 3000, 3000, "{}"),
            )
        except sqlite3.IntegrityError:
            pass
        else:
            raise AssertionError("duplicate session sequence was accepted")
        writer.commit()
        reader = sqlite3.connect(f"file:{path.as_posix()}?mode=ro", uri=True)
        reader.execute("PRAGMA query_only=ON")
        rows = reader.execute(
            "SELECT s.id,s.parent_id,s.directory,m.id,m.seq,m.time_created,"
            "json_extract(m.data,'$.model.id'),json_extract(m.data,'$.model.providerID'),"
            "json_extract(m.data,'$.tokens.input'),json_extract(m.data,'$.tokens.cache.read') "
            "FROM session s JOIN session_message m ON m.session_id=s.id ORDER BY m.seq"
        ).fetchall()
        assert rows == [
            ("ses-a", "parent-a", "/synthetic/project", "msg-zero", 1, 1000, "model-a", "provider-a", 0, 0),
            ("ses-a", "parent-a", "/synthetic/project", "msg-used", 2, 3000, "model-b", "provider-b", 4, 2),
        ]
        reader.close()
        writer.close()
    print(f"{version}: source DDL, metadata/numeric projection, and duplicate sequence verified")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("v11833", type=Path, help="v1.18.33 schema.gen.ts at commit 51ef4be1")
    parser.add_argument("v11834", type=Path, help="v1.18.34 schema.gen.ts at commit aec0b9a6")
    args = parser.parse_args()
    verify("v1.18.33", args.v11833)
    verify("v1.18.34", args.v11834)
