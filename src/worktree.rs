//! Creating worktrees as btrfs subvolumes.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::btrfs;
use crate::git;
use crate::signal;

pub struct Source<'a> {
    pub path: &'a Path,
    pub commit: gix::ObjectId,
    pub kind: SourceKind,
    repo: gix::Repository,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Base,
    Worktree,
}

impl<'a> Source<'a> {
    pub fn open_base(path: &'a Path) -> Result<Self> {
        Self::new(path, git::open(path)?, SourceKind::Base)
    }

    pub fn open_worktree(path: &'a Path, repo: gix::Repository) -> Result<Self> {
        if !btrfs::is_subvolume(path)? {
            bail!(
                "{} is not a btrfs subvolume; only worktrees that git mito added can be copied",
                path.display()
            );
        }
        Self::new(path, repo, SourceKind::Worktree)
    }

    fn new(path: &'a Path, repo: gix::Repository, kind: SourceKind) -> Result<Self> {
        let commit =
            git::head(&repo).with_context(|| format!("reading worktree {}", path.display()))?;
        Ok(Self {
            path,
            commit,
            kind,
            repo,
        })
    }

    fn changes_while_copied(&self) -> bool {
        self.kind == SourceKind::Worktree
    }

    /// Copies the index into `repo`, then snapshots the files to `path`. An
    /// index older than the files is what git handles in any worktree being
    /// edited; the reverse order can leave a file modified twice within a
    /// second looking unchanged.
    fn copy_into(&self, repo: &gix::Repository, path: &Path) -> Result<()> {
        self.copy_index(repo)?;
        btrfs::snapshot(self.path, path)?;
        self.ensure_copy_valid()
    }

    /// git takes no lock for the length of an operation or a commit, so a
    /// check after the copy also detects one that started during it. An
    /// operation's files are stored in the worktree's gitdir, which the copy
    /// does not get, and rebase and bisect record the worktree's branch,
    /// which the copy is not on.
    fn ensure_copy_valid(&self) -> Result<()> {
        use gix::state::InProgress;
        if !self.changes_while_copied() {
            return Ok(());
        }
        if git::head(&self.repo)? != self.commit {
            bail!(
                "HEAD of {} moved while it was copied; try again",
                self.path.display()
            );
        }
        let operation = match self.repo.state() {
            None => return Ok(()),
            Some(InProgress::ApplyMailbox) => "`git am`",
            Some(InProgress::ApplyMailboxRebase) => "`git am` or a rebase",
            Some(InProgress::Bisect) => "a bisect",
            Some(InProgress::CherryPick | InProgress::CherryPickSequence) => "a cherry-pick",
            Some(InProgress::Merge) => "a merge",
            Some(InProgress::Rebase | InProgress::RebaseInteractive) => "a rebase",
            Some(InProgress::Revert | InProgress::RevertSequence) => "a revert",
        };
        bail!(
            "{} is in the middle of {operation}; finish or abort it first",
            self.path.display()
        );
    }

    /// git rehashes only the entries of files modified in the same second as
    /// the index was written or later, so a copy of the index with a later
    /// mtime misses files modified after that write within the same second.
    fn copy_index(&self, repo: &gix::Repository) -> Result<()> {
        let context = || format!("copying the index of {}", self.path.display());
        let keep_mtime = self.changes_while_copied();
        let index = repo.index_path();
        let git_dir = self.repo.git_dir();
        copy(&self.repo.index_path(), &index, keep_mtime).with_context(context)?;
        if let Some(name) = shared_index(&index, git_dir, repo.object_hash())? {
            copy(
                &git_dir.join(&name),
                &repo.git_dir().join(&name),
                keep_mtime,
            )
            .with_context(context)?;
        }
        Ok(())
    }
}

/// With `keep_mtime`, the mtime comes from the opened `from`, so it matches
/// the content copied even if git renames a new file over `from` meanwhile.
fn copy(from: &Path, to: &Path, keep_mtime: bool) -> Result<()> {
    let mut source = File::open(from).with_context(|| format!("opening {}", from.display()))?;
    let mut dest = File::create(to).with_context(|| format!("creating {}", to.display()))?;
    std::io::copy(&mut source, &mut dest)?;
    if keep_mtime {
        dest.set_modified(source.metadata()?.modified()?)?;
    }
    Ok(())
}

/// A guard over a worktree registered with git that isn't finished yet.
/// Until it is kept, by [`WorktreeGuard::keep`] or
/// [`WorktreeGuard::commit`], dropping it rolls everything back: the
/// worktree, its registration, and whatever `git worktree add` did to its
/// branch.
pub struct WorktreeGuard {
    /// Where git runs.
    dir: PathBuf,
    path: PathBuf,
    /// The new worktree.
    repo: gix::Repository,
    /// The commit `git worktree add` set HEAD to.
    head: gix::ObjectId,
    /// What `git worktree add` did to the branch HEAD is on.
    branch: Option<BranchChange>,
    source_commit: Option<gix::ObjectId>,
    done: bool,
}

