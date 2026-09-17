#!/usr/bin/env python3
"""Real PostgreSQL + HTTP MCP checks. Requires Docker and cargo build -p ntk-api.

Creates an isolated disposable container. Does not use production credentials.
Storage byte transfer is covered by the client's HTTP contract tests; this
suite checks actual authorization, database isolation and the remote adapter.
"""
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parent.parent
NAME = "ntk-attachment-test-" + uuid.uuid4().hex[:10]
KEY = "ntk-attachment-test-key"


def run(*args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kwargs).stdout.strip()


def sql(text):
    return run("docker", "exec", "-i", "-e", "PGPASSWORD=test", NAME,
               "psql", "-h", "127.0.0.1", "-U", "postgres", "-d", "postgres",
               "-v", "ON_ERROR_STOP=1", "-At", input=text)


def fixture():
    sql("create role ntk_admin; create role ntk_api login noinherit password 'test';")
    sql((ROOT / "server/db/core.sql").read_text())
    for ws in ("attach_a", "attach_b"):
        sql(f"""
            create schema {ws}; create role ntk_ws_{ws};
            grant usage on schema {ws} to ntk_ws_{ws};
            grant ntk_ws_{ws} to ntk_api;
            insert into core.workspaces(name,schema_name) values ('{ws}','{ws}');
        """)
        for migration in ("001_init", "002_seed", "004_trigger_schema", "009_soft_delete"):
            sql(f"set search_path = {ws};\n" + (ROOT / f"sql/{migration}.sql").read_text())
        sql(f"""
            grant select,insert,update,delete on all tables in schema {ws} to ntk_ws_{ws};
            grant usage,select on all sequences in schema {ws} to ntk_ws_{ws};
            set search_path = {ws};
            insert into tickets(id,title,status) values ('tst-1234567890','Fixture','open');
            insert into attachments(ticket_id,object_key,filename,size_bytes)
                values ('tst-1234567890','attachments/{ws}/tst-1234567890/file','{ws}.txt',3);
        """)
    digest = hashlib.sha256(KEY.encode()).hexdigest()
    sql(f"""
        insert into core.users(id,display_name,kind) values ('tester','Tester','human');
        insert into core.user_workspaces values ('tester','attach_a');
        insert into core.user_keys(key_hash,key_prefix,user_id) values ('{digest}','test','tester');
    """)


def request(base, method, params):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    req = urllib.request.Request(base + "/mcp-claude", data=body, headers={
        "Authorization": "Bearer " + KEY, "Content-Type": "application/json",
    })
    with urllib.request.urlopen(req, timeout=15) as response:
        raw = response.read().decode()
    if raw.startswith("event:") or raw.startswith("data:"):
        messages = [json.loads(line[6:]) for line in raw.splitlines() if line.startswith("data: ")]
        return next(message["result"] for message in messages if "result" in message)
    return json.loads(raw)["result"]


def check(base, name, workspace="attach_a", **extra):
    args = {"workspace": workspace, "id": "TST-1234567890", **extra}
    result = request(base, "tools/call", {"name": name, "arguments": args})
    return result["isError"], result["content"][0]["text"]


def assertions(base):
    tools = request(base, "tools/list", {})["tools"]
    assert {"ntk_attach", "ntk_attachments"} <= {tool["name"] for tool in tools}
    error, text = check(base, "ntk_attachments")
    files = json.loads(text)["attachments"]
    assert not error and [f["filename"] for f in files] == ["attach_a.txt"]
    assert "X-Amz-Signature=" in files[0]["url"]
    for name in ("ntk_attach", "ntk_attachments"):
        extra = {"filename": "test.txt", "content": "hello"} if name == "ntk_attach" else {}
        error, text = check(base, name, workspace="attach_b", **extra)
        assert error and "no access" in text, (name, text)
    error, text = check(base, "ntk_attach", filename="file", path="/etc/passwd")
    assert error and "local stdio MCP" in text
    # Exceeds Axum's former 2 MiB request limit. It must reach the real ticket lookup.
    error, text = check(base, "ntk_attach", id="missing-1234567890", filename="large.txt",
                        content="x" * (3 * 1024 * 1024))
    assert error and "no such ticket" in text
    sql("update attach_a.tickets set deleted_at=now() where id='tst-1234567890';")
    error, text = check(base, "ntk_attach", filename="file", content="hello")
    assert error and "no such ticket" in text
    error, text = check(base, "ntk_attachments")
    assert not error and json.loads(text)["attachments"] == []
    assert sql("select count(*) from attach_a.attachments;") == "1"
    print("PASS: tool discovery, real DB listing, case-insensitive IDs, cross-workspace upload/list denial,")
    print("      remote path refusal, >2 MiB HTTP request, deleted-ticket checks; no new attachments.")


def main():
    process = None
    with tempfile.TemporaryFile(mode="w+") as log:
        try:
            run("docker", "run", "-d", "--name", NAME, "-e", "POSTGRES_PASSWORD=test",
                "-p", "127.0.0.1::5432", "postgres:16")
            for _ in range(100):
                try:
                    sql("select 1;")
                    break
                except subprocess.CalledProcessError:
                    time.sleep(0.1)
            fixture()
            pgport = run("docker", "port", NAME, "5432/tcp").rsplit(":", 1)[1]
            with socket.socket() as sock:
                sock.bind(("127.0.0.1", 0))
                port = sock.getsockname()[1]
            base = f"http://127.0.0.1:{port}"
            env = {k: v for k, v in os.environ.items()
                   if not any(part in k for part in ("AWS_", "SPACES", "QDRANT", "OPENAI", "EMBED"))}
            env.update(DATABASE_URL=f"postgresql://ntk_api:test@127.0.0.1:{pgport}/postgres",
                       PORT=str(port), PUBLIC_URL=base, GOOGLE_HD="example.com",
                       GOOGLE_CLIENT_ID="test", GOOGLE_CLIENT_SECRET="test",
                       AWS_ACCESS_KEY_ID="fixture", AWS_SECRET_ACCESS_KEY="fixture",
                       SPACES_BUCKET="fixture", SPACES_ENDPOINT="https://storage.invalid")
            process = subprocess.Popen([str(ROOT / "target/debug/ntk-api")], env=env, stdout=log, stderr=log)
            for _ in range(100):
                try:
                    with urllib.request.urlopen(base + "/health", timeout=1):
                        break
                except (OSError, urllib.error.URLError):
                    time.sleep(0.1)
            assertions(base)
        except Exception as error:
            log.seek(0)
            print(log.read())
            if isinstance(error, subprocess.CalledProcessError):
                print(error.stderr)
            raise
        finally:
            if process:
                process.terminate()
                process.wait(timeout=10)
            subprocess.run(["docker", "rm", "-fv", NAME], capture_output=True)


if __name__ == "__main__":
    main()
