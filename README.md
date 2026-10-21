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

## Commands

- `git mito update [--force]` builds a new base at the tracked revision next to
  the current one and swaps it in atomically; `add` keeps working meanwhile.
  The branch `mito/base` is moved to the base's commit.
- `git mito add [-b <branch> | -B <branch> | --detach] [--lock] <path> [<commit-ish>]`
  takes `git worktree add`'s options and resolves them the same way, so it
  defaults to `HEAD`. When that isn't the base's commit, the difference from
  the base is checked out; run `update` regularly to keep it small.
- `git mito remove [--force] <path>` deletes the worktree's subvolume in one
  step where permitted. `git worktree remove` also works, deleting file by
  file.

## Configuration

Optional, in `.mito.toml` at the root of the main worktree. It doesn't need to
be tracked.

```toml
# Where mito keeps its state, relative to the repository root.
dir = ".mito"

[base]
# Revision the base follows. Defaults to origin/HEAD, then HEAD.
track = "origin/main"
```

`update` does not fetch; run `git fetch` first to follow a remote, for example
with an alias for `git fetch && git mito update`.

## Layout

```text
.mito/
  base -> bases/<id>   the current base
  bases/<id>/          one read-only subvolume per base, each a locked worktree
  lock, update.lock    flocks
```

A published base never changes (its subvolume is read-only), so its files,
index and `HEAD` always agree.
`add` holds `lock` shared while it reads `base` and copies from it; `update`
holds it exclusively only to swap the symlink, then deletes old bases. mito
adds the state directory to `.git/info/exclude`.

## Requirements and limitations

- mito runs the `git` binary (tested with 2.56) for everything that writes a
  worktree's files or index, so filters such as Git LFS and line-ending
  conversion apply as usual. Reads go through gix, which can't read reftable
  refs yet.
- The repository's worktrees and `.mito` must be on one btrfs filesystem.
- Deleting a subvolume without root needs the `user_subvol_rm_allowed` mount
  option. Without it mito falls back to deleting file by file, which is slow
  for large trees.
- Subvolumes nested inside the base are not snapshotted with it; they show up
  empty in new worktrees.
- Build tools that record absolute paths may rebuild in a new worktree, since
  each worktree lives at its own path. Running the build under `bwrap` with the
  worktree bound to a stable path works around this.

## Development

```sh
nix develop               # toolchain, crate2nix, libclang for bindgen
cargo build
tests/scenario.sh <empty-dir-on-btrfs>   # end-to-end, with git-mito on PATH
nix flake check           # unit tests and a NixOS VM test (needs KVM)
crate2nix generate        # after changing dependencies; commit Cargo.nix
```
