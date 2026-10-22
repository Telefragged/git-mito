use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::btrfs;
use crate::git;
use crate::git::short;
use crate::mito::Mito;
use crate::worktree::{self, Source};

/// Move the base to the tracked revision, creating it if needed.
///
/// The new base is built beside the current one and swapped in atomically,
/// so `git mito add` keeps working throughout.
#[derive(clap::Args)]
pub struct UpdateArgs {
    /// Rebuild the base even if it is already at the tracked revision.
    #[arg(short, long)]
    force: bool,
}

pub fn run(mito: &Mito, args: &UpdateArgs) -> Result<()> {
    fs::create_dir_all(mito.bases_dir())?;
    btrfs::ensure_btrfs(&mito.dir)?;
    mito.ensure_excluded()?;
    let _update = mito.lock_update()?;
    mito.collect_garbage()?;

    let target = resolve_track(mito)?;
    let current = mito.current_base()?;
    let source = current.as_deref().map(Source::open).transpose()?;
    if let Some(source) = &source
        && source.commit == target
        && !args.force
    {
        println!("Base is already at {}", short(&target));
        return Ok(());
    }

    let id = unused_id(mito, &short(&target));
    let path = mito.bases_dir().join(&id);
    // Locked, so that `git worktree remove` and `prune` leave it alone.
    let git_args: Vec<OsString> = ["--detach", "--lock", "--reason", "managed by git-mito"]
        .map(OsString::from)
        .into_iter()
        .chain([path.clone().into(), target.to_string().into()])
        .collect();
    worktree::clone(&mito.repo, &mito.root, &git_args, &path, source.as_ref())?.checkout()?;

    if let Some(command) = &mito.config.base.warm
        && let Err(e) = warm(command, &path)
    {
        let _ = worktree::destroy(&mito.root, &path);
        return Err(e);
    }

    // A published base must never change: its files, index and HEAD have to
    // keep agreeing for every worktree cloned from it.
    btrfs::set_read_only(&path, true)?;
    {
        let _lock = mito.lock_base(true)?;
        mito.publish_base(&id)?;
    }
    let message = format!("mito: update base to {}", short(&target));
    mito.repo.reference(
        "refs/heads/mito/base",
        target,
        gix::refs::transaction::PreviousValue::Any,
        message,
    )?;
    println!("Base updated to {} at {}", short(&target), path.display());

    // Nothing can be reading the old base any more.
    mito.collect_garbage()
}

/// Runs the warm-up `command` in the new base at `path`. The base is a
/// snapshot of the previous one, so whatever the last warm-up left behind is
/// there to build on.
fn warm(command: &str, path: &Path) -> Result<()> {
    eprintln!("Warming the new base: {command}");
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command).current_dir(path);
    git::scrub_env(&mut cmd);
    let status = cmd.status().context("running the warm-up command")?;
    if !status.success() {
        bail!("the warm-up command failed ({status}); the base was left as it was");
    }
    Ok(())
}

/// Resolves the revision the base follows: `base.track` from the config,
/// else `origin/HEAD`, else the main worktree's HEAD.
fn resolve_track(mito: &Mito) -> Result<gix::ObjectId> {
    match &mito.config.base.track {
        Some(rev) => git::resolve_commit(&mito.repo, rev),
        None => git::resolve_commit(&mito.repo, "origin/HEAD")
            .or_else(|_| git::resolve_commit(&mito.main, "HEAD")),
    }
}

/// `stem`, suffixed with `.1`, `.2`, … if a base already uses it, as when
/// `--force` rebuilds the current commit.
fn unused_id(mito: &Mito, stem: &str) -> String {
    let mut id = stem.to_owned();
    let mut n = 1;
    while mito.bases_dir().join(&id).exists() {
        id = format!("{stem}.{n}");
        n += 1;
    }
    id
}
