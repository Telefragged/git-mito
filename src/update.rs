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

    let target = resolve_track(mito)?;
    let short_target = short(&target);
    // Held while the current base is read and copied, as in `add`.
    let guard = mito.lock_base()?;
    let current = guard.base.clone();
    let source = guard.base.as_deref().map(Source::open).transpose()?;
    let current_commit = source.as_ref().map(|s| s.commit);
    if current_commit == Some(target) && !args.force {
        println!("Base is already at {short_target}");
        return Ok(());
    }

    let id = unused_id(mito, &short_target);
    let path = mito.bases_dir().join(&id);
    // Locked, so that `git worktree remove` and `prune` leave it alone.
    let git_args: Vec<OsString> = ["--detach", "--lock", "--reason", "managed by git-mito"]
        .map(OsString::from)
        .into_iter()
        .chain([path.clone().into(), target.to_string().into()])
        .collect();
    // Rolls the new base back if anything fails or it is discarded, until it
    // is kept once published.
    let mut pending = worktree::clone(&mito.repo, &mito.root, &git_args, &path, source.as_ref())?;
    drop(guard);
    pending.checkout()?;

    // The base published by now; the commit is read again only if it changed.
    let published = |path: Option<&Path>| -> Result<Option<gix::ObjectId>> {
        match path {
            Some(p) if Some(p) == current.as_deref() => Ok(current_commit),
            Some(p) => Ok(Some(git::head(&git::open(p)?)?)),
            None => Ok(None),
        }
    };
    // Don't run on-update for a base that has already lost.
    let early = published(mito.lock_base()?.base.as_deref())?;
    if let Some(winner) = loses_to(&mito.repo, resolve_track(mito)?, target, early)? {
        discarded(&short_target, winner);
        return Ok(());
    }
    if let Some(command) = &mito.config.base.on_update {
        on_update(command, &path)?;
    }
    // A published base must never change: its files, index and HEAD have to
    // keep agreeing for every worktree cloned from it.
    btrfs::set_read_only(&path, true)?;

    let tracked = resolve_track(mito)?;
    let lock = mito.lock_base_exclusive()?;
    let old = lock.base.clone();
    if let Some(winner) = loses_to(&mito.repo, tracked, target, published(old.as_deref())?)? {
        drop(lock);
        discarded(&short_target, winner);
        return Ok(());
    }
    mito.publish_base(&lock, &id, target)?;
    drop(lock);
    pending.keep();
    println!("Base updated to {short_target} at {}", path.display());
    // Swapped under the exclusive lock, so nothing still reads the old base.
    if let Some(old) = old {
        worktree::destroy(&mito.root, &old)?;
    }
    Ok(())
}

fn discarded(target: &str, winner: gix::ObjectId) {
    let winner = short(&winner);
    println!(
        "Discarded the base at {target}: {winner} was published meanwhile, and {target} isn't ahead of it"
    );
}

/// The published commit a base at `target` loses to, if any. It wins when
/// nothing is published, when the tracked revision is still `target` (the
/// common case), or when `target` is strictly ahead of `published` in history.
fn loses_to(
    repo: &gix::Repository,
    tracked: gix::ObjectId,
    target: gix::ObjectId,
    published: Option<gix::ObjectId>,
) -> Result<Option<gix::ObjectId>> {
    let Some(published) = published else {
        return Ok(None);
    };
    let ahead = published != target
        && repo
            .merge_base(published, target)?
            .is_some_and(|base| base == published);
    Ok((tracked != target && !ahead).then_some(published))
}

/// Runs the on-update `command` in the new base at `path`. The base is a
/// snapshot of the previous one, so whatever the last run left behind is
/// there to build on.
fn on_update(command: &str, path: &Path) -> Result<()> {
    eprintln!("Running on-update in the new base: {command}");
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command).current_dir(path);
    git::scrub_env(&mut cmd);
    let status = cmd.status().context("running the on-update command")?;
    if !status.success() {
        bail!("the on-update command failed ({status}); the base was left as it was");
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
