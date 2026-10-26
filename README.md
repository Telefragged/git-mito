# git-mito

Fast `git worktree add` for very large repositories on btrfs.

mito keeps a checked-out **base** worktree in a btrfs subvolume. New worktrees
are btrfs snapshots of it, with the base's git index copied in, so creating
one takes about as long as taking the snapshot, however many files the
repository has. Ignored files in the base, such as build outputs, come along.

```sh
git mito update          # create the base, or move it to the tracked revision
git mito add ../feature  # like `git worktree add`, but a snapshot of the base
git mito remove ../feature
```

## Performance

Measured on an ADATA SX8200PNP NVMe SSD with btrfs (Linux 7.2.11,
btrfs-progs 7.1, `compress=zstd:3`) and an AMD Ryzen 9 9950X3D, with a warm
page cache:

| | `git worktree add` | `git mito add` |
|---|---|---|
| Create a worktree in a repository with 72,867 commits, a 1.2 GB `.git` and about 1 GiB of checked-out files | 1.60 s | 5.1 ms |
| `cargo t` in a new git-mito worktree after a one-line change, with `cargo build --all-targets` as the base's on-update command | 7.2 s | 0.9 s |

## Commands

### `git mito update`

```text
Move the base to the tracked revision, creating it if needed.

The new base is built beside the current one and swapped in atomically, so `git mito add` keeps working throughout.

Usage: git-mito update [OPTIONS]

Options:
  -f, --force
          Rebuild the base even if it is already at the tracked revision

  -h, --help
          Print help (see a summary with '-h')
```

### `git mito add`

```text
Create a worktree by snapshotting the base.

Takes `git worktree add`'s options and resolves them the same way. A <commit-ish> other than the base's checks out the difference from it.

Usage: git-mito add [OPTIONS] <PATH> [commit-ish]

Arguments:
  <PATH>


  [commit-ish]


Options:
  -b <new-branch>
          Create a new branch named <new-branch> starting at <commit-ish>

  -B <new-branch>
          Like -b, but reset <new-branch> to <commit-ish> if it exists

  -d, --detach
          Detach HEAD in the new worktree

  -f, --force
          Check out a branch even if another worktree has it checked out

      --lock
          Keep the new worktree locked after creation

      --reason <REASON>
          Reason for --lock

      --track
          Set up tracking for a new branch (see git-branch(1))

      --no-track


  -h, --help
          Print help (see a summary with '-h')
```

### `git mito remove`

```text
Remove a worktree, deleting its subvolume in one step where permitted.

`git worktree remove` works on mito worktrees too, but deletes them file by file.

Usage: git-mito remove [OPTIONS] <WORKTREE>

Arguments:
  <WORKTREE>


Options:
  -f, --force
          Remove even if the worktree is dirty or locked, or is a base

  -h, --help
          Print help (see a summary with '-h')
```

## Configuration

Optional, in `.mito.toml` at the root of the main worktree.

```toml
# Where mito keeps its state, relative to the repository root.
dir = ".mito"

[base]
# Revision the base follows. Defaults to origin/HEAD, then HEAD.
track = "origin/main"
# Run in each new base `update` builds, before it is published. Unset by
# default.
on-update = "cargo build"
```

`on-update` runs with `sh -c`, so it can be a command line, a program or a
script in the repository (`./scripts/build.sh`), with the new base as its
working directory. Its build outputs then come along in every worktree added
from that base. Each base starts as a snapshot of the previous one, with all
its files and build outputs, so a build there is incremental on what the last
one left behind.

If it fails, `update` stops and the previous base stays in place.

`update` does not fetch. Run `git fetch` first to follow a remote, for example
with an alias for `git fetch && git mito update`.

## Shell completion

bash, zsh and fish complete both `git mito` and `git-mito`, including the
repository's worktrees for `remove`. The nix package installs the
completions. Otherwise generate them at shell startup, so they always match
the installed binary:

```sh
source <(COMPLETE=bash git-mito)   # ~/.bashrc
source <(COMPLETE=zsh git-mito)    # ~/.zshrc, after compinit
COMPLETE=fish git-mito | source    # ~/.config/fish/config.fish
```

## Requirements and limitations

- mito invokes the `git` binary (tested with 2.56) for everything that writes a
  worktree's files or index.
- The repository's worktrees and `.mito` must be on one btrfs filesystem.
- Deleting a subvolume without root needs the `user_subvol_rm_allowed` mount
  option. Without it mito falls back to deleting file by file, which can be slow
  for large trees.
- Subvolumes nested inside the base are not snapshotted with it. They show up
  empty in new worktrees.
- Submodules must not be initialized in the base. Run
  `git submodule update --init` in each new worktree instead.
- Some build tools, like `dotnet build` and `cmake`, use absolute paths to detect
  modified files. Because each worktree has a new path these tools would start a
  fresh rebuild every time. One way to work around this is to rebind the worktree
  under a stable path, for example using `bwrap --bind ...`, both in `on-update`
  and worktree.

## Development

```sh
nix develop               # toolchain, crate2nix, libclang for bindgen
                          # (or `direnv allow` to load it on cd)
cargo build
tests/scenario.sh <empty-dir-on-btrfs>   # end-to-end, with git-mito on PATH
nix flake check           # unit tests, shell completions (tests/completions.sh)
                          # and a NixOS VM test (needs KVM)
crate2nix generate        # after changing dependencies; commit Cargo.nix
```
