<!-- NTK -->
Тикеты ведутся через инструменты `ntk`, подключённые по MCP. Инструкция приходит
при подключении, вызывать за ней ничего не нужно.
<!-- /NTK -->

### Agent Tasks

Trigger: user explicitly asks to create a task for the agent / with tag `agent`.

Use `-t agent` only for tasks the automation can execute from a repo checkout: code changes, audits, investigations, docs, and verification. Do NOT use it for vague product decisions, third-party account work, manual ops, visual assets, or atomic cross-repo changes.

The agent scheduler only picks tickets that are `open` and tagged `agent`. When it starts, it changes the ticket to `in_progress` and adds `agent-wip`. If triage decides the task is not suitable, it removes `agent`, adds `agent-skipped`, and sends the ticket back to `open` with a comment.

How the workflow runs under the hood:
- Project name from the ticket is resolved through `workflows/ntk-task2/apps.json` in `hltm-workflows`; that mapping defines the target repo and default base branch.
- Default path: the pipeline creates a worktree from `origin/<base>`, creates branch `ntk/<ticket-id>`, lets the agent edit files, then stages, commits, and pushes that branch.
- Commit message is `ntk <ticket-id>: <title>`.
- On success it moves the ticket to `to_test`, removes `agent-wip`, adds `agent-done`, and appends `Agent: ветка <branch>@<commit>`.
- For audit/investigation tasks with no code change, the agent writes `answer.md`; the pipeline appends it to the ticket instead of pushing a branch.
- For code-change tasks, empty diff is a failure unless an explicit audit answer was produced.

Base branches and task splitting:
- For one independent change, create one focused ticket with `-t agent`.
- For a series that must build on shared groundwork, create a first base/scaffold ticket and tag every ticket in the series with the same `base:<branch>` tag, for example `-t agent,base:feat/paywall-cleanup`.
- With `base:<branch>`, the pipeline commits directly to that branch instead of `ntk/<ticket-id>`. If the branch does not exist, it is created from the project's default base.
- Use `--deps` to express order between split tasks. Keep each ticket independently reviewable: one repo, one clear outcome, concrete files/modules when known, and ACs that can be checked from the diff or output.
- Do not make a giant "do everything" agent ticket. Split by behavior/module when the review would otherwise be muddy.

Verification tags:
- If the target repo has `ntk-verify.json`, add `verify:<name>` tags to opt into those recipes. The pipeline runs matching recipes after implementation.
- Unknown `verify:<name>` fails loudly, so do not invent recipe names.

Agent-ticket minimum detail:
- Target repo/project and affected area.
- Exact user-visible or technical outcome.
- Constraints: what not to change, compatibility concerns, known edge cases.
- ACs that say what to inspect or run, not vague "works correctly".
- For multi-ticket series: dependency/base plan and the shared `base:<branch>` tag.

### Code layout

Keep a source file under 250 lines. Past that, split it along a real seam — a
file per responsibility, not an arbitrary cut at the line count. The limit is
there so a file can be read whole before it is changed; a 700-line module gets
edited by grep instead.

| crate | что внутри |
| --- | --- |
| `ntk-core` | модель тикета: одна форма для CLI, MCP и API |
| `ntk-cli` | бинарь `ntk`: команды, MCP-сервер, самообновление |
| `ntk-api` | сервис на дроплете: авторизация по ключу, чтение, запись, захват |
| `ntk-migrate` | миграции по схеме каждого воркспейса |

Тесты делятся вместе с модулем, который покрывают.

Проверки, которые нельзя заменить фейком, ходят в живой сервис: гонка за тикет
и изоляция воркспейсов. Фейк показывает, что код согласован с представлением о
базе; согласовано ли представление с базой, показывает только живой прогон —
именно так нашлась блокировка справочника статусов в `FOR UPDATE`.
