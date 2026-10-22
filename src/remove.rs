use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap_complete::{ArgValueCompleter, CompletionCandidate};

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
    #[arg(add = ArgValueCompleter::new(complete_worktree))]
    worktree: PathBuf,
}

pub fn run(mito: &Mito, args: &RemoveArgs) -> Result<()> {
    let path = args
        .worktree
        .canonicalize()
        .with_context(|| format!("{} is not a worktree", args.worktree.display()))?;
    if is_base(mito, &path) {
        bail!("{} is a base managed by `git mito update`", path.display());
    }
    let (_, worktree) = linked_worktrees(mito)?
        .find(|(p, _)| *p == path)
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

/// The linked worktrees whose directories exist, by canonical path.
fn linked_worktrees(
    mito: &Mito,
) -> Result<impl Iterator<Item = (PathBuf, gix::worktree::Proxy<'_>)>> {
    Ok(mito
        .repo
        .worktrees()?
        .into_iter()
        .filter_map(|wt| Some((wt.base().ok()?.canonicalize().ok()?, wt))))
}

fn is_base(mito: &Mito, path: &Path) -> bool {
    path.starts_with(mito.bases_dir().canonicalize().unwrap_or_default())
}

/// Completes the worktrees `remove` takes: every linked one except the bases.
/// Paths are relative to the current directory unless an absolute one is
/// being typed, and each comes with its branch as help.
fn complete_worktree(current: &OsStr) -> Vec<CompletionCandidate> {
    worktree_candidates(current).unwrap_or_default()
}

fn worktree_candidates(current: &OsStr) -> Option<Vec<CompletionCandidate>> {
    let mito = Mito::discover().ok()?;
    let cwd = std::env::current_dir().ok()?;
    let absolute = Path::new(current).is_absolute();
    let mut candidates: Vec<_> = linked_worktrees(&mito)
        .ok()?
        .filter(|(path, _)| !is_base(&mito, path))
        .filter_map(|(path, wt)| {
            // Both are canonical, so relative to `/` they hold only the
            // plain components the function requires.
            let shown = if absolute {
                path
            } else {
                gix::path::relativize_with_prefix(
                    path.strip_prefix("/").ok()?,
                    cwd.strip_prefix("/").ok()?,
                )
                .into_owned()
            };
            if !shown
                .as_os_str()
                .as_encoded_bytes()
                .starts_with(current.as_encoded_bytes())
            {
                return None;
            }
            let branch = wt
                .into_repo_with_possibly_inaccessible_worktree()
                .ok()
                .and_then(|repo| repo.head_name().ok().flatten())
                .map(|name| name.shorten().to_string().into());
            Some(CompletionCandidate::new(shown).help(branch))
        })
        .collect();
    candidates.sort_by(|a, b| a.get_value().cmp(b.get_value()));
    Some(candidates)
}
