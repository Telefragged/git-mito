//! The btrfs operations mito needs: creating, snapshotting, flagging and
//! deleting subvolumes.

use std::ffi::CString;
use std::fs::File;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Once;

use anyhow::{Context, Result, bail};
use btrfs_uapi::raw::BTRFS_FIRST_FREE_OBJECTID;
use btrfs_uapi::subvolume::{self, SubvolumeFlags};
use nix::errno::Errno;
use nix::sys::statfs::{BTRFS_SUPER_MAGIC, statfs};

fn is_btrfs(path: &Path) -> Result<bool> {
    let stat = statfs(path).with_context(|| format!("statfs {}", path.display()))?;
    Ok(stat.filesystem_type() == BTRFS_SUPER_MAGIC)
}

/// Fails unless `path` lives on a btrfs filesystem.
pub fn ensure_btrfs(path: &Path) -> Result<()> {
    if !is_btrfs(path)? {
        bail!("{} is not on a btrfs filesystem", path.display());
    }
    Ok(())
}

pub fn is_subvolume(path: &Path) -> Result<bool> {
    let meta = std::fs::symlink_metadata(path)?;
    // A subvolume's root directory always has the first free object ID.
    Ok(meta.is_dir() && meta.ino() == u64::from(BTRFS_FIRST_FREE_OBJECTID) && is_btrfs(path)?)
}

fn open(path: &Path) -> Result<File> {
    File::open(path).with_context(|| format!("opening {}", path.display()))
}

/// Opens the parent directory of `path` and returns it with the leaf name,
/// the form the subvolume ioctls take.
fn parent_and_name(path: &Path) -> Result<(File, CString)> {
    let name = path
        .file_name()
        .with_context(|| format!("{} has no file name", path.display()))?;
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let dir = open(parent)?;
    Ok((dir, CString::new(name.as_bytes())?))
}

pub fn create_subvolume(path: &Path) -> Result<()> {
    let (dir, name) = parent_and_name(path)?;
    subvolume::subvolume_create(dir.as_fd(), &name, &[])
        .with_context(|| format!("creating subvolume {}", path.display()))
}

/// Creates a writable snapshot of the subvolume `source` at `dest`, which must
/// not exist.
pub fn snapshot(source: &Path, dest: &Path) -> Result<()> {
    let src = open(source)?;
    let (dir, name) = parent_and_name(dest)?;
    match subvolume::snapshot_create(dir.as_fd(), src.as_fd(), &name, false, &[]) {
        // ENOTTY: the destination isn't btrfs; EXDEV: it's another btrfs.
        Err(Errno::ENOTTY | Errno::EXDEV) => bail!(
            "{} must be on the same btrfs filesystem as {}",
            dest.display(),
            source.display()
        ),
        result => result
            .with_context(|| format!("snapshotting {} to {}", source.display(), dest.display())),
    }
}

pub fn set_read_only(path: &Path, read_only: bool) -> Result<()> {
    let file = open(path)?;
    let mut flags = subvolume::subvolume_flags_get(file.as_fd())?;
    if flags.contains(SubvolumeFlags::RDONLY) == read_only {
        return Ok(());
    }
    flags.set(SubvolumeFlags::RDONLY, read_only);
    subvolume::subvolume_flags_set(file.as_fd(), flags)
        .with_context(|| format!("setting read-only={read_only} on {}", path.display()))
}

/// Deletes the directory tree at `path`. A subvolume is deleted with the
/// subvolume ioctl, which is instant; when the kernel refuses that to an
/// unprivileged user it falls back to a recursive delete.
pub fn delete_tree(path: &Path) -> Result<()> {
    if is_subvolume(path)? {
        // Unprivileged users can delete neither a read-only subvolume (EROFS)
        // nor its contents one by one.
        set_read_only(path, false)?;
        let (dir, name) = parent_and_name(path)?;
        match subvolume::subvolume_delete(dir.as_fd(), &name) {
            Ok(()) => return Ok(()),
            Err(Errno::EPERM) => warn_slow_delete(path),
            Err(e) => {
                return Err(e).with_context(|| format!("deleting subvolume {}", path.display()));
            }
        }
    }
    std::fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))
}

fn warn_slow_delete(path: &Path) {
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        eprintln!(
            "warning: deleting subvolumes needs root or the `user_subvol_rm_allowed` mount \
             option; deleting {} file by file",
            path.display()
        );
    });
}
