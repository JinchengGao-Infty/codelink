use std::fmt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use clap::Args;
use clap::Parser;
use regex_lite::Regex;
use serde::Deserialize;
use serde::Serialize;
use sqlx::Row;
use sqlx::SqlitePool;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::sleep;

static JOB_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Parser)]
pub struct Cli {
    #[command(subcommand)]
    pub subcommand: CommandKind,
}

#[derive(Debug, clap::Subcommand)]
pub enum CommandKind {
    /// Run a background Codex agent task.
    Bg(BgArgs),

    /// Watch a remote tmux/log job in the background.
    WatchRemote(WatchRemoteArgs),

    /// List CodeLink jobs.
    Jobs(JobsArgs),

    /// Show the latest result or notification for a job.
    Result(JobIdArgs),

    /// Print a job's captured log snapshots.
    Logs(JobIdArgs),

    /// Print unread completion/failure notifications.
    Notifications(NotificationsArgs),

    /// Mark a job canceled. Running watchers will stop on the next poll.
    Cancel(JobIdArgs),

    /// Internal worker entrypoint.
    #[clap(hide = true)]
    Worker(WorkerArgs),
}

#[derive(Debug, Args, Clone)]
pub struct BgArgs {
    /// Stable human-readable job id. Defaults to an auto-generated id.
    #[arg(long)]
    pub job_id: Option<String>,

    /// Working directory for the background agent. Defaults to the current directory.
    #[arg(long, value_name = "DIR")]
    pub cwd: Option<PathBuf>,

    /// CodeLink-compatible executable to run.
    #[arg(long, default_value = "codelink")]
    pub codex_bin: String,

    /// Extra argument passed to CodeLink before `exec`. Repeat for multiple args.
    #[arg(long = "codex-arg", value_name = "ARG", allow_hyphen_values = true)]
    pub codex_args: Vec<String>,

    /// Prompt for `codelink exec`.
    #[arg(value_name = "PROMPT", required = true)]
    pub prompt: Vec<String>,
}

#[derive(Debug, Args, Clone)]
pub struct WatchRemoteArgs {
    /// Stable human-readable job id. Defaults to an auto-generated id.
    #[arg(long)]
    pub job_id: Option<String>,

    /// SSH host alias, for example `school`.
    #[arg(long)]
    pub host: String,

    /// Remote tmux session name to check.
    #[arg(long)]
    pub tmux_session: String,

    /// Remote log path to tail.
    #[arg(long)]
    pub log_path: String,

    /// Poll interval in seconds.
    #[arg(long, default_value_t = 60)]
    pub interval_seconds: u64,

    /// Number of remote log lines to capture per poll.
    #[arg(long, default_value_t = 120)]
    pub tail_lines: u32,

    /// Regex that marks the job successful when found in the log tail.
    #[arg(long)]
    pub success_regex: Option<String>,

    /// Regex that marks the job failed when found in the log tail.
    #[arg(long, default_value = "(?i)(traceback|\\berror\\b|failed|oom|killed)")]
    pub failure_regex: String,

    /// Mark the job stalled after this many seconds without a log update.
    #[arg(long, default_value_t = 1800)]
    pub stall_after_seconds: i64,

    /// Optional note shown in job listings and notifications.
    #[arg(long)]
    pub note: Option<String>,
}

#[derive(Debug, Args)]
pub struct JobsArgs {
    /// Include terminal jobs.
    #[arg(long, default_value_t = false)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct JobIdArgs {
    pub job_id: String,
}

