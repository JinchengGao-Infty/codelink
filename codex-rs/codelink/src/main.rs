use clap::Parser;
use codex_codelink::Cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    codex_codelink::run_main(Cli::parse()).await
}
