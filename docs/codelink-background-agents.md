# CodeLink Background Agents

CodeLink is a Codex fork focused on local background agent orchestration. The
goal is to keep Codex's upstream agent loop intact while adding a durable local
supervisor for async agent jobs, long-running shell jobs, status inspection, and
completion notifications back to the active session.

This design must be implemented from public behavior and our own architecture.
Do not inspect or copy leaked proprietary client source when building this fork.

## MVP

- `codel bg ...`: start a background Codex agent task.
- `codel timer --after 5m ...`: wake the active CodeLink session after a delay.
- `codel watch-remote ...`: start a background remote tmux/log watcher.
- `codel jobs`: list running, completed, failed, and canceled jobs.
- `codel result <job_id>`: print latest job result and notification.
- `codel logs <job_id>`: stream or print captured log snapshots.
- `codel notifications`: print unread completion/failure notifications.
- `codel cancel <job_id>`: mark the job canceled.

## Job Model

Each job records:

- stable `job_id`
- `cwd` and repository root
- branch and commit at launch
- optional dedicated git worktree path
- prompt and launch arguments
- model and reasoning effort
- status: `queued`, `running`, `done`, `failed`, `canceled`
- worker pid
- Codex session id when available
- stdout/stderr log paths
- result path
- diff summary path
- timestamps for created, started, completed, and last heartbeat

The durable store should live under `~/.codelink/jobs.sqlite`. Per-job artifacts
should live under `~/.codelink/jobs/<job_id>/`.

## Architecture

### CLI Layer

Add a CodeLink command surface first, without changing the core model loop.
The public command is `codel`, with `codelink` kept as a long-form alias. `codex codelink ...` may remain as a compatibility path while this fork is still close to upstream Codex.

- `codel watch-remote`
- `codel timer`
- `codel bg`
- `codel jobs`
- `codel result`
- `codel logs`
- `codel notifications`
- `codel cancel`

The first implementation can call the existing Codex binary in a child process.
Once stable, it can move closer to internal Rust APIs.

### Supervisor

`codelinkd` owns:

- job registry
- worker process lifecycle
- log capture
- cancellation
- stale-process cleanup
- periodic heartbeat updates
- completion notifications

The supervisor must survive the TUI exiting. A job started in one terminal should
still be inspectable from another terminal.

### Agent Workers

Background agent workers should default to isolated worktrees:

1. Create a worktree from the launch commit.
2. Run the Codex task in that worktree.
3. Capture final answer, session id, and `git diff`.
4. Leave the main working tree untouched.

Direct writes to the current checkout should require an explicit flag.

The initial implementation uses the durable job store and spawns an existing
Codex CLI as a detached child process:

```sh
codel bg \
  --job-id audit-readme \
  --cwd /path/to/repo \
  --codex-arg=--model \
  --codex-arg gpt-5.5 \
  "review README and write findings"
```

This runs:

```sh
codex --model gpt-5.5 exec "review README and write findings"
```

Artifacts are written under `~/.codelink/jobs/audit-readme/`:

- `spec.json`
- `history.log`
- `agent.stdout`
- `agent.stderr`
- `result.md`
- `notification.md`

`codel cancel <job_id>` marks the job canceled; the worker observes that on
its next heartbeat and kills the child process.

### Timer Jobs

Timer jobs are lightweight scheduled wakeups that use the same durable store,
status line, notification, and active wake path:

```sh
codel timer --job-id check-phase-a --after 2h "Check the remote Phase A run"
```

When the delay expires, CodeLink writes `result.md`, creates `notification.md`,
and wakes the active TUI. The main AI then reads `codel result <job_id>` through
the same automatic wake turn used by background agents.

### TUI Notification Bridge

The active CodeLink TUI registers a local wake socket and keeps a slow polling
fallback. Workers send a wake packet when a job is registered or when a
notification is written, so completion events do not depend on continuous
polling. The bridge does two things:

1. newly observed `running` or `stalled` jobs are inserted into history as a
   visible background-task reminder;
2. unread completion notifications are drained, their `notification.md` files
   are read, and a compact completion event is inserted into history.

When a job finishes, the TUI should render a compact event:

```text
[CodeLink] job abc123 completed: docs build fixed
result: ~/.codelink/jobs/abc123/result.md
session: <codex-session-id>
diff: ~/.codelink/jobs/abc123/diff.patch
```

Later, this can be promoted from a visual notification into an assistant-visible
conversation item so the main agent can react to completed work.

## Implementation Order

1. Add the job store and filesystem layout.
2. Add CLI commands for `bg`, `timer`, `watch-remote`, `jobs`, `result`, `logs`,
   `notifications`, and `cancel`.
3. Spawn background Codex worker processes with log capture.
4. Add worktree isolation and diff capture.
5. Add TUI polling and completion notifications.
6. Add richer scheduled jobs.
7. Add background shell jobs after agent jobs are stable.

## Non-Goals For The First Pass

- Rewriting Codex's model loop.
- Building a cloud service.
- Depending on Claude Code internals.
- Applying background agent patches automatically.
- Sharing authentication state outside normal Codex config.