#[derive(Debug, Args)]
pub struct NotificationsArgs {
    /// Include notifications that were already printed.
    #[arg(long, default_value_t = false)]
    pub all: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeLinkNotification {
    pub job_id: String,
    pub notification_path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeLinkJobSummary {
    pub job_id: String,
    pub kind: String,
    pub status: String,
    pub artifact_dir: PathBuf,
    pub last_summary: Option<String>,
}

#[derive(Debug, Args)]
pub struct WorkerArgs {
    #[arg(long)]
    pub job_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum JobStatus {
    Running,
    Done,
    Failed,
    Stalled,
    Canceled,
}

impl JobStatus {
    fn as_str(self) -> &'static str {
        match self {
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
            JobStatus::Stalled => "stalled",
            JobStatus::Canceled => "canceled",
        }
    }

    fn from_db(value: &str) -> Result<Self> {
        match value {
            "running" => Ok(Self::Running),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            "stalled" => Ok(Self::Stalled),
            "canceled" => Ok(Self::Canceled),
            other => bail!("unknown CodeLink job status `{other}`"),
        }
    }

    fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Canceled)
    }
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JobKind {
    RemoteTmux,
    CodexAgent,
}

impl JobKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::RemoteTmux => "remote_tmux",
            Self::CodexAgent => "codex_agent",
        }
    }

    fn from_db(value: &str) -> Result<Self> {
        match value {
            "remote_tmux" => Ok(Self::RemoteTmux),
            "codex_agent" => Ok(Self::CodexAgent),
            other => bail!("unknown CodeLink job kind `{other}`"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RemoteTmuxSpec {
    host: String,
    tmux_session: String,
    log_path: String,
    interval_seconds: u64,
    tail_lines: u32,
    success_regex: Option<String>,
    failure_regex: String,
    stall_after_seconds: i64,
    note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexAgentSpec {
    codex_bin: String,
    codex_args: Vec<String>,
    prompt: String,
}

#[derive(Debug)]
struct JobRecord {
    job_id: String,
    kind: JobKind,
    status: JobStatus,
    cwd: PathBuf,
    spec_json: String,
    artifact_dir: PathBuf,
    last_summary: Option<String>,
    last_log_bytes: Option<i64>,
    last_log_changed_at: Option<i64>,
    child_pid: Option<i64>,
}

#[derive(Debug)]
struct RuntimePaths {
    jobs_dir: PathBuf,
    db_path: PathBuf,
}

impl RuntimePaths {
    fn discover() -> Result<Self> {
        let root = if let Ok(raw) = std::env::var("CODELINK_HOME") {
            PathBuf::from(raw)
        } else {
            dirs::home_dir()
                .context("could not determine home directory")?
                .join(".codelink")
        };
        let jobs_dir = root.join("jobs");
        let db_path = root.join("jobs.sqlite");
        Ok(Self { jobs_dir, db_path })
    }

    async fn ensure(&self) -> Result<()> {
        tokio::fs::create_dir_all(&self.jobs_dir)
            .await
            .with_context(|| format!("failed to create {}", self.jobs_dir.display()))?;
        Ok(())
    }
}

pub async fn run_main(cli: Cli) -> Result<()> {
    match cli.subcommand {
        CommandKind::Bg(args) => bg(args).await,
        CommandKind::WatchRemote(args) => watch_remote(args).await,
        CommandKind::Jobs(args) => list_jobs(args).await,
        CommandKind::Result(args) => print_result(args).await,
        CommandKind::Logs(args) => print_logs(args).await,
        CommandKind::Notifications(args) => print_notifications(args).await,
        CommandKind::Cancel(args) => cancel_job(args).await,
        CommandKind::Worker(args) => run_worker(args).await,
    }
}

async fn bg(args: BgArgs) -> Result<()> {
    let prompt = args.prompt.join(" ");
    if prompt.trim().is_empty() {
        bail!("background agent prompt must not be empty");
    }

    let paths = RuntimePaths::discover()?;
    paths.ensure().await?;
    let pool = open_store(&paths).await?;
    let job_id = args.job_id.unwrap_or_else(default_job_id);
    let artifact_dir = paths.jobs_dir.join(&job_id);
    tokio::fs::create_dir_all(&artifact_dir)
        .await
        .with_context(|| format!("failed to create {}", artifact_dir.display()))?;

    let cwd = args.cwd.unwrap_or(std::env::current_dir()?);
    let spec = CodexAgentSpec {
        codex_bin: args.codex_bin,
        codex_args: args.codex_args,
        prompt,
    };
    let spec_json = serde_json::to_string_pretty(&spec)?;
    let now = now_seconds();
    sqlx::query(
        r#"
        INSERT INTO jobs (
            job_id, kind, status, cwd, spec_json, artifact_dir,
            created_at, updated_at, last_heartbeat_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?7)
        "#,
    )
    .bind(&job_id)
    .bind(JobKind::CodexAgent.as_str())
    .bind(JobStatus::Running.as_str())
    .bind(cwd.display().to_string())
    .bind(&spec_json)
    .bind(artifact_dir.display().to_string())
    .bind(now)
    .execute(&pool)
    .await
    .with_context(|| format!("failed to register CodeLink job `{job_id}`"))?;

    write_artifact(&artifact_dir.join("spec.json"), &spec_json).await?;
    append_history(
        &artifact_dir,
        &format!("{} registered background Codex agent\n", timestamp_line()),
    )
    .await?;
    spawn_worker(&job_id, &artifact_dir).await?;

    println!("STATUS: STARTED — CodeLink background agent registered");
    println!("job_id: {job_id}");
    println!("cwd: {}", cwd.display());
    println!("codex_bin: {}", spec.codex_bin);
    println!("artifact_dir: {}", artifact_dir.display());
    println!("check: codelink result {job_id}; codelink logs {job_id}");
    Ok(())
}

async fn watch_remote(args: WatchRemoteArgs) -> Result<()> {
    if args.interval_seconds == 0 {
        bail!("--interval-seconds must be greater than zero");
    }
    if args.tail_lines == 0 {
        bail!("--tail-lines must be greater than zero");
    }
    compile_optional_regex(args.success_regex.as_deref(), "--success-regex")?;
    compile_required_regex(&args.failure_regex, "--failure-regex")?;

    let paths = RuntimePaths::discover()?;
    paths.ensure().await?;
    let pool = open_store(&paths).await?;
    let job_id = args.job_id.unwrap_or_else(default_job_id);
    let artifact_dir = paths.jobs_dir.join(&job_id);
    tokio::fs::create_dir_all(&artifact_dir)
        .await
        .with_context(|| format!("failed to create {}", artifact_dir.display()))?;

    let spec = RemoteTmuxSpec {
        host: args.host,
        tmux_session: args.tmux_session,
        log_path: args.log_path,
        interval_seconds: args.interval_seconds,
        tail_lines: args.tail_lines,
        success_regex: args.success_regex,
        failure_regex: args.failure_regex,
        stall_after_seconds: args.stall_after_seconds,
        note: args.note,
    };
    let spec_json = serde_json::to_string_pretty(&spec)?;
    let now = now_seconds();
    sqlx::query(
        r#"
        INSERT INTO jobs (
            job_id, kind, status, cwd, spec_json, artifact_dir,
            created_at, updated_at, last_heartbeat_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?7)
        "#,
    )
    .bind(&job_id)
    .bind(JobKind::RemoteTmux.as_str())
    .bind(JobStatus::Running.as_str())
    .bind(std::env::current_dir()?.display().to_string())
    .bind(&spec_json)
    .bind(artifact_dir.display().to_string())
    .bind(now)
    .execute(&pool)
    .await
    .with_context(|| format!("failed to register CodeLink job `{job_id}`"))?;

    write_artifact(&artifact_dir.join("spec.json"), &spec_json).await?;
    append_history(
        &artifact_dir,
        &format!("{} registered remote tmux watcher\n", timestamp_line()),
    )
    .await?;
    spawn_worker(&job_id, &artifact_dir).await?;

    println!("STATUS: STARTED — CodeLink background watcher registered");
    println!("job_id: {job_id}");
    println!("host: {}", spec.host);
    println!("tmux_session: {}", spec.tmux_session);
    println!("remote_log: {}", spec.log_path);
    println!("artifact_dir: {}", artifact_dir.display());
    println!("check: codelink result {job_id}; codelink logs {job_id}");
    Ok(())
}

async fn spawn_worker(job_id: &str, artifact_dir: &Path) -> Result<()> {
    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let worker_log = artifact_dir.join("worker.log");
    let worker_err = artifact_dir.join("worker.err");
    let stdout = std::fs::File::create(&worker_log)
        .with_context(|| format!("failed to create {}", worker_log.display()))?;
    let stderr = std::fs::File::create(&worker_err)
        .with_context(|| format!("failed to create {}", worker_err.display()))?;
    let mut command = Command::new(&exe);
    if exe
        .file_stem()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name != "codelink")
    {
        command.arg("codelink");
    }
    command
        .arg("worker")
        .arg("--job-id")
        .arg(job_id)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    command.spawn().context("failed to spawn CodeLink worker")?;
    Ok(())
}

async fn run_worker(args: WorkerArgs) -> Result<()> {
    let paths = RuntimePaths::discover()?;
    paths.ensure().await?;
    let pool = open_store(&paths).await?;
    let job = load_job(&pool, &args.job_id).await?;
    match job.kind {
        JobKind::RemoteTmux => run_remote_tmux_worker(pool, job).await,
        JobKind::CodexAgent => run_codex_agent_worker(pool, job).await,
    }
}

async fn run_remote_tmux_worker(pool: SqlitePool, job: JobRecord) -> Result<()> {
    let spec: RemoteTmuxSpec = serde_json::from_str(&job.spec_json)
        .with_context(|| format!("failed to parse spec for job `{}`", job.job_id))?;
    let success_re = compile_optional_regex(spec.success_regex.as_deref(), "--success-regex")?;
    let failure_re = compile_required_regex(&spec.failure_regex, "--failure-regex")?;

    loop {
        let latest = load_job(&pool, &job.job_id).await?;
        if latest.status == JobStatus::Canceled {
            write_notification(&pool, &latest, JobStatus::Canceled, "canceled by user").await?;
            break;
        }
        if latest.status.is_terminal() {
            break;
        }

        let poll = poll_remote(&spec).await;
        let observed_at = now_seconds();
        match poll {
            Ok(snapshot) => {
                let analysis = analyze_snapshot(&snapshot, &success_re, &failure_re);
                let next_status = next_status(&latest, &snapshot, &analysis, &spec, observed_at);
                let summary = snapshot_summary(&snapshot, next_status, &analysis);
                persist_snapshot(
                    &pool,
                    &latest,
                    &snapshot,
                    next_status,
                    &summary,
                    observed_at,
                )
                .await?;
                if next_status.is_terminal() {
                    write_notification(&pool, &latest, next_status, &summary).await?;
                    break;
                }
            }
            Err(err) => {
                let summary = format!("remote poll failed: {err:#}");
                persist_poll_error(&pool, &latest, &summary, observed_at).await?;
            }
        }

        sleep(Duration::from_secs(spec.interval_seconds)).await;
    }

    Ok(())
}

async fn run_codex_agent_worker(pool: SqlitePool, job: JobRecord) -> Result<()> {
    let spec: CodexAgentSpec = serde_json::from_str(&job.spec_json)
        .with_context(|| format!("failed to parse spec for job `{}`", job.job_id))?;
    if job.status == JobStatus::Canceled {
        write_notification(&pool, &job, JobStatus::Canceled, "canceled before start").await?;
        return Ok(());
    }

    let stdout_path = job.artifact_dir.join("agent.stdout");
    let stderr_path = job.artifact_dir.join("agent.stderr");
    let stdout = std::fs::File::create(&stdout_path)
        .with_context(|| format!("failed to create {}", stdout_path.display()))?;
    let stderr = std::fs::File::create(&stderr_path)
        .with_context(|| format!("failed to create {}", stderr_path.display()))?;

    let mut command = Command::new(&spec.codex_bin);
    command
        .args(&spec.codex_args)
        .arg("exec")
        .arg(&spec.prompt)
        .current_dir(&job.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn `{}`", spec.codex_bin))?;
    let child_pid = child.id().map(i64::from);
    sqlx::query(
        "UPDATE jobs SET child_pid = ?1, updated_at = ?2, last_heartbeat_at = ?2, last_summary = ?3 WHERE job_id = ?4",
    )
    .bind(child_pid)
    .bind(now_seconds())
    .bind(format!("background agent pid={}", child_pid.unwrap_or(-1)))
    .bind(&job.job_id)
    .execute(&pool)
    .await?;
    append_history(
        &job.artifact_dir,
        &format!(
            "{} started background agent pid={}\n",
            timestamp_line(),
            child_pid.unwrap_or(-1)
        ),
    )
    .await?;

    let final_status = loop {
        if let Some(status) = child.try_wait()? {
            break if status.success() {
                JobStatus::Done
            } else {
                JobStatus::Failed
            };
        }
        let latest = load_job(&pool, &job.job_id).await?;
        if latest.status == JobStatus::Canceled {
            let _ = child.kill().await;
            break JobStatus::Canceled;
        }
        sqlx::query("UPDATE jobs SET updated_at = ?1, last_heartbeat_at = ?1 WHERE job_id = ?2")
            .bind(now_seconds())
            .bind(&job.job_id)
            .execute(&pool)
            .await?;
        sleep(Duration::from_secs(1)).await;
    };

    let stdout_text = tokio::fs::read_to_string(&stdout_path)
        .await
        .unwrap_or_default();
    let stderr_text = tokio::fs::read_to_string(&stderr_path)
        .await
        .unwrap_or_default();
    let result = format!(
        "# CodeLink Background Agent Result\n\nstatus: {final_status}\njob_id: {}\ncwd: {}\n\n## stdout\n\n```text\n{}\n```\n\n## stderr\n\n```text\n{}\n```\n",
        job.job_id,
        job.cwd.display(),
        trim_for_result(&stdout_text),
        trim_for_result(&stderr_text)
    );
    write_artifact(&job.artifact_dir.join("result.md"), &result).await?;
    let summary = match final_status {
        JobStatus::Done => "background agent completed".to_string(),
        JobStatus::Failed => "background agent failed".to_string(),
        JobStatus::Canceled => "background agent canceled".to_string(),
        JobStatus::Running | JobStatus::Stalled => format!("background agent {final_status}"),
    };
    sqlx::query(
        "UPDATE jobs SET status = ?1, updated_at = ?2, last_heartbeat_at = ?2, last_summary = ?3 WHERE job_id = ?4",
    )
    .bind(final_status.as_str())
    .bind(now_seconds())
    .bind(&summary)
    .bind(&job.job_id)
    .execute(&pool)
    .await?;
    append_history(
        &job.artifact_dir,
        &format!("{} {summary}\n", timestamp_line()),
    )
    .await?;
    write_notification(&pool, &job, final_status, &summary).await?;
    Ok(())
}

async fn poll_remote(spec: &RemoteTmuxSpec) -> Result<RemoteSnapshot> {
    let remote = format!(
        "tmux has-session -t {session} >/dev/null 2>&1; tmux_status=$?; echo __CODELINK_TMUX_STATUS__=$tmux_status; if [ -e {log} ]; then wc -c < {log} | awk '{{print \"__CODELINK_LOG_BYTES__=\"$1}}'; tail -n {tail} {log}; else echo __CODELINK_LOG_MISSING__=1; fi",
        session = shell_quote(&spec.tmux_session),
        log = shell_quote(&spec.log_path),
        tail = spec.tail_lines,
    );
    let output = Command::new("ssh")
        .arg(&spec.host)
        .arg(remote)
        .output()
        .await
        .with_context(|| format!("failed to run ssh {}", spec.host))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        bail!(
            "ssh exited with status {:?}: {}",
            output.status.code(),
            stderr.trim()
        );
    }
    Ok(parse_snapshot(stdout, stderr))
}

fn parse_snapshot(stdout: String, stderr: String) -> RemoteSnapshot {
    let mut tmux_running = false;
    let mut log_missing = false;
    let mut log_bytes = None;
    let mut log_lines = Vec::new();

    for line in stdout.lines() {
        if let Some(raw) = line.strip_prefix("__CODELINK_TMUX_STATUS__=") {
            tmux_running = raw.trim() == "0";
        } else if let Some(raw) = line.strip_prefix("__CODELINK_LOG_BYTES__=") {
            log_bytes = raw.trim().parse::<i64>().ok();
        } else if line == "__CODELINK_LOG_MISSING__=1" {
            log_missing = true;
        } else {
            log_lines.push(line.to_string());
        }
    }

    RemoteSnapshot {
        tmux_running,
        log_missing,
        log_bytes,
        log_tail: log_lines.join("\n"),
        stderr,
    }
}

#[derive(Debug)]
struct RemoteSnapshot {
    tmux_running: bool,
    log_missing: bool,
    log_bytes: Option<i64>,
    log_tail: String,
    stderr: String,
}

#[derive(Debug)]
struct SnapshotAnalysis {
    progress: Option<String>,
    success: bool,
    failure: bool,
}

fn analyze_snapshot(
    snapshot: &RemoteSnapshot,
    success_re: &Option<Regex>,
    failure_re: &Regex,
) -> SnapshotAnalysis {
    let success = success_re
        .as_ref()
        .is_some_and(|regex| regex.is_match(&snapshot.log_tail));
    let failure = failure_re.is_match(&snapshot.log_tail);
    SnapshotAnalysis {
        progress: parse_progress(&snapshot.log_tail),
        success,
        failure,
    }
}

fn next_status(
    latest: &JobRecord,
    snapshot: &RemoteSnapshot,
    analysis: &SnapshotAnalysis,
    spec: &RemoteTmuxSpec,
    observed_at: i64,
) -> JobStatus {
    if analysis.success {
        return JobStatus::Done;
    }
    if analysis.failure {
        return JobStatus::Failed;
    }
    if !snapshot.tmux_running {
        return JobStatus::Failed;
    }
    if let Some(prev_bytes) = latest.last_log_bytes
        && let Some(current_bytes) = snapshot.log_bytes
        && current_bytes == prev_bytes
        && let Some(changed_at) = latest.last_log_changed_at
        && spec.stall_after_seconds > 0
        && observed_at.saturating_sub(changed_at) >= spec.stall_after_seconds
    {
        return JobStatus::Stalled;
    }
    JobStatus::Running
}

fn snapshot_summary(
    snapshot: &RemoteSnapshot,
    status: JobStatus,
    analysis: &SnapshotAnalysis,
) -> String {
    let progress = analysis.progress.as_deref().unwrap_or("unknown");
    let log_bytes = snapshot
        .log_bytes
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    format!(
        "status={status}; tmux_running={}; log_missing={}; log_bytes={log_bytes}; progress={progress}",
        snapshot.tmux_running, snapshot.log_missing
    )
}

fn parse_progress(log_tail: &str) -> Option<String> {
    let regex = Regex::new(r"(?m)(\d+)\s*/\s*(\d+)(?:\s+segments)?").ok()?;
    let capture = regex.captures_iter(log_tail).last()?;
    let done = capture.get(1)?.as_str();
    let total = capture.get(2)?.as_str();
    Some(format!("{done}/{total}"))
}

async fn persist_snapshot(
    pool: &SqlitePool,
    job: &JobRecord,
    snapshot: &RemoteSnapshot,
    status: JobStatus,
    summary: &str,
    observed_at: i64,
) -> Result<()> {
    let log_tail_path = job.artifact_dir.join("log.tail");
    write_artifact(&log_tail_path, &snapshot.log_tail).await?;
    if !snapshot.stderr.trim().is_empty() {
        write_artifact(&job.artifact_dir.join("ssh.stderr"), &snapshot.stderr).await?;
    }
    append_history(
        &job.artifact_dir,
        &format!("{} {summary}\n", timestamp_line()),
    )
    .await?;
    let log_changed_at = if snapshot.log_bytes.is_some() && snapshot.log_bytes != job.last_log_bytes
    {
        Some(observed_at)
    } else {
        job.last_log_changed_at
    };
    sqlx::query(
        r#"
        UPDATE jobs
        SET status = ?1,
            updated_at = ?2,
            last_heartbeat_at = ?2,
            last_summary = ?3,
            last_log_bytes = ?4,
            last_log_changed_at = ?5
        WHERE job_id = ?6
        "#,
    )
    .bind(status.as_str())
    .bind(observed_at)
    .bind(summary)
    .bind(snapshot.log_bytes)
    .bind(log_changed_at)
    .bind(&job.job_id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn persist_poll_error(
    pool: &SqlitePool,
    job: &JobRecord,
    summary: &str,
    observed_at: i64,
) -> Result<()> {
    append_history(
        &job.artifact_dir,
        &format!("{} {summary}\n", timestamp_line()),
    )
    .await?;
    sqlx::query(
        r#"
        UPDATE jobs
        SET status = ?1, updated_at = ?2, last_heartbeat_at = ?2, last_summary = ?3
        WHERE job_id = ?4
        "#,
    )
    .bind(JobStatus::Stalled.as_str())
    .bind(observed_at)
    .bind(summary)
    .bind(&job.job_id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn list_jobs(args: JobsArgs) -> Result<()> {
    let paths = RuntimePaths::discover()?;
    paths.ensure().await?;
    let pool = open_store(&paths).await?;
    let rows = if args.all {
        sqlx::query(
            "SELECT job_id, kind, status, updated_at, last_summary FROM jobs ORDER BY updated_at DESC",
        )
        .fetch_all(&pool)
        .await?
    } else {
        sqlx::query(
            "SELECT job_id, kind, status, updated_at, last_summary FROM jobs WHERE status IN ('running', 'stalled') ORDER BY updated_at DESC",
        )
        .fetch_all(&pool)
        .await?
    };
    if rows.is_empty() {
        println!("No CodeLink jobs.");
        return Ok(());
    }
    println!("job_id\tkind\tstatus\tupdated_at\tsummary");
    for row in rows {
        let job_id: String = row.try_get("job_id")?;
        let kind: String = row.try_get("kind")?;
        let status: String = row.try_get("status")?;
        let updated_at: i64 = row.try_get("updated_at")?;
        let summary: Option<String> = row.try_get("last_summary")?;
        println!(
            "{job_id}\t{kind}\t{status}\t{updated_at}\t{}",
            summary.unwrap_or_default()
        );
    }
    Ok(())
}

async fn print_result(args: JobIdArgs) -> Result<()> {
    let paths = RuntimePaths::discover()?;
    paths.ensure().await?;
    let pool = open_store(&paths).await?;
    let job = load_job(&pool, &args.job_id).await?;
    println!("job_id: {}", job.job_id);
    println!("kind: {}", job.kind.as_str());
    println!("status: {}", job.status);
    println!("cwd: {}", job.cwd.display());
    if let Some(child_pid) = job.child_pid {
        println!("child_pid: {child_pid}");
    }
    println!("artifact_dir: {}", job.artifact_dir.display());
    if let Some(summary) = &job.last_summary {
        println!("summary: {summary}");
    }
    let result = job.artifact_dir.join("result.md");
    if result.exists() {
        println!();
        println!("{}", tokio::fs::read_to_string(result).await?);
    }
    let notification = job.artifact_dir.join("notification.md");
    if notification.exists() {
        println!();
        println!("{}", tokio::fs::read_to_string(notification).await?);
    }
    Ok(())
}

async fn print_logs(args: JobIdArgs) -> Result<()> {
    let paths = RuntimePaths::discover()?;
    paths.ensure().await?;
    let pool = open_store(&paths).await?;
    let job = load_job(&pool, &args.job_id).await?;
    for name in [
        "history.log",
        "log.tail",
        "agent.stdout",
        "agent.stderr",
        "worker.err",
    ] {
        let path = job.artifact_dir.join(name);
        if path.exists() {
            println!("==> {} <==", path.display());
            println!("{}", tokio::fs::read_to_string(path).await?);
        }
    }
    Ok(())
}

async fn print_notifications(args: NotificationsArgs) -> Result<()> {
    let paths = RuntimePaths::discover()?;
    paths.ensure().await?;
    let pool = open_store(&paths).await?;
    let rows = load_notification_rows(&pool, args.all).await?;
    if rows.is_empty() {
        println!("No CodeLink notifications.");
        return Ok(());
    }
    for row in rows {
        let job_id: String = row.try_get("job_id")?;
        let path: String = row.try_get("notification_path")?;
        println!("==> job {job_id}: {path} <==");
        println!("{}", tokio::fs::read_to_string(&path).await?);
        if !args.all {
            sqlx::query("UPDATE notifications SET read_at = ?1 WHERE job_id = ?2")
                .bind(now_seconds())
                .bind(job_id)
                .execute(&pool)
                .await?;
        }
    }
    Ok(())
}

pub async fn drain_unread_notifications() -> Result<Vec<CodeLinkNotification>> {
    let paths = RuntimePaths::discover()?;
    if !paths.db_path.exists() {
        return Ok(Vec::new());
    }
    let pool = open_existing_store(&paths).await?;
    let rows = load_notification_rows(&pool, /*include_read*/ false).await?;
    let mut notifications = Vec::new();
    for row in rows {
        let job_id: String = row.try_get("job_id")?;
        let path = PathBuf::from(row.try_get::<String, _>("notification_path")?);
        let content = tokio::fs::read_to_string(&path).await.with_context(|| {
            format!(
                "failed to read CodeLink notification for job `{job_id}` at {}",
                path.display()
            )
        })?;
        sqlx::query("UPDATE notifications SET read_at = ?1 WHERE job_id = ?2")
            .bind(now_seconds())
            .bind(&job_id)
            .execute(&pool)
            .await?;
        notifications.push(CodeLinkNotification {
            job_id,
            notification_path: path,
            content,
        });
    }
    Ok(notifications)
}

pub async fn active_jobs() -> Result<Vec<CodeLinkJobSummary>> {
    let paths = RuntimePaths::discover()?;
    if !paths.db_path.exists() {
        return Ok(Vec::new());
    }
    let pool = open_existing_store(&paths).await?;
    let rows = sqlx::query(
        "SELECT job_id, kind, status, artifact_dir, last_summary FROM jobs WHERE status IN ('running', 'stalled') ORDER BY updated_at DESC",
    )
    .fetch_all(&pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let artifact_dir: String = row.try_get("artifact_dir")?;
            Ok(CodeLinkJobSummary {
                job_id: row.try_get("job_id")?,
                kind: row.try_get("kind")?,
                status: row.try_get("status")?,
                artifact_dir: PathBuf::from(artifact_dir),
                last_summary: row.try_get("last_summary")?,
            })
        })
        .collect()
}

async fn cancel_job(args: JobIdArgs) -> Result<()> {
    let paths = RuntimePaths::discover()?;
    paths.ensure().await?;
    let pool = open_store(&paths).await?;
    let job = load_job(&pool, &args.job_id).await?;
    sqlx::query("UPDATE jobs SET status = ?1, updated_at = ?2 WHERE job_id = ?3")
        .bind(JobStatus::Canceled.as_str())
        .bind(now_seconds())
        .bind(&job.job_id)
        .execute(&pool)
        .await?;
    append_history(
        &job.artifact_dir,
        &format!("{} canceled by user\n", timestamp_line()),
    )
    .await?;
    println!("STATUS: CANCELED — CodeLink job {}", job.job_id);
    Ok(())
}

async fn load_notification_rows(
    pool: &SqlitePool,
    include_read: bool,
) -> Result<Vec<sqlx::sqlite::SqliteRow>> {
    if include_read {
        sqlx::query("SELECT job_id, notification_path FROM notifications ORDER BY created_at DESC")
            .fetch_all(pool)
            .await
            .map_err(Into::into)
    } else {
        sqlx::query(
            "SELECT job_id, notification_path FROM notifications WHERE read_at IS NULL ORDER BY created_at DESC",
        )
        .fetch_all(pool)
        .await
        .map_err(Into::into)
    }
}

async fn write_notification(
    pool: &SqlitePool,
    job: &JobRecord,
    status: JobStatus,
    summary: &str,
) -> Result<()> {
    let path = job.artifact_dir.join("notification.md");
    let content = format!(
        "[CodeLink] job {} {}\nsummary: {}\nartifact_dir: {}\n",
        job.job_id,
        status,
        summary,
        job.artifact_dir.display()
    );
    write_artifact(&path, &content).await?;
    sqlx::query(
        r#"
        INSERT INTO notifications (job_id, created_at, notification_path)
        VALUES (?1, ?2, ?3)
        ON CONFLICT(job_id) DO UPDATE SET
            created_at = excluded.created_at,
            notification_path = excluded.notification_path,
            read_at = NULL
        "#,
    )
    .bind(&job.job_id)
    .bind(now_seconds())
    .bind(path.display().to_string())
    .execute(pool)
    .await?;
    Ok(())
}

async fn load_job(pool: &SqlitePool, job_id: &str) -> Result<JobRecord> {
    let row = sqlx::query(
        "SELECT job_id, kind, status, cwd, spec_json, artifact_dir, last_summary, last_log_bytes, last_log_changed_at, child_pid FROM jobs WHERE job_id = ?1",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await?
    .with_context(|| format!("CodeLink job `{job_id}` not found"))?;
    let status: String = row.try_get("status")?;
    let kind: String = row.try_get("kind")?;
    let artifact_dir: String = row.try_get("artifact_dir")?;
    let cwd: String = row.try_get("cwd")?;
    Ok(JobRecord {
        job_id: row.try_get("job_id")?,
        kind: JobKind::from_db(&kind)?,
        status: JobStatus::from_db(&status)?,
        cwd: PathBuf::from(cwd),
        spec_json: row.try_get("spec_json")?,
        artifact_dir: PathBuf::from(artifact_dir),
        last_summary: row.try_get("last_summary")?,
        last_log_bytes: row.try_get("last_log_bytes")?,
        last_log_changed_at: row.try_get("last_log_changed_at")?,
        child_pid: row.try_get("child_pid")?,
    })
}

async fn open_store(paths: &RuntimePaths) -> Result<SqlitePool> {
    paths.ensure().await?;
    let url = format!("sqlite://{}?mode=rwc", paths.db_path.display());
    let pool = SqlitePool::connect(&url)
        .await
        .with_context(|| format!("failed to open {}", paths.db_path.display()))?;
    initialize_schema(&pool).await?;
    Ok(pool)
}

async fn open_existing_store(paths: &RuntimePaths) -> Result<SqlitePool> {
    let url = format!("sqlite://{}?mode=rw", paths.db_path.display());
    let pool = SqlitePool::connect(&url)
        .await
        .with_context(|| format!("failed to open {}", paths.db_path.display()))?;
    initialize_schema(&pool).await?;
    Ok(pool)
}

async fn initialize_schema(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS jobs (
            job_id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            status TEXT NOT NULL,
            cwd TEXT NOT NULL,
            spec_json TEXT NOT NULL,
            artifact_dir TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            last_heartbeat_at INTEGER,
            last_summary TEXT,
            last_log_bytes INTEGER,
            last_log_changed_at INTEGER,
            child_pid INTEGER
        )
        "#,
    )
    .execute(pool)
    .await?;
    try_add_jobs_column(pool, "last_log_bytes INTEGER").await?;
    try_add_jobs_column(pool, "last_log_changed_at INTEGER").await?;
    try_add_jobs_column(pool, "child_pid INTEGER").await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS notifications (
            job_id TEXT PRIMARY KEY,
            created_at INTEGER NOT NULL,
            read_at INTEGER,
            notification_path TEXT NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn try_add_jobs_column(pool: &SqlitePool, column_sql: &str) -> Result<()> {
    let sql = format!("ALTER TABLE jobs ADD COLUMN {column_sql}");
    let result = sqlx::query(&sql).execute(pool).await;
    match result {
        Ok(_) => Ok(()),
        Err(err) if err.to_string().contains("duplicate column name") => Ok(()),
        Err(err) => Err(err.into()),
    }
}

async fn write_artifact(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, content)
        .await
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

async fn append_history(artifact_dir: &Path, content: &str) -> Result<()> {
    let path = artifact_dir.join("history.log");
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(content.as_bytes()).await?;
    Ok(())
}

fn compile_optional_regex(raw: Option<&str>, name: &str) -> Result<Option<Regex>> {
    raw.map(|value| Regex::new(value).with_context(|| format!("invalid {name}: {value}")))
        .transpose()
}

fn compile_required_regex(raw: &str, name: &str) -> Result<Regex> {
    Regex::new(raw).with_context(|| format!("invalid {name}: {raw}"))
}

fn shell_quote(value: &str) -> String {
    shlex::try_quote(value)
        .map(|value| value.into_owned())
        .unwrap_or_else(|_| "''".to_string())
}

fn default_job_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = JOB_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("job-{nanos}-{}-{sequence}", std::process::id())
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn timestamp_line() -> String {
    format!("[{}]", now_seconds())
}

fn trim_for_result(value: &str) -> String {
    const MAX_CHARS: usize = 16_000;
    if value.chars().count() <= MAX_CHARS {
        return value.to_string();
    }
    let tail = value
        .chars()
        .rev()
        .take(MAX_CHARS)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("[truncated to last {MAX_CHARS} chars]\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parse_progress_uses_latest_segment_count() {
        let log = "progress: 100 / 200 segments\nlater 1817 / 20656 segments";
        assert_eq!(parse_progress(log), Some("1817/20656".to_string()));
    }

    #[test]
    fn parse_snapshot_extracts_markers_and_tail() {
        let snapshot = parse_snapshot(
            "__CODELINK_TMUX_STATUS__=0\n__CODELINK_LOG_BYTES__=123\nhello\nworld\n".to_string(),
            String::new(),
        );
        assert_eq!(snapshot.tmux_running, true);
        assert_eq!(snapshot.log_bytes, Some(123));
        assert_eq!(snapshot.log_tail, "hello\nworld");
    }

    #[test]
    fn default_job_ids_do_not_collide_in_a_burst() {
        let ids = (0..1000)
            .map(|_| default_job_id())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 1000);
    }

    #[tokio::test]
    async fn concurrent_codex_agent_workers_keep_artifacts_isolated() -> Result<()> {
        let temp_home = tempfile::tempdir()?;
        let paths = RuntimePaths {
            jobs_dir: temp_home.path().join("jobs"),
            db_path: temp_home.path().join("jobs.sqlite"),
        };
        paths.ensure().await?;
        let pool = open_store(&paths).await?;

        register_test_codex_agent(&pool, &paths, "agent-one", "alpha prompt").await?;
        register_test_codex_agent(&pool, &paths, "agent-two", "beta prompt").await?;

        let job_one = load_job(&pool, "agent-one").await?;
        let job_two = load_job(&pool, "agent-two").await?;
        tokio::try_join!(
            run_codex_agent_worker(pool.clone(), job_one),
            run_codex_agent_worker(pool.clone(), job_two)
        )?;

        let job_one = load_job(&pool, "agent-one").await?;
        let job_two = load_job(&pool, "agent-two").await?;
        assert_eq!(job_one.status, JobStatus::Done);
        assert_eq!(job_two.status, JobStatus::Done);

        let result_one = tokio::fs::read_to_string(job_one.artifact_dir.join("result.md")).await?;
        let result_two = tokio::fs::read_to_string(job_two.artifact_dir.join("result.md")).await?;
        assert!(result_one.contains("exec alpha prompt"));
        assert!(!result_one.contains("beta prompt"));
        assert!(result_two.contains("exec beta prompt"));
        assert!(!result_two.contains("alpha prompt"));

        let notification_one =
            tokio::fs::read_to_string(job_one.artifact_dir.join("notification.md")).await?;
        let notification_two =
            tokio::fs::read_to_string(job_two.artifact_dir.join("notification.md")).await?;
        assert!(notification_one.contains("[CodeLink] job agent-one done"));
        assert!(notification_two.contains("[CodeLink] job agent-two done"));

        let notification_rows = sqlx::query("SELECT job_id FROM notifications ORDER BY job_id")
            .fetch_all(&pool)
            .await?;
        let job_ids = notification_rows
            .iter()
            .map(|row| row.try_get::<String, _>("job_id"))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(job_ids, vec!["agent-one", "agent-two"]);

        Ok(())
    }

    #[tokio::test]
    async fn drain_unread_notifications_reads_files_and_marks_them_read() -> Result<()> {
        let temp_home = tempfile::tempdir()?;
        let paths = RuntimePaths {
            jobs_dir: temp_home.path().join("jobs"),
            db_path: temp_home.path().join("jobs.sqlite"),
        };
        paths.ensure().await?;
        let pool = open_store(&paths).await?;

        register_test_codex_agent(&pool, &paths, "notify-one", "notify prompt").await?;
        let job = load_job(&pool, "notify-one").await?;
        write_notification(&pool, &job, JobStatus::Done, "test summary").await?;

        let previous = std::env::var("CODELINK_HOME").ok();
        unsafe { std::env::set_var("CODELINK_HOME", temp_home.path()) };

        let first = drain_unread_notifications().await?;
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].job_id, "notify-one");
        assert!(first[0].content.contains("[CodeLink] job notify-one done"));
        assert_eq!(
            first[0].notification_path,
            paths.jobs_dir.join("notify-one").join("notification.md")
        );

        let second = drain_unread_notifications().await?;
        assert!(second.is_empty());

        match previous {
            Some(value) => unsafe { std::env::set_var("CODELINK_HOME", value) },
            None => unsafe { std::env::remove_var("CODELINK_HOME") },
        }

        Ok(())
    }

    #[tokio::test]
    async fn active_jobs_returns_only_running_or_stalled_jobs() -> Result<()> {
        let temp_home = tempfile::tempdir()?;
        let paths = RuntimePaths {
            jobs_dir: temp_home.path().join("jobs"),
            db_path: temp_home.path().join("jobs.sqlite"),
        };
        paths.ensure().await?;
        let pool = open_store(&paths).await?;

        register_test_codex_agent(&pool, &paths, "running-job", "running prompt").await?;
        register_test_codex_agent(&pool, &paths, "done-job", "done prompt").await?;
        sqlx::query("UPDATE jobs SET status = ?1 WHERE job_id = ?2")
            .bind(JobStatus::Done.as_str())
            .bind("done-job")
            .execute(&pool)
            .await?;

        let previous = std::env::var("CODELINK_HOME").ok();
        unsafe { std::env::set_var("CODELINK_HOME", temp_home.path()) };

        let jobs = active_jobs().await?;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_id, "running-job");
        assert_eq!(jobs[0].kind, "codex_agent");
        assert_eq!(jobs[0].status, "running");

        match previous {
            Some(value) => unsafe { std::env::set_var("CODELINK_HOME", value) },
            None => unsafe { std::env::remove_var("CODELINK_HOME") },
        }

        Ok(())
    }

    async fn register_test_codex_agent(
        pool: &SqlitePool,
        paths: &RuntimePaths,
        job_id: &str,
        prompt: &str,
    ) -> Result<()> {
        let artifact_dir = paths.jobs_dir.join(job_id);
        tokio::fs::create_dir_all(&artifact_dir).await?;
        let spec = CodexAgentSpec {
            codex_bin: "/bin/echo".to_string(),
            codex_args: Vec::new(),
            prompt: prompt.to_string(),
        };
        let spec_json = serde_json::to_string_pretty(&spec)?;
        let now = now_seconds();
        sqlx::query(
            r#"
            INSERT INTO jobs (
                job_id, kind, status, cwd, spec_json, artifact_dir,
                created_at, updated_at, last_heartbeat_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?7)
            "#,
        )
        .bind(job_id)
        .bind(JobKind::CodexAgent.as_str())
        .bind(JobStatus::Running.as_str())
        .bind(std::env::current_dir()?.display().to_string())
        .bind(&spec_json)
        .bind(artifact_dir.display().to_string())
        .bind(now)
        .execute(pool)
        .await?;
        write_artifact(&artifact_dir.join("spec.json"), &spec_json).await?;
        Ok(())
    }
}
