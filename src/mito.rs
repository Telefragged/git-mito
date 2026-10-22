//! Mito's on-disk state, kept in `<root>/.mito` by default:
//!
//! ```text
//! .mito/
//!   base -> bases/<id>   symlink to the current base; swapped atomically.
//!                        <id> is the base's short commit, suffixed `.N` if
//!                        that commit is rebuilt
//!   bases/<id>/          one btrfs subvolume per base, each a git worktree
//!   lock                 flock guarding the swap of `base`
//! ```
//!
//! A base never changes once `base` points at it, so its files, index and
//! `HEAD` always agree. Anything copying a base (`add`, `update`) holds `lock`
//! shared from reading `base` until the copy is made. `update` holds it
//! exclusively only to decide on and make the swap, the one point where
//! concurrent updates meet; nothing can still be reading the base it
//! replaces, so it deletes that base afterwards without the lock.
//!
//! [`BaseLock`] enforces this: `base` is read only when taking the lock, and
//! a [`Source`](crate::worktree::Source) borrows its directory from it, so
//! the lock can't be dropped before the copy is made, and the swap takes the
//! lock exclusively.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use nix::fcntl::{Flock, FlockArg};

use crate::config::Config;
use crate::git;

pub struct Mito {
    /// The repository of the worktree mito was run in.
    pub repo: gix::Repository,
    /// The main worktree's repository.
    pub main: gix::Repository,
    /// The main worktree.
    pub root: PathBuf,
    pub config: Config,
    pub dir: PathBuf,
}

impl Mito {
    /// Finds the repository containing the current directory, from the main
    /// worktree or any linked one.
    pub fn discover() -> Result<Self> {
        let repo = gix::discover(std::env::current_dir()?).context("not in a git repository")?;
        let ref_storage = repo.config_snapshot().string("extensions.refStorage");
        if ref_storage.is_some_and(|s| s == "reftable") {
            bail!("repositories with reftable refs are not supported yet");
        }
        let main = repo.main_repo()?;
        let Some(root) = main.workdir().filter(|_| !main.is_bare()) else {
            bail!("bare repositories are not supported");
        };
        let root = root.to_path_buf();
        let config = Config::load(&root)?;
        let dir = root.join(config.dir.as_deref().unwrap_or(Path::new(".mito")));
        Ok(Self {
            repo,
            main,
            root,
            config,
            dir,
        })
    }

    fn base_link(&self) -> PathBuf {
        self.dir.join("base")
    }

    pub fn bases_dir(&self) -> PathBuf {
        self.dir.join("bases")
    }

    fn current_base(&self) -> Result<Option<PathBuf>> {
        match fs::read_link(self.base_link()) {
            Ok(target) => Ok(Some(self.dir.join(target))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context("reading the base pointer"),
        }
    }

    /// Makes `bases/<id>`, at `commit`, the current base: swaps the base
    /// pointer atomically and moves the `mito/base` branch to match.
    pub fn publish_base(
        &self,
        _lock: &ExclusiveBaseLock,
        id: &str,
        commit: gix::ObjectId,
    ) -> Result<()> {
        let tmp = self.dir.join("base.tmp");
        let _ = fs::remove_file(&tmp);
        std::os::unix::fs::symlink(Path::new("bases").join(id), &tmp)?;
        fs::rename(&tmp, self.base_link()).context("swapping the base pointer")?;
        self.repo.reference(
            "refs/heads/mito/base",
            commit,
            gix::refs::transaction::PreviousValue::Any,
            format!("mito: update base to {}", git::short(&commit)),
        )?;
        Ok(())
    }

    /// Takes the lock guarding `base` shared, to read and copy the base.
    pub fn lock_base(&self) -> Result<BaseLock> {
        self.lock_base_as(FlockArg::LockShared)
    }

    /// Takes the lock guarding `base` exclusively, to swap it.
    pub fn lock_base_exclusive(&self) -> Result<ExclusiveBaseLock> {
        self.lock_base_as(FlockArg::LockExclusive)
            .map(ExclusiveBaseLock)
    }

    fn lock_base_as(&self, arg: FlockArg) -> Result<BaseLock> {
        let flock = lock(&self.dir.join("lock"), arg)?;
        Ok(BaseLock {
            base: self.current_base()?,
            _flock: flock,
        })
    }

    /// Adds the state directory to `info/exclude` when it lives inside the
    /// main worktree, so it never shows up as untracked.
    pub fn ensure_excluded(&self) -> Result<()> {
        let Ok(rel) = self.dir.strip_prefix(&self.root) else {
            return Ok(());
        };
        let pattern = format!("/{}/", rel.display());
        let info = self.main.git_dir().join("info");
        let exclude = info.join("exclude");
        let current = fs::read_to_string(&exclude).unwrap_or_default();
        if current.lines().any(|l| l.trim() == pattern) {
            return Ok(());
        }
        fs::create_dir_all(&info)?;
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&exclude)?;
        if !current.is_empty() && !current.ends_with('\n') {
            writeln!(file)?;
        }
        writeln!(file, "{pattern}")?;
        Ok(())
    }
}

/// The lock guarding `base`, held, and the base it guards.
pub struct BaseLock {
    /// The directory of the current base, if there is one. It stays the
    /// current base while the lock is held.
    pub base: Option<PathBuf>,
    _flock: Flock<File>,
}

/// [`BaseLock`], held exclusively.
pub struct ExclusiveBaseLock(BaseLock);

impl Deref for ExclusiveBaseLock {
    type Target = BaseLock;

    fn deref(&self) -> &BaseLock {
        &self.0
    }
}

fn lock(path: &Path, arg: FlockArg) -> Result<Flock<File>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    Flock::lock(file, arg)
        .map_err(|(_, e)| e)
        .with_context(|| format!("locking {}", path.display()))
}
