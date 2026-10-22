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
    /// The commit `git worktree add` set HEAD to.
    head: gix::ObjectId,
    /// What undoes `git worktree add`'s change to the branch HEAD is on.
    branch: Option<BranchUndo>,
    source_commit: Option<gix::ObjectId>,
    done: bool,
}

/// Undoes what `git worktree add` did to a branch, as long as the branch is
/// still at `at`, where it left it, so as not to undo a move made meanwhile.
enum BranchUndo {
    /// Deletes a branch it created, with any tracking set up for it.
    Delete {
        name: gix::refs::FullName,
        at: gix::ObjectId,
    },
    /// Moves a branch it reset back to `to`.
    Reset {
        name: gix::refs::FullName,
        at: gix::ObjectId,
        to: gix::ObjectId,
    },
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
    let repo = git::open(path)?;
    let head = git::head(&repo)?;
    let branch = repo.head_name()?.map(|name| match branches.get(&name) {
        Some(&to) => BranchUndo::Reset { name, at: head, to },
        None => BranchUndo::Delete { name, at: head },
    });
    let pending = Pending {
        dir: dir.to_owned(),
        path: path.to_owned(),
        repo,
        head,
        branch,
        source_commit: source.map(|s| s.commit),
        done: false,
    };
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

    /// Whether git has written the worktree's index split. Its gitdir is new,
    /// so a shared index in it can only be one git wrote for this index.
    pub fn has_split_index(&self) -> Result<bool> {
        let gitdir = self.repo.git_dir();
        for entry in
            fs::read_dir(gitdir).with_context(|| format!("reading {}", gitdir.display()))?
        {
            if entry?
                .file_name()
                .as_encoded_bytes()
                .starts_with(b"sharedindex.")
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Brings the files to HEAD with `git reset --hard`, which touches only
    /// what differs from the source. Returns HEAD.
    pub fn checkout(&mut self) -> Result<gix::ObjectId> {
        let head = self.head;
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
        match &self.branch {
            Some(BranchUndo::Delete { name, at }) => {
                let full = name.as_bstr().to_string();
                if git::run(&self.dir, ["update-ref", "-d", &full, &at.to_string()]).is_ok() {
                    let section = format!("branch.{}", name.shorten());
                    let _ = git::run(&self.dir, ["config", "--remove-section", &section]);
                }
            }
            Some(BranchUndo::Reset { name, at, to }) => {
                let full = name.as_bstr().to_string();
                let _ = git::run(
                    &self.dir,
                    ["update-ref", &full, &to.to_string(), &at.to_string()],
                );
            }
            None => {}
        }
    }
}

/// A worktree to delete, as a base once replaced. It is deleted when
/// dropped, so that nothing failing or panicking first leaves it behind;
/// [`Retired::destroy`] deletes it and reports errors.
pub struct Retired {
    /// Where git runs.
    dir: PathBuf,
    path: Option<PathBuf>,
}

impl Retired {
    pub fn new(dir: &Path, path: PathBuf) -> Self {
        Self {
            dir: dir.to_owned(),
            path: Some(path),
        }
    }

    pub fn destroy(mut self) -> Result<()> {
        match self.path.take() {
            Some(path) => destroy(&self.dir, &path),
            None => Ok(()),
        }
    }
}

impl Drop for Retired {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = destroy(&self.dir, &path);
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
