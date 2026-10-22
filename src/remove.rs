use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::git;
use crate::mito::Mito;
use crate::worktree;

/// Remove a worktree, deleting its subvolume in one step where permitted.
///
/// `git worktree remove` works on mito worktrees too, but deletes them file
/// by file.
#[derive(clap::Args)]
pub struct RemoveArgs {
    /// Remove even if the worktree is dirty or locked.
    #[arg(short, long)]
    force: bool,
    worktree: PathBuf,
}

pub fn run(mito: &Mito, args: &RemoveArgs) -> Result<()> {
    let path = args
        .worktree
        .canonicalize()
        .with_context(|| format!("{} is not a worktree", args.worktree.display()))?;
    if path.starts_with(mito.bases_dir().canonicalize().unwrap_or_default()) {
        bail!("{} is a base managed by `git mito update`", path.display());
    }
    let worktree = mito
        .repo
        .worktrees()?
        .into_iter()
        .find(|wt| wt.base().ok().and_then(|p| p.canonicalize().ok()).as_ref() == Some(&path))
        .with_context(|| format!("{} is not a linked worktree", path.display()))?;

    if !args.force {
        if worktree.is_locked() {
            let reason = match worktree.lock_reason() {
                Some(reason) if !reason.is_empty() => format!(" ({reason})"),
                _ => String::new(),
            };
            bail!(
                "{} is locked{reason}; use --force to remove it anyway",
                path.display()
            );
        }
        if !git::run(&path, ["status", "--porcelain"])?.is_empty() {
            bail!(
                "{} contains modified or untracked files; use --force to remove it anyway",
                path.display()
            );
        }
    }

    worktree::destroy(&mito.root, &path)?;
    println!("Removed worktree {}", path.display());
    Ok(())
}
