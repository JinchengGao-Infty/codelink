# CodeLink Context Pruner

CodeLink includes an opt-in request-local context pruning pass for long
image-heavy or tool-heavy sessions. The first built-in preset is the `manga`
profile, migrated from the old `codex-manga` fork.

Enable it with:

```bash
codelink --profile manga
```

The profile sets these environment defaults unless the user already set them:

```bash
CODELINK_CONTEXT_PRUNER=1
CODELINK_CONTEXT_DIRECTIVES=1
CODELINK_PRUNE_KEEP_RECENT_TURNS=1
CODELINK_PRUNE_SEGMENT_TURNS=10
CODELINK_PRUNE_HEAVY_TOOL_CHARS=4096
```

Behavior:

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
after a stable checkpoint:

```text
[codelink-context-checkpoint scope=older-images] brief continuity summary and QA status
[codelink-context-checkpoint scope=older-heavy] brief tool/history summary and current state
```

Legacy `[manga-context-checkpoint ...]` directives and `CODEX_MANGA_*`
environment variables are still accepted for old sessions and scripts, but
CodeLink does not install or maintain a separate `manga` command.

Keep this feature isolated from auth, billing, providers, and transport code so
upstream OpenAI Codex changes remain easy to merge.
