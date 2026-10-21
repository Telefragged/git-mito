//! Mito's on-disk state, kept in `<root>/.mito` by default:
//!
//! ```text
//! .mito/
//!   base -> bases/<id>   symlink to the current base; swapped atomically
//!   bases/<id>/          one btrfs subvolume per base, each a git worktree
//!   lock                 flock guarding `base` against deletion races
//!   update.lock          flock serialising `git mito update`
//! ```
//!
//! A base never changes once `base` points at it, so its files, index and
//! `HEAD` always agree. `add` holds `lock` shared from reading `base` until it
//! has copied the base; `update` holds it exclusively only to swap `base`.
//! Once that exclusive lock has been taken, no `add` can still be reading an
//! older base, so older bases can be deleted without holding any lock.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use nix::fcntl::{Flock, FlockArg};

use crate::btrfs;
use crate::config::Config;
use crate::worktree;

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

    /// The directory of the current base, if there is one.
    pub fn current_base(&self) -> Result<Option<PathBuf>> {
        match fs::read_link(self.base_link()) {
            Ok(target) => Ok(Some(self.dir.join(target))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context("reading the base pointer"),
        }
    }

    /// Makes `bases/<id>` the current base, atomically.
    pub fn publish_base(&self, id: &str) -> Result<()> {
        let tmp = self.dir.join("base.tmp");
        let _ = fs::remove_file(&tmp);
        std::os::unix::fs::symlink(Path::new("bases").join(id), &tmp)?;
        fs::rename(&tmp, self.base_link()).context("swapping the base pointer")
    }

    /// Takes the lock that keeps the base `add` reads from being deleted.
    pub fn lock_base(&self, exclusive: bool) -> Result<Flock<File>> {
        let arg = if exclusive {
            FlockArg::LockExclusive
        } else {
            FlockArg::LockShared
        };
        lock(&self.dir.join("lock"), arg)
    }

    /// Takes the lock serialising updates, failing if one is running.
    pub fn lock_update(&self) -> Result<Flock<File>> {
        lock(
            &self.dir.join("update.lock"),
            FlockArg::LockExclusiveNonblock,
        )
        .context("another `git mito update` is running")
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

    /// Deletes every base but the current one, along with its worktree
    /// registration. Only `update` calls this, under the update lock.
    pub fn collect_garbage(&self) -> Result<()> {
        let current = self.current_base()?;
        let bases = self.bases_dir();
        let stale = |path: &Path| path.starts_with(&bases) && Some(path) != current.as_deref();
        for worktree in self.repo.worktrees()? {
            let path = worktree.base()?;
            if stale(&path) {
                worktree::destroy(&self.root, &path)?;
            }
        }
        // Whatever an interrupted `update` left behind unregistered.
        if let Ok(entries) = fs::read_dir(&bases) {
            for entry in entries {
                let path = entry?.path();
                if stale(&path) {
                    btrfs::delete_tree(&path)?;
                }
            }
        }
        Ok(())
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
