mod jj;
mod log;
mod submit;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use crate::{jj::Jj, log::Log, submit::submit};

#[derive(Parser, Debug)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Sets the JJ workspace root
    #[arg(short, long, global = true, value_name = "DIR")]
    path: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Submit(SubmitArgs),
}

#[derive(Args, Debug)]
struct SubmitArgs {
    /// The base revset to submit against, must resolve to a single bookmark that has a remote
    #[arg(short, long, value_name = "REVSET")]
    base: Option<String>,

    /// Do not actually submit, just print the submission plan
    #[arg(long, default_value_t = false)]
    dry_run: bool,

    /// Skip confirmation
    #[arg(long, default_value_t = false)]
    no_confirm: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let Cli { command, path } = Cli::parse();

    let root = if let Some(path) = path {
        path
    } else {
        std::env::current_dir().context("failed to get cwd")?
    };

    match command {
        Commands::Submit(args) => cmd_submit(root, args).await,
    }
}

async fn cmd_submit(root: PathBuf, args: SubmitArgs) -> Result<()> {
    let jj = Jj::new(&root)?;
    let repo = jj.repo().await?;

    let log = Log::new(&jj, &args.base.unwrap_or_else(|| "trunk()".to_string())).await?;

    eprintln!(
        "The following stack will be submitted:\n{}",
        log.display(repo.as_ref())
    );

    if !args.dry_run && !args.no_confirm {
        let confirmed = dialoguer::Confirm::new()
            .with_prompt("Submit?")
            .interact()?;

        if !confirmed {
            return Ok(());
        }
    }

    submit(&log, repo.as_ref(), args.dry_run).await?;

    todo!("actually submit");
}
