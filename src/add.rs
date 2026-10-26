use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::mito::Mito;
use crate::worktree::{self, Source};

/// Create a worktree by snapshotting the base.
///
/// Takes `git worktree add`'s options and resolves them the same way. A
/// <commit-ish> other than the base's checks out the difference from it.
#[derive(clap::Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent `git worktree add` flag"
)]
pub struct AddArgs {
    /// Create a new branch named <new-branch> starting at <commit-ish>.
    #[arg(short = 'b', value_name = "new-branch", conflicts_with_all = ["force_branch", "detach"])]
    new_branch: Option<String>,
    /// Like -b, but reset <new-branch> to <commit-ish> if it exists.
    #[arg(short = 'B', value_name = "new-branch", conflicts_with = "detach")]
    force_branch: Option<String>,
    /// Detach HEAD in the new worktree.
    #[arg(short, long)]
    detach: bool,
    /// Check out a branch even if another worktree has it checked out.
    #[arg(short, long)]
    force: bool,
    /// Keep the new worktree locked after creation.
    #[arg(long)]
    lock: bool,
    /// Reason for --lock.
    #[arg(long, requires = "lock")]
    reason: Option<String>,
    /// Set up tracking for a new branch (see git-branch(1)).
    #[arg(long, overrides_with = "no_track")]
    track: bool,
    #[arg(long, overrides_with = "track")]
    no_track: bool,
    path: PathBuf,
    #[arg(value_name = "commit-ish")]
    commitish: Option<String>,
}

pub fn run(mito: &Mito, args: AddArgs) -> Result<()> {
    // Run git where the user ran mito, so HEAD means what it would for git.
    let cwd = std::env::current_dir()?;
    let path = std::path::absolute(&args.path)?;
    let guard = mito.lock_base()?;
    let Some(base) = guard.base.as_deref() else {
        bail!("there is no base worktree yet; run `git mito update` first");
    };
    let source = Source::open(base)?;

    let mut git_args: Vec<OsString> = Vec::new();
    let mut flag = |f: &str| git_args.push(f.into());
    if args.force {
        flag("--force");
    }
    if args.lock {
        flag("--lock");
    }
    if let Some(reason) = &args.reason {
        flag("--reason");
        flag(reason);
    }
    if args.track {
        flag("--track");
    }
    if args.no_track {
        flag("--no-track");
    }
    // git resolves HEAD and picks or creates the branch itself, as it would
    // for `git worktree add`.
    if let Some(branch) = &args.new_branch {
        flag("-b");
        flag(branch);
    } else if let Some(branch) = &args.force_branch {
        flag("-B");
        flag(branch);
    } else if args.detach {
        flag("--detach");
    }
    git_args.push(path.clone().into());
    git_args.extend(args.commitish.map(OsString::from));

    let mut pending = worktree::clone(&mito.repo, &cwd, &git_args, &path, Some(&source), true)?;
    drop(guard);
    let head = pending.checkout()?;
    pending.keep()?;
    // As `git worktree add` reports its checkout.
    let commit = mito.repo.find_commit(head)?;
    let summary = commit.message()?.summary();
    println!("HEAD is now at {} {summary}", commit.id().shorten_or_id());
    Ok(())
}
