//! Git access. Reads go through gix, in-process and typed. Anything that
//! writes a worktree's files or index runs the `git` binary, which `git mito`
//! always has at hand, so it behaves exactly like git: filters such as Git
//! LFS, symlinks and the index's cache tree included.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

/// Runs `git -C <dir> <args>` and returns its stdout, trailing newline
/// trimmed. On failure the error carries git's stderr.
pub fn run<I, S>(dir: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = output(dir, args)?;
    let mut stdout = String::from_utf8(output.stdout).context("git printed non-UTF-8 output")?;
    stdout.truncate(stdout.trim_end_matches('\n').len());
    Ok(stdout)
}

/// Runs `git -C <dir> <args>` and returns everything it printed. On failure
/// the error carries git's stderr.
pub fn output<I, S>(dir: &Path, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    scrub_env(&mut cmd);
    let output = cmd.output().context("running git")?;
    if !output.status.success() {
        let args: Vec<_> = cmd
            .get_args()
            .skip(2)
            .map(|a| a.to_string_lossy())
            .collect();
        bail!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

/// Opens the repository or worktree at `path`.
pub fn open(path: &Path) -> Result<gix::Repository> {
    gix::open(path).with_context(|| format!("opening repository at {}", path.display()))
}

/// The commit HEAD points at.
pub fn head(repo: &gix::Repository) -> Result<gix::ObjectId> {
    // Not `head_id()`: it peels through the object database, which takes
    // ~18 ms on the Linux kernel's repository.
    let head = repo.head()?;
    let id = head.id().context("HEAD does not point at a commit yet")?;
    Ok(id.detach())
}

/// Removes the environment variables that pin git to one worktree's location
/// (the part of `git rev-parse --local-env-vars` that does so; config and
/// object settings still apply), so `cmd` finds the worktree it runs in.
pub fn scrub_env(cmd: &mut Command) {
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_PREFIX",
    ] {
        cmd.env_remove(var);
    }
}

/// The commit `rev` names.
pub fn resolve_commit(repo: &gix::Repository, rev: &str) -> Result<gix::ObjectId> {
    let spec = format!("{rev}^{{commit}}");
    let id = repo
        .rev_parse_single(spec.as_str())
        .with_context(|| format!("`{rev}` does not name a commit"))?;
    Ok(id.detach())
}

/// Every local branch and the commit it points at.
pub fn local_branches(
    repo: &gix::Repository,
) -> Result<HashMap<gix::refs::FullName, gix::ObjectId>> {
    let mut branches = HashMap::new();
    for branch in repo.references()?.local_branches()? {
        let branch = branch.map_err(|e| anyhow::anyhow!(e))?;
        if let Some(id) = branch.try_id() {
            branches.insert(branch.name().to_owned(), id.detach());
        }
    }
    Ok(branches)
}

pub fn short(id: &gix::oid) -> String {
    id.to_hex_with_len(10).to_string()
}
