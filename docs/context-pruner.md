# CodeLink Context Pruner

CodeLink enables a request-local context pruning pass by default for long
image-heavy or tool-heavy sessions. This migrated the useful behavior from the
old `codex-manga` fork into the normal `codel` command (`codelink` also works).

Use CodeLink normally:

```bash
codel --yolo
```

The default behavior is equivalent to these settings:

```bash
CODELINK_CONTEXT_PRUNER=1
CODELINK_CONTEXT_DIRECTIVES=1
CODELINK_PRUNE_KEEP_RECENT_TURNS=1
CODELINK_PRUNE_SEGMENT_TURNS=10
CODELINK_PRUNE_HEAVY_TOOL_CHARS=4096
```

Disable it for a session with:

```bash
CODELINK_CONTEXT_PRUNER=0 codel
```

Behavior:

- Visible message text is annotated with stable ids such as
  `[codelink-message-id m0007]` in the request sent to the model.
  These ids are internal compression anchors and should not be echoed in
  user-facing replies.
- The model can call the built-in `compress` tool with `start_id`, `end_id`, and
  `summary` for old closed ranges. On the next prompt build, CodeLink replaces
  that range with a `[codelink-compressed-block ...]` summary item if the ids
  still match and the range is outside the recent-turn guard.
- Older image payloads in messages and tool outputs are replaced with compact
  text placeholders before a prompt is sent to the model.
- Older `image_generation_call.result` payloads are replaced with a placeholder.
- Older heavy tool inputs and outputs are replaced with placeholders that
  preserve tool name, call id, size, success, exit code, and a short preview.
- Recent turns are preserved; by default the newest user turn is never pruned.
- Automatic pruning advances only on segment boundaries by default, which keeps
  prompt-cache invalidation predictable.
- Duplicate tool calls with identical tool name and arguments keep the newest
  output and replace older duplicate outputs.
- Older failed tool calls have large inputs and outputs replaced while
  preserving the call/output structure.
- The stored rollout history is not rewritten; pruning is applied only to the
  model request being built.

Directive mode is enabled by default. The model may request deliberate pruning
after a stable checkpoint, or range compression with the `compress` tool:

```json
{
  "topic": "phase A dry run setup",
  "content": [
    {
      "start_id": "m0003",
      "end_id": "m0011",
      "summary": "Preserve exact files, commands, logs, decisions, failures, and current state."
    }
  ]
}
```

Payload-only checkpoint directives are still supported:

```text
[codelink-context-checkpoint scope=older-images] brief continuity summary and QA status
[codelink-context-checkpoint scope=older-heavy] brief tool/history summary and current state
```

Legacy `[manga-context-checkpoint ...]` directives and `CODEX_MANGA_*`
environment variables are still accepted for old sessions and scripts, but
CodeLink does not install or maintain a separate `manga` command. `codelink` remains a long-form alias. `--profile
manga` is accepted as a compatibility no-op.

Keep this feature isolated from auth, billing, providers, and transport code so
upstream OpenAI Codex changes remain easy to merge.

Implementation note: this is a clean-room CodeLink implementation inspired by
the public behavior of OpenCode-DCP. Do not copy AGPL plugin source into this
Apache-2.0 fork.
