# CodeLink Agent Brief

CodeLink is Infty's fork of OpenAI Codex. Its first goal is to add Claude
Code-style local orchestration around Codex: background tasks, background agent
workers, scheduled status checks, durable job logs, and completion notifications
back to the active controlling AI session.

## Source Boundaries

- Public sources are allowed: OpenAI Codex source, public docs, public issue
  discussions, and behavior observable from normal product use.
- The Claude Code sourcemap directory may be treated only as a high-level
  comparison artifact for feature discovery, UX shape, and risk analysis:
  `/Users/gaojincheng/Downloads/claude-code-sourcemap-main`.
- Do not copy, translate, port, or mechanically rewrite Claude Code source,
  identifiers, algorithms, module structure, protocol details, or tests.
- Do not trust Claude Code internals as an engineering authority. Any similar
  CodeLink feature must be specified from user needs, public behavior, and
  Codex's own architecture, then implemented independently.
- Keep CodeLink clean enough to publish as open source.

## First Product Requirement: Background Run Watcher

CodeLink must support a job like this without blocking the main AI session:

```text
STATUS: IN_PROGRESS — 200-call dry run is running remotely in tmux.

runner: scripts/runners/run_phaseA_200_dry_topk.sh
remote: school:~/MM-SAE-Finance/scripts/runners/run_phaseA_200_dry_topk.sh
tmux session: mmsae-phaseA200-dry
log:
  ~/MM-SAE-Finance/runs/phaseA_200_dry_topk_L14_seed0/logs/phaseA_200_dry_topk.log
stage: extract Cache A/B
progress: 1817 / 20656 segments
elapsed: 10m46s
estimated remaining extract: about 2h20m
run dir size: 2.9G
GPU7 memory: about 20G
```

The user should be able to hand CodeLink a remote status command such as:

```sh
ssh school 'tmux list-sessions | grep mmsae-phaseA200-dry; tail -n 80 ~/MM-SAE-Finance/runs/phaseA_200_dry_topk_L14_seed0/logs/phaseA_200_dry_topk.log'
```

CodeLink should then:

1. register the run as a background job;
2. poll it on a configured interval;
3. parse stage/progress/health from the log when possible;
4. keep the main AI session free for other work;
5. notify the main session when the run finishes, fails, stalls, or crosses a
   user-defined milestone;
6. preserve status history and exact commands for audit.

This requirement covers remote tmux jobs first. Local shell jobs and full
background Codex agent jobs can reuse the same job registry and notification
bridge.

First CLI shape:

```sh
codel watch-remote \
  --job-id phaseA200 \
  --host school \
  --tmux-session mmsae-phaseA200-dry \
  --log-path ~/MM-SAE-Finance/runs/phaseA_200_dry_topk_L14_seed0/logs/phaseA_200_dry_topk.log \
  --success-regex 'STATUS: DONE|DONE|completed successfully'
```

Then the controlling AI can poll:

```sh
codel jobs
codel result phaseA200
codel notifications
```

The `codex codelink ...` subcommand may remain as a compatibility path during
development, but the public CodeLink command should be `codel`; `codelink` remains a long-form alias.

## Initial Job Contract

Every background job should record:

- `job_id`
- `kind`: `remote_tmux`, `local_shell`, or `codex_agent`
- `cwd`
- command or prompt
- remote host when applicable
- tmux session when applicable
- log path
- status: `queued`, `running`, `done`, `failed`, `stalled`, `canceled`
- last parsed progress
- last heartbeat time
- start and finish timestamps
- stdout/stderr or remote log snapshots
- final result summary

Artifacts should live under `~/.codelink/jobs/<job_id>/`. The durable index
should live under `~/.codelink/jobs.sqlite`.

## UX Target

The controlling AI should receive compact events:

```text
[CodeLink] job phaseA200 finished successfully
stage: h1_topk completed
duration: 2h47m
log: ~/.codelink/jobs/phaseA200/log.tail
remote log: school:~/MM-SAE-Finance/runs/phaseA_200_dry_topk_L14_seed0/logs/phaseA_200_dry_topk.log
```

Failures must be visible immediately:

```text
[CodeLink] job phaseA200 failed
reason: remote tmux session exited; log tail contains traceback
log: ~/.codelink/jobs/phaseA200/log.tail
```

## Engineering Rules

- Prefer adding new CodeLink-specific crates or modules instead of growing
  `codex-core`.
- Keep the first pass as a supervisor around existing Codex processes and shell
  commands. Do not rewrite the model loop.
- Background Codex agent jobs should default to isolated git worktrees.
- Never auto-apply a background agent patch to the main working tree unless the
  user explicitly requests it.
- All user-visible UI changes need focused tests or snapshots.

## Second Product Requirement: Background Codex Agent

CodeLink must be able to launch a Codex task in the background and notify the
controlling session after it finishes. The first CLI shape is:

```sh
codel bg --job-id audit-readme --cwd /path/to/repo "review README and write findings"
```

The first pass may run `codel exec <prompt>` as a child process and capture
stdout/stderr under `~/.codelink/jobs/<job_id>/`. The job store and notification
paths are shared with `watch-remote`:

```sh
codel jobs --all
codel result audit-readme
codel logs audit-readme
codel notifications
codel cancel audit-readme
```

Cancellation is cooperative: `codel cancel <job_id>` marks the job canceled,
and the worker kills the child process on its next heartbeat.

## TUI Background Job Reminder Bridge

The interactive CodeLink TUI must make background work visible without requiring
the user to remember polling commands manually.

On startup and then periodically, the TUI should:

1. read active CodeLink jobs from `~/.codelink/jobs.sqlite`;
2. insert a compact history reminder for newly observed `running` or `stalled`
   jobs;
3. drain unread completion notifications;
4. read each job's `notification.md`;
5. insert a compact completion message that points to `codel result <job_id>`
   and `codel logs <job_id>`.

The reminder bridge must not create a CodeLink store just by launching the TUI.
If `~/.codelink/jobs.sqlite` does not exist, it should stay silent.

## Manga Profile Migration

The old `codex-manga` fork should not remain a separate long-lived fork. Its
context-pruning behavior belongs in CodeLink and should be enabled by default:

```sh
codel --yolo
```

CodeLink should not install or maintain a separate `manga` command. `--profile
manga` may remain as a compatibility no-op, but the normal path is just
`codel`. `codelink` remains available as a long-form alias. The request-local
context pruner uses `CODELINK_*` environment variables, accepts
`CODELINK_CONTEXT_PRUNER=0` to disable it, and keeps accepting legacy
`CODEX_MANGA_*` variables and `[manga-context-checkpoint ...]` directives for
old sessions.

CodeLink also carries a clean-room DCP-style compression path: prompt-visible
messages get `[codelink-message-id mNNNN]` markers, the model may call the
built-in `compress` tool for stale closed ranges, and the runtime replaces valid
old ranges with `[codelink-compressed-block ...]` summaries on the next prompt
build. Treat OpenCode-DCP as public behavior reference only; do not copy AGPL
plugin source into this Apache-2.0 fork.

The pruner must stay request-local: it may prune the prompt sent to the model,
but must not rewrite stored rollout history. Keep it isolated from auth,
billing, providers, and transport code.
