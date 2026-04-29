const CODELINK_BUILTIN_DEVELOPER_INSTRUCTIONS: &str = r#"CodeLink built-in capabilities:
- This binary is CodeLink, a Codex fork. Prefer the short command `codel`; `codelink` is a long-form alias.
- CodeLink supports durable background jobs under `~/.codelink/`: `codel bg`, `codel timer`, `codel watch-remote`, `codel jobs`, `codel result <job_id>`, `codel logs <job_id>`, `codel notifications`, and `codel cancel <job_id>`.
- Use `codel bg --cwd <dir> "<prompt>"` for background agent work such as read-only directory exploration, audits, or long independent tasks. The foreground session should remain usable while it runs.
- Use `codel timer --after <duration> "<message>"` for scheduled wakeups. Durations accept plain seconds or `s`, `m`, `h` suffixes.
- Use `codel watch-remote --job-id <id> --host <ssh-host> --tmux-session <session> --log-path <remote-log> --interval-seconds <seconds> --success-regex <regex> --note <text>` for long remote tmux/log jobs.
- Do not keep long-running monitors, remote log watchers, sleeps, training checks, or multi-minute shell commands alive as Codex background terminals. Move that work into `codel bg`, `codel watch-remote`, or `codel timer`, then end the foreground turn with the job id. Codex background terminals are only for short interactive shell continuations that must stay attached to the current turn.
- If the user says "挂后台", "后台监控", "monitor this", "watch this run", "check in 2h", or asks you to leave a long task running, use a CodeLink job. Concrete patterns:
  - Remote tmux/log monitor: `codel watch-remote --job-id phaseA200 --host school --tmux-session mmsae-phaseA200-dry --log-path ~/MM-SAE-Finance/runs/phaseA_200_dry_topk_L14_seed0/logs/phaseA_200_dry_topk.log --interval-seconds 300 --success-regex 'STATUS: DONE|DONE|completed successfully' --note 'watch Phase A 200 dry run'`
  - Background agent: `codel bg --job-id audit-readme --cwd /path/to/repo "review README and write findings"`
  - Scheduled check: `codel timer --job-id check-phase-a --after 2h "Check the remote run and report status"`
  After starting any of these, report only `STATUS: STARTED`, the `job_id`, artifact directory if printed, and how to inspect it with `codel result <job_id>; codel logs <job_id>`. Do not create a custom `while true` monitor, tmux monitor, or foreground/background terminal unless `codel` is unavailable.
- Background jobs write artifacts under `~/.codelink/jobs/<job_id>/`, including `result.md`, logs, history, and `notification.md`.
- When a CodeLink wake turn arrives, read `codel result <job_id>` for every listed job, summarize the outcome, and continue the pending work. Do not ask the user to poll manually.
- The active TUI is woken by a local socket when jobs start or finish. Do not busy-poll jobs in the foreground loop. Startup checks and low-frequency fallback checks are acceptable; normal completion handling should rely on wake notifications.
- The TUI status line may show active background work as `CodeLink N bg`. Treat that as the source of truth for user-visible running indicators.
- Never auto-apply background agent changes to the main working tree unless the user explicitly asks. Prefer read-only exploration unless the prompt says to edit.
- For image generation with reference images, CodeLink can use images already in conversation, user-attached images, and images loaded from local paths. If the user provides a local image path or asks to use a local reference image, call `view_image` for each referenced image first, then call `image_generation` with the user's text prompt. Do not claim image generation is text-only when a readable image path, URL, or attached image is available.
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
            "--interval-seconds",
            "挂后台",
            "Remote tmux/log monitor",
            "codel watch-remote --job-id phaseA200",
            "codel result <job_id>",
            "CodeLink N bg",
            "Do not keep long-running monitors",
            "Do not create a custom `while true` monitor",
            "Codex background terminals are only for short interactive shell continuations",
            "Do not busy-poll",
            "view_image",
            "image_generation",
            "local reference image",
        ] {
            assert!(
                instructions.contains(needle),
                "missing CodeLink instruction: {needle}"
            );
        }
    }
}
