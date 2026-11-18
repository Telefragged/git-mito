use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use clap_complete::ArgValueCompleter;

use crate::mito::Mito;
use crate::remove::complete_worktree;
use crate::worktree::{self, Source, SourceKind};

/// Create a worktree by snapshotting the base.
///
/// Takes `git worktree add`'s options and resolves them the same way. A
/// <commit-ish> other than the base's checks out the difference from it.
///
/// With --from, snapshots another worktree that git mito added instead, with
/// its uncommitted changes and its untracked and ignored files. It must not
/// be in the middle of a merge, rebase, cherry-pick, revert or bisect.
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
    /// Snapshot <worktree>, a worktree git mito added, instead of the base.
    /// The new worktree starts at its HEAD, on a new branch named after
    /// <path> unless -b, -B or --detach is given.
    #[arg(
        long,
        value_name = "worktree",
        conflicts_with = "commitish",
        add = ArgValueCompleter::new(complete_worktree),
    )]
    from: Option<PathBuf>,
    path: PathBuf,
    #[arg(value_name = "commit-ish")]
    commitish: Option<String>,
}

pub fn run(mito: &Mito, args: AddArgs) -> Result<()> {
    // Run git where the user ran mito, so HEAD means what it would for git.
    let cwd = std::env::current_dir()?;
    let path = std::path::absolute(&args.path)?;
    let from = args
        .from
        .as_deref()
        .map(|from| {
            from.canonicalize()
                .with_context(|| format!("{} is not a worktree", from.display()))
        })
        .transpose()?;
    let base_lock = from.is_none().then(|| mito.lock_base()).transpose()?;
    let source = match &from {
        Some(from) => Source::open_worktree(from, open_linked_worktree(mito, from)?)?,
        None => Source::open_base(
            base_lock
                .as_ref()
                .and_then(|lock| lock.base.as_deref())
                .context("there is no base worktree yet; run `git mito update` first")?,
        )?,
    };
    let from_commit = (source.kind == SourceKind::Worktree).then_some(source.commit);

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
    } else if from_commit.is_some() {
        // Without a <commit-ish>, git would check out an existing branch
        // named after <path>, and the reset to its commit would discard the
        // copied changes.
        flag("-b");
        git_args.push(path.file_name().context("<path> has no file name")?.into());
    }
    git_args.push(path.clone().into());
    git_args.extend(args.commitish.map(OsString::from));
    git_args.extend(from_commit.map(|commit| commit.to_string().into()));

    let mut pending = worktree::clone(&mito.repo, &cwd, &git_args, &path, Some(&source), true)?;
    drop(base_lock);
    let head = pending.checkout()?;
    pending.keep()?;
    // As `git worktree add` reports its checkout.
    let commit = mito.repo.find_commit(head)?;
    let summary = commit.message()?.summary();
    println!("HEAD is now at {} {summary}", commit.id().shorten_or_id());
    Ok(())
}

fn open_linked_worktree(mito: &Mito, path: &Path) -> Result<gix::Repository> {
    let not_linked = || anyhow!("{} is not a linked worktree", path.display());
    let repo = gix::open(path).map_err(|_| not_linked())?;
    let same_repo = repo.common_dir().canonicalize()? == mito.main.git_dir().canonicalize()?;
    if !same_repo || repo.worktree().is_none_or(|wt| wt.is_main()) {
        return Err(not_linked());
    }
    Ok(repo)
}
