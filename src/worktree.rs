//! Creating worktrees as btrfs subvolumes.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::btrfs;
use crate::git;

/// The base a worktree is cloned from.
pub struct Source<'a> {
    pub path: &'a Path,
    pub commit: gix::ObjectId,
    /// The base's index, which matches its files.
    pub index: PathBuf,
}

impl<'a> Source<'a> {
    pub fn open(path: &'a Path) -> Result<Self> {
        let repo = git::open(path)?;
        let commit = git::head(&repo)
            .with_context(|| format!("reading base worktree {}", path.display()))?;
        let index = repo.index_path();
        Ok(Self {
            path,
            commit,
            index,
        })
    }
}

/// A worktree registered with git that isn't finished yet. Until it is kept,
/// by [`Pending::keep`] or [`Pending::commit`], dropping it rolls everything
/// back: the worktree, its registration, and whatever `git worktree add` did
/// to its branch.
pub struct Pending {
    /// Where git runs.
    dir: PathBuf,
    path: PathBuf,
    /// The new worktree.
    repo: gix::Repository,
    /// The branch HEAD is on and its tip before `git worktree add`; `None`
    /// for a branch git created.
    branch: Option<(gix::refs::FullName, Option<gix::ObjectId>)>,
    source_commit: Option<gix::ObjectId>,
    done: bool,
}

/// Creates the worktree at `path` by running `git worktree add --no-checkout
/// <args>` in `dir` (`repo` is its repository), then replaces its directory
/// with a subvolume: a snapshot of `source` with the source's index, or an
/// empty one. Once this returns, the source is no longer read.
pub fn clone(
    repo: &gix::Repository,
    dir: &Path,
    args: &[OsString],
    path: &Path,
    source: Option<&Source<'_>>,
) -> Result<Pending> {
    let branches = git::local_branches(repo)?;
    let add = ["worktree", "add", "--no-checkout"].map(OsString::from);
    git::run(dir, add.iter().chain(args))?;
    let mut pending = Pending {
        dir: dir.to_owned(),
        path: path.to_owned(),
        repo: git::open(path)?,
        branch: None,
        source_commit: source.map(|s| s.commit),
        done: false,
    };
    pending.branch = pending.repo.head_name()?.map(|name| {
        let tip = branches.get(&name).copied();
        (name, tip)
    });
    pending.populate(source)?;
    Ok(pending)
}

impl Pending {
    fn populate(&self, source: Option<&Source<'_>>) -> Result<()> {
        // Replace the directory with a subvolume, then restore the new
        // worktree's `.git` file over the source's copy. It is carried across
        // in memory: rename(2) can't cross into another subvolume.
        let gitfile = self.path.join(".git");
        let link = fs::read(&gitfile)?;
        fs::remove_file(&gitfile)?;
        fs::remove_dir(&self.path)?;
        match source {
            Some(source) => btrfs::snapshot(source.path, &self.path)?,
            None => btrfs::create_subvolume(&self.path)?,
        }
        fs::write(&gitfile, link)?;
        if let Some(source) = source {
            // On btrfs this is a reflink: the copy shares the source's extents.
            fs::copy(&source.index, self.repo.index_path())
                .with_context(|| format!("copying the index of {}", source.path.display()))?;
        }
        Ok(())
    }

    /// Brings the files to HEAD with `git reset --hard`, which touches only
    /// what differs from the source. Returns HEAD.
    pub fn checkout(&mut self) -> Result<gix::ObjectId> {
        let head = git::head(&self.repo)?;
        match self.source_commit {
            Some(base) if base == head => {}
            Some(base) => {
                eprintln!(
                    "note: {} differs from the base ({}); checking out the difference",
                    git::short(&head),
                    git::short(&base)
                );
                git::run(&self.path, ["reset", "--hard", "--quiet"])?;
            }
            None => {
                git::run(&self.path, ["reset", "--hard", "--quiet"])?;
            }
        }
        Ok(head)
    }

    /// Keeps the worktree: it is done, and dropping no longer rolls it back.
    pub fn keep(mut self) {
        self.done = true;
    }

    /// Runs `publish`, the step that makes the worktree final, and keeps the
    /// worktree if it succeeds: once published, it must stay whatever fails
    /// next. `publish` must not fail after making it final.
    pub fn commit<T>(self, publish: impl FnOnce() -> Result<T>) -> Result<T> {
        let published = publish()?;
        self.keep();
        Ok(published)
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let _ = destroy(&self.dir, &self.path);
        if let Some((name, tip)) = &self.branch {
            let name = name.as_bstr().to_string();
            let _ = match tip {
                None => git::run(&self.dir, ["update-ref", "-d", &name]),
                Some(tip) => git::run(&self.dir, ["update-ref", &name, &tip.to_string()]),
            };
        }
    }
}

/// Deletes the worktree at `path`, its subvolume in one step where
/// permitted, and unregisters it, even if locked.
pub fn destroy(dir: &Path, path: &Path) -> Result<()> {
    if path.exists() {
        btrfs::delete_tree(path)?;
    }
    let remove = ["worktree", "remove", "-ff"].map(OsStr::new);
    git::run(dir, remove.iter().chain([&path.as_os_str()]))?;
    Ok(())
}
