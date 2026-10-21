//! git-mito: git worktrees as btrfs snapshots of a maintained base worktree.

mod add;
mod btrfs;
mod config;
mod git;
mod mito;
mod remove;
mod update;
mod worktree;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "git mito", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Add(add::AddArgs),
    Update(update::UpdateArgs),
    Remove(remove::RemoveArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = mito::Mito::discover().and_then(|mito| match cli.command {
        Command::Add(args) => add::run(&mito, args),
        Command::Update(args) => update::run(&mito, args),
        Command::Remove(args) => remove::run(&mito, args),
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("git-mito: {e:#}");
            ExitCode::FAILURE
        }
    }
}
