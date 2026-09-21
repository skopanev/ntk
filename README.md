# ntk

Ticket manager backed by Postgres, served over MCP. One service, several
workspaces, fast.

Tickets live in Postgres behind an HTTP API. The client holds one key and no
database credentials: what you may read and write is decided by the server, not
by the key you carry.

## Connect

There is nothing to install. The service speaks MCP over HTTP; point your client
at it and sign in through the browser.

```json
"ntk": {
  "type": "http",
  "url": "https://YOUR_NTK_DOMAIN/mcp-claude"
}
```

```sh
claude mcp add --transport http --scope user ntk https://YOUR_NTK_DOMAIN/mcp-claude
```

The local stdio client was removed on 21.09.2026. It was never a second
implementation — it opened MCP on stdio and forwarded the calls to this same
HTTP service. A forwarder means a second surface to keep at parity, a second
build, a second thing installed on people's machines, and a second way to fall
behind the server. Clients speak HTTP themselves.

If your configuration still starts a local binary, see **Migrating from
CLI/stdio to HTTP MCP** below.

## Signing in

Authentication is the client's, not a file of ours. In Claude Code open `/mcp`
→ `ntk` → **Authenticate** and sign in with the work account. In Codex run
`codex mcp login ntk` and restart the session afterwards.

On NTK's page choose **Copy sign-in link** and paste the *complete* link into
the client's pending prompt — both `code` and `state`, not the code alone, and
not a link from an earlier attempt. Expired attempts are restarted, not
repaired.

One identity per person: the scope lives on the server, so signing in once
covers every workspace you have access to.

## Workspaces

A workspace is named explicitly in every call. There is no default: a forgotten
workspace used to write into somebody else's silently, and now it is a refusal
with an explanation.

`.ntkrc` is no longer read by anything. It bound a directory to a workspace for
the terminal client; with the client gone, the workspace travels in the call
itself. Files left in repositories are inert.

## Commands

| Command | Description |
| --- | --- |
| `ntk whoami` | Who am I, and which workspaces are open to me |
| `ntk ls` | List tickets — yours by default, `--all` for everyone's |
| `ntk show <id>` | One ticket in full: fields, body, dependencies |
| `ntk walk` | Step through tickets for review; changes nothing |
| `ntk next` | Take the next free ticket into work |
| `ntk start <id>` | Take one specific ticket into work |
| `ntk create <title>` | Open a ticket |
| `ntk update <id>` | Change status, title, body, assignee, tags — in one call |
| `ntk close <id>` | Move to `done` |
| `ntk rm <id>` | Retire a ticket: it stops showing, it is not erased |
| `ntk restore <id>` | Bring a retired ticket back under its own id, search included |
| `ntk deps <id>` | What it waits on, and what waits on it |
| `ntk modules` | Modules of a project, current and archived |
| `ntk meta` | What the workspace has: statuses, priorities, projects, people |
| `ntk projects` | Project lifecycle: retire an emptied name, move tickets wholesale |

Each tool describes its own arguments to the client; this table is the shape,
not the reference.

```
ntk_create   workspace=test  project=ntk  title="Fix the auth flow"  priority=high  tags=backend
ntk_update   workspace=test  id=tst-1     status=to_review  append="progress note"
ntk_ls       workspace=test  status=in_progress  tag=infra
```

`tags` on `ntk_update` takes signed values — `+add`, `-remove`. Unsigned is
rejected, so that "add" never turns out to have meant "replace everything".

## MCP attachments

Both local stdio and remote HTTP MCP expose `ntk_attach` and `ntk_attachments`.
Attach a document in one call; no tracker URL or browser form is needed:

```json
{"name":"ntk_attach","arguments":{"workspace":"test","id":"ntk-xxxxxxxxxx","filename":"research.md","content_type":"text/markdown","content":"# Findings\nDetails go here."}}
```

Provide exactly one source: `content` for UTF-8 text, `content_base64` for
binary files (standard padded base64), or `path` for an absolute file path on
the **local stdio MCP machine**. Remote HTTP MCP accepts contents, never local
paths. Files must contain 1 byte to 50 MiB; the remote JSON request limit is
about 68 MiB. Upload success is returned only after storage verification and
attachment registration. Repeating an upload creates another attachment, so
after an uncertain response, list attachments before retrying.

Call `ntk_attachments` with `workspace` and `id` for filenames, sizes, MIME
types and download links valid for 15 minutes. Call again to refresh links.
Local users need the updated binary and a restarted MCP process; remote users
need the updated API deployment and a refreshed tool list.

## Statuses

Statuses come from the server, not from a list compiled into the client. Each one
sits in a group, and the group decides what an edit costs:

| Group | Statuses | Editing a ticket that sits here |
| --- | --- | --- |
| `todo` | `open`, `blocked`, `to_review`, `reviewed` | free |
| `in_progress` | `in_progress`, `to_test` | needs `--force` |
| `complete` | `done` | needs `--force` |

The guard reads the status the ticket **is in**, never the one you are setting.
Moving a fresh ticket to `done` costs nothing:

```bash
ntk update tst-1 -s done      # tst-1 is open — no force needed
ntk close tst-1               # the same move, spelled shorter
```

