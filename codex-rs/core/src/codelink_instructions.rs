const CODELINK_BUILTIN_DEVELOPER_INSTRUCTIONS: &str = r#"CodeLink built-in capabilities:
- This binary is CodeLink, a Codex fork. Prefer the short command `codel`; `codelink` is a long-form alias.
- CodeLink supports durable background jobs under `~/.codelink/`: `codel bg`, `codel timer`, `codel watch-remote`, `codel jobs`, `codel result <job_id>`, `codel logs <job_id>`, `codel notifications`, and `codel cancel <job_id>`.
- Use `codel bg --cwd <dir> "<prompt>"` for background agent work such as read-only directory exploration, audits, or long independent tasks. The foreground session should remain usable while it runs.
- Use `codel timer --after <duration> "<message>"` for scheduled wakeups. Durations accept plain seconds or `s`, `m`, `h` suffixes.
- Use `codel watch-remote` for long remote tmux/log jobs.
- Background jobs write artifacts under `~/.codelink/jobs/<job_id>/`, including `result.md`, logs, history, and `notification.md`.
- When a CodeLink wake turn arrives, read `codel result <job_id>` for every listed job, summarize the outcome, and continue the pending work. Do not ask the user to poll manually.
- The active TUI is woken by a local socket when jobs start or finish. Do not busy-poll jobs in the foreground loop. Startup checks and low-frequency fallback checks are acceptable; normal completion handling should rely on wake notifications.
- The TUI status line may show active background work as `CodeLink N bg`. Treat that as the source of truth for user-visible running indicators.
- Never auto-apply background agent changes to the main working tree unless the user explicitly asks. Prefer read-only exploration unless the prompt says to edit.
"#;

pub(crate) fn developer_instructions() -> String {
    CODELINK_BUILTIN_DEVELOPER_INSTRUCTIONS.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_mention_core_codelink_commands() {
        let instructions = developer_instructions();
        for needle in [
            "codel bg",
            "codel timer",
            "codel watch-remote",
            "codel result <job_id>",
            "CodeLink N bg",
            "Do not busy-poll",
        ] {
            assert!(
                instructions.contains(needle),
                "missing CodeLink instruction: {needle}"
            );
        }
    }
}