/// What `git worktree add` did to a branch, which it left at `at`. A rollback
/// undoes it only while the branch is still there, so as not to undo a move
/// made meanwhile.
enum BranchChange {
    /// It created the branch; undone by deleting it, with any tracking set up
    /// for it.
    Created {
        name: gix::refs::FullName,
        at: gix::ObjectId,
    },
    /// It moved an existing branch away from `from`; undone by moving it back.
    Moved {
        name: gix::refs::FullName,
        at: gix::ObjectId,
        from: gix::ObjectId,
    },
}

/// Creates the worktree at `path` by running `git worktree add --no-checkout
/// <args>` in `dir` (`repo` is its repository), then replaces its directory
/// with a subvolume: a snapshot of `source` with the source's index, or an
/// empty one. Once this returns, the source is no longer read. With `echo`,
/// git's own messages are passed on, as `git worktree add` would print them.
pub fn clone(
    repo: &gix::Repository,
    dir: &Path,
    args: &[OsString],
    path: &Path,
    source: Option<&Source<'_>>,
    echo: bool,
) -> Result<WorktreeGuard> {
    let branches = git::local_branches(repo)?;
    let add = ["worktree", "add", "--no-checkout"].map(OsString::from);
    let output = git::output(dir, add.iter().chain(args))?;
    if echo {
        std::io::stderr().write_all(&output.stderr)?;
    }
    let repo = git::open(path)?;
    let head = git::head(&repo)?;
    let branch = repo.head_name()?.map(|name| match branches.get(&name) {
        Some(&from) => BranchChange::Moved {
            name,
            at: head,
            from,
        },
        None => BranchChange::Created { name, at: head },
    });
    let pending = WorktreeGuard {
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

impl WorktreeGuard {
    fn populate(&self, source: Option<&Source<'_>>) -> Result<()> {
        // Replace the directory with a subvolume, then restore the new
        // worktree's `.git` file over the source's copy. It is carried across
        // in memory: rename(2) can't cross into another subvolume.
        let gitfile = self.path.join(".git");
        let link = fs::read(&gitfile)?;
        fs::remove_file(&gitfile)?;
        fs::remove_dir(&self.path)?;
        match source {
            Some(source) => source.copy_into(&self.repo, &self.path)?,
            None => btrfs::create_subvolume(&self.path)?,
        }
        fs::write(&gitfile, link)?;
        Ok(())
    }

    /// Brings the files to HEAD with `git reset --hard`, which touches only
    /// what differs from the source. Returns HEAD.
    pub fn checkout(&mut self) -> Result<gix::ObjectId> {
        let head = self.head;
        if self.source_commit != Some(head) {
            git::run(&self.path, ["reset", "--hard", "--quiet"])?;
        }
        Ok(head)
    }

    /// Keeps the worktree: it is done, and dropping no longer rolls it back.
    pub fn keep(self) -> Result<()> {
        self.commit(|| Ok(()))
    }

    /// Runs `publish`, the step that makes the worktree final, and keeps the
    /// worktree if it succeeds: once published, it must stay whatever fails
    /// next. `publish` must not fail after making it final. Neither happens
    /// once mito has been interrupted.
    pub fn commit<T>(mut self, publish: impl FnOnce() -> Result<T>) -> Result<T> {
        signal::check()?;
        let published = publish()?;
        self.done = true;
        Ok(published)
    }
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let _ = destroy(&self.dir, &self.path);
        match &self.branch {
            Some(BranchChange::Created { name, at }) => {
                let full = name.as_bstr().to_string();
                if git::run(&self.dir, ["update-ref", "-d", &full, &at.to_string()]).is_ok() {
                    let section = format!("branch.{}", name.shorten());
                    let _ = git::run(&self.dir, ["config", "--remove-section", &section]);
                }
            }
            Some(BranchChange::Moved { name, at, from }) => {
                let full = name.as_bstr().to_string();
                let _ = git::run(
                    &self.dir,
                    ["update-ref", &full, &from.to_string(), &at.to_string()],
                );
            }
            None => {}
        }
    }
}

fn shared_index(
    copied_index: &Path,
    source_git_dir: &Path,
    hash: gix::hash::Kind,
) -> Result<Option<String>> {
    if !has_shared_index(source_git_dir)? {
        return Ok(None);
    }
    let data =
        fs::read(copied_index).with_context(|| format!("reading {}", copied_index.display()))?;
    let (state, _) = gix::index::State::from_bytes(
        &data,
        filetime::FileTime::zero(),
        hash,
        gix::index::decode::Options::default(),
    )
    .with_context(|| format!("decoding {}", copied_index.display()))?;
    Ok(state
        .link()
        .map(|link| format!("sharedindex.{}", link.shared_index_checksum)))
}

fn has_shared_index(dir: &Path) -> Result<bool> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
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

/// A guard over a base no longer in use, as one replaced or removed.
/// Dropping it deletes the base, so that nothing failing or panicking first
/// leaves it behind; [`UnusedBaseGuard::destroy`] deletes it and reports
/// errors.
pub struct UnusedBaseGuard {
    /// Where git runs.
    dir: PathBuf,
    path: Option<PathBuf>,
}

impl UnusedBaseGuard {
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

impl Drop for UnusedBaseGuard {
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
