mod jj;
mod stack;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use crate::{jj::Jj, stack::Log};

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
struct SubmitArgs {}

// TODO: async
// TODO: planner: rebase bookmark onto base
// TODO: planner: push bookmark
// TODO: planner: dry-run
// TODO: planner: create pr
// TODO: planner: set base
// TODO: planner: create stack comment
// TODO: planner: update base
// TODO: planner: update stack comment
// TODO: custom colored error reporting with owo-colors
// TODO: custom remote
// TODO: custom base
// TODO: status command
// TODO: interactive stack picker
fn main() -> Result<()> {
    let args = Cli::parse();

    let root = if let Some(path) = args.path {
        path
    } else {
        std::env::current_dir().context("failed to get cwd")?
    };

    match args.command {
        Commands::Submit(args) => cmd_submit(root, args),
    }
}

fn submit(root: PathBuf, _args: SubmitArgs) -> Result<()> {
    let jj = Jj::new(&root)?;
    let repo = jj.repo()?;

    let log = Log::new(&jj, "trunk()::")?;

    println!(
        "The following stack will be submitted:\n{}",
        log.display(repo.as_ref())
    );
    let confirmation = dialoguer::Confirm::new()
        .with_prompt("Submit?")
        .interact()?;

    if !confirmation {
        return Ok(());
    }

    println!("Submitting...");

    Ok(())
}