What needs `--force` is editing a ticket that has already been picked up —
anything in `in_progress` or `complete`. That is somebody else's work in flight,
so it takes a deliberate flag rather than a convenient default.

Priorities are `high`, `med`, `low`.

## Projects

There is no rename. A project id sits as the prefix of every ticket id, and
those are primary keys — renaming would rewrite the key of every ticket that
already exists, and every reference to it from outside. So "rename" here is two
deliberate steps:

```bash
ntk projects --move old --to new -W ws   # every ticket, one transaction
ntk projects --archive old -W ws         # the emptied name leaves the choice
```

The move keeps ticket ids and dependencies exactly as they were. The price is
that moved tickets keep the old prefix: `old-1a2b3c` now lives in project `new`.
That is deliberate — a stable reference is worth more than a tidy prefix.

A module is registered per project, and a ticket's module is a foreign key on
the pair. The move therefore refuses up front, naming every module the target
lacks, instead of failing halfway through on a constraint.

Archiving refuses while the project still holds tickets: they would not be
deleted, but they would vanish from `meta` — work that is invisible and still
alive. Move first, then retire.

## Tickets

IDs are `{project}-{suffix}`, and a partial suffix matches as long as it is
unambiguous:

```bash
ntk show tst-1
```

A module is the unit of work inside a project. `ntk meta` lists the ones you may
choose; an archived module stays visible but cannot be picked for new work.

## MCP

Connect a remote HTTP MCP server at `https://YOUR_NTK_DOMAIN/mcp-claude`.
Sign in with Google using an account allowed by the NTK enrollment rules.
No local NTK executable is needed for this connection.

For clients with a localhost callback, NTK stays on a **Finish signing in** page.
Choose **Copy sign-in link** and paste the complete link into the app's sign-in
prompt. The link preserves the authorization code and client state; it expires
in five minutes and can be exchanged once with the initiating client's PKCE
verifier. If the app runs on the same computer as the browser, **Return to app**
can complete the local callback instead. Hosted HTTPS callbacks keep their
registered redirect.

HTTP MCP uses the same command catalogue and permissions as the API.

### Migrating from CLI/stdio to HTTP MCP

If your MCP entry contains `"command": "ntk"` (or an absolute path to the binary)
and `"args": ["mcp"]`, it still starts the local CLI. Replace that **existing**
entry with HTTP; keep the server name `ntk`. Use your deployment's hostname in
place of `YOUR_NTK_DOMAIN` below.

1. **Replace the connection.** For a globally installed Claude Code server:

   ```sh
   claude mcp remove --scope user ntk
   claude mcp add --transport http --scope user ntk https://YOUR_NTK_DOMAIN/mcp-claude
   ```

   If the old entry is project/local scoped, replace it in that scope instead;
   an old project entry can override the new global entry.

   For Claude or agy JSON configuration, replace only the `ntk` object inside
   `mcpServers`:

   ```json
   "ntk": {
     "type": "http",
     "url": "https://YOUR_NTK_DOMAIN/mcp-claude"
   }
   ```

   For Codex, replace the old `[mcp_servers.ntk]` block in the active
   `config.toml`:

   ```toml
   [mcp_servers.ntk]
   url = "https://YOUR_NTK_DOMAIN/mcp-claude"
   ```

   Remove the old entry's `command`, `args` and CLI-only environment settings.
   Preserve other MCP servers and unrelated client settings.

2. **Restart the client session.** If it runs in a container, start a new
   container session so it loads the updated MCP configuration.

3. **Authenticate again.** In Claude Code, open `/mcp` → `ntk` → Authenticate.
   Sign in with the Google account that has NTK access. The local CLI's saved
   key does not automatically become the HTTP client's OAuth session.

   In Codex CLI, run `codex mcp login ntk` from a terminal using the same
   Codex configuration. Complete browser sign-in and choose **Return to app**.
   Codex waits for a local callback; a container must expose that callback to
   your browser. After the login command succeeds, restart the Codex session.

4. **Finish the browser step.** Choose **Copy sign-in link** on NTK's page and
   paste the complete link into Claude's pending authorization prompt. Keep
   both `code` and `state`; do not extract only the code or paste an old attempt's
   link. If the attempt expired, start authentication again. Clients without a
   callback-paste prompt need **Return to app** and a reachable local callback.

5. **Verify and remove the old CLI.** Confirm that `ntk` connects and exposes
   its tools, then uninstall the local NTK executable using its installation
   method. Your tickets and workspace access remain on the same NTK server.

## Repository

Three crates, because they have different fates: the core is shared, the service
lives on the droplet, and the migration runner runs at deployment.

- `crates/ntk-core` — types and rules shared by everything
- `crates/ntk-api` — the service
- `crates/ntk-migrate` — migration runner, deployment only
- `sql/` — per-workspace schema and migrations
- `server/` — provisioning, Postgres and Caddy config, systemd units, backups
- `dist/` — MCP bundle packaging
- `release.sh`, `sign-macos.sh` — build, sign, notarize, publish

Deploying from scratch starts with `server/db/core.sql`: the migrations in
`sql/` add tables to the `core` schema but do not create the schema itself.
`server/README.md` has the order.
