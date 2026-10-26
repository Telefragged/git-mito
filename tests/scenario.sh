#!/usr/bin/env bash
# End-to-end scenario for git-mito. Runs in a fresh directory on btrfs:
#
#   tests/scenario.sh <empty-dir-on-btrfs>
#
# Expects `git-mito` on PATH. Used by the NixOS VM test and runnable locally.
set -euo pipefail

work=${1:?usage: scenario.sh <empty-dir-on-btrfs>}
mkdir -p "$work"
work=$(cd "$work" && pwd)
cd "$work"

export GIT_AUTHOR_NAME=mito GIT_AUTHOR_EMAIL=mito@example.com
export GIT_COMMITTER_NAME=mito GIT_COMMITTER_EMAIL=mito@example.com
export GIT_CONFIG_GLOBAL=/dev/null

step() { printf '\n=== %s\n' "$*"; }
fail() {
  echo "FAIL: $*" >&2
  exit 1
}
assert_clean() {
  local status
  status=$(git -C "$1" status --porcelain)
  [[ -z $status ]] || fail "$1 is not clean: $status"
}
assert_eq() { [[ $1 == "$2" ]] || fail "expected '$2', got '$1' ($3)"; }
is_subvolume() { [[ $(stat -c %i "$1") == 256 ]]; }

step "set up a repository"
git init -q -b main repo
cd repo
mkdir -p src/deep/er
for i in $(seq 1 300); do echo "file $i" >"src/f$i"; done
echo deep >src/deep/er/leaf
echo keep >keep
echo gone >gone
# Type changes in the second commit: a symlink becomes a file and vice versa.
ln -s keep link
echo plain >becomes-link
printf 'target/\n' >.gitignore
git add -A
git commit -qm one

step "first update creates the base and runs on-update in it"
# on-update builds an ignored artifact and counts its runs; each base is a
# snapshot of the last, so the count carries over.
echo /.mito.toml >>.git/info/exclude
cat >.mito.toml <<'TOML'
[base]
on-update = "mkdir -p target && echo artifact >target/out && pwd >target/ran-in && echo run >>target/runs"
TOML
git mito update
base=$(readlink -f .mito/base)
assert_eq "$(cat "$base/target/ran-in")" "$base" "on-update ran in the new base"
assert_eq "$(wc -l <"$base/target/runs")" 1 "on-update ran once"
is_subvolume "$base" || fail "base is not a subvolume"
assert_clean "$base"
assert_clean .
assert_eq "$(git -C "$base" rev-parse HEAD)" "$(git rev-parse main)" "base commit"
assert_eq "$(git rev-parse mito/base)" "$(git rev-parse main)" "mito/base ref"
git -C "$base" symbolic-ref -q HEAD && fail "base HEAD is not detached"

step "update is a no-op when the base is current"
git mito update | grep -q "already"

step "add snapshots the base"
git mito add ../wt1
is_subvolume ../wt1 || fail "wt1 is not a subvolume"
assert_eq "$(git -C ../wt1 branch --show-current)" wt1 "wt1 branch"
assert_eq "$(git -C ../wt1 rev-parse HEAD)" "$(git rev-parse main)" "wt1 commit"
assert_eq "$(cat ../wt1/target/out)" artifact "artifact carried over"
assert_eq "$(git -C ../wt1 rev-parse --git-dir)" "$work/repo/.git/worktrees/wt1" "wt1 gitdir"
assert_clean ../wt1
# The copied index must already match: no entry should need refreshing.
git -C ../wt1 diff-files --quiet || fail "wt1 index is stale"
echo change >>../wt1/keep
assert_eq "$(git -C ../wt1 status --porcelain)" " M keep" "wt1 sees its own edits"
assert_clean "$base"

step "add at a commit other than the base checks out the difference"
echo changed >keep
git rm -q gone
echo new >src/deep/new
rm link becomes-link
echo "no longer a link" >link
ln -s keep becomes-link
git add -A
git commit -qm two
git mito add -b feat ../wt2 main
assert_eq "$(git -C ../wt2 rev-parse HEAD)" "$(git rev-parse main)" "wt2 commit"
assert_eq "$(cat ../wt2/keep)" changed "wt2 modified file"
[[ -e ../wt2/gone ]] && fail "wt2 still has a deleted file"
assert_eq "$(cat ../wt2/src/deep/new)" new "wt2 added file"
[[ -L ../wt2/link ]] && fail "wt2 link did not become a file"
assert_eq "$(cat ../wt2/link)" "no longer a link" "wt2 symlink turned file"
[[ -L ../wt2/becomes-link ]] || fail "wt2 file did not become a link"
assert_clean ../wt2
git -C ../wt2 diff-files --quiet || fail "wt2 index is stale"

step "add defaults to HEAD, like git worktree add"
git mito add ../wt-head
assert_eq "$(git -C ../wt-head branch --show-current)" wt-head "wt-head branch"
assert_eq "$(git -C ../wt-head rev-parse HEAD)" "$(git rev-parse main)" "wt-head commit"
assert_clean ../wt-head
git mito add --detach ../wt3
git -C ../wt3 symbolic-ref -q HEAD && fail "wt3 HEAD is not detached"
assert_eq "$(git -C ../wt3 rev-parse HEAD)" "$(git rev-parse main)" "wt3 commit"
(cd ../wt2 && git mito add -b from-wt2 ../wt-sub)
assert_eq "$(git -C ../wt-sub rev-parse HEAD)" "$(git -C ../wt2 rev-parse HEAD)" "HEAD of the current worktree"

step "add of an existing branch"
git branch -q existing main
git mito add ../wt4 existing
assert_eq "$(git -C ../wt4 branch --show-current)" existing "wt4 branch"

step "add refuses a non-empty path and cleans up after failure"
mkdir ../occupied && touch ../occupied/x
git mito add ../occupied && fail "add into a non-empty directory succeeded"
git mito add -b feat ../wt5 && fail "add with an existing -b branch succeeded"
git rev-parse -q --verify feat >/dev/null || fail "failed add deleted the existing branch"
[[ -e ../wt5 ]] && fail "failed add left its directory behind"
git worktree list | grep -q wt5 && fail "failed add left its registration behind"

step "a failed add rolls back its worktree and branch"
# A snapshot can't land outside btrfs, so these fail after git has registered
# the worktree and created or moved its branch.
elsewhere=$(mktemp -d /dev/shm/mito-XXXXXX)
if [[ $(stat -f -c %T "$elsewhere") != btrfs ]]; then
  git mito add "$elsewhere/fresh" && fail "add outside btrfs succeeded"
  git rev-parse -q --verify fresh >/dev/null && fail "failed add left its new branch behind"
  git branch -q movable main
  git mito add -B movable "$elsewhere/moved" main~1 && fail "add outside btrfs succeeded"
  assert_eq "$(git rev-parse movable)" "$(git rev-parse main)" "failed add -B restores the branch"
  git worktree list | grep -q "$elsewhere" && fail "failed add left its registration behind"
  [[ -z $(ls -A "$elsewhere") ]] || fail "failed add left files behind"
fi
rm -rf "$elsewhere"

step "update moves the base and deletes the old one"
old_base=$base
git mito update
base=$(readlink -f .mito/base)
[[ $base != "$old_base" ]] || fail "base did not move"
[[ -e $old_base ]] && fail "old base still exists"
assert_eq "$(git -C "$base" rev-parse HEAD)" "$(git rev-parse main)" "new base commit"
assert_eq "$(git rev-parse mito/base)" "$(git rev-parse main)" "mito/base ref"
assert_eq "$(cat "$base/target/ran-in")" "$base" "on-update ran in the new base"
assert_eq "$(wc -l <"$base/target/runs")" 2 "on-update built on the previous base"
assert_clean "$base"
assert_eq "$(git worktree list | grep -c '\.mito/bases/')" 1 "one base registered"
assert_eq "$(git -C ../wt1 status --porcelain)" " M keep" "wt1 untouched by update"

step "add from the updated base"
git mito add ../wt6
assert_eq "$(git -C ../wt6 rev-parse HEAD)" "$(git rev-parse main)" "wt6 commit"
assert_clean ../wt6
git -C ../wt6 diff-files --quiet || fail "wt6 index is stale"

step "remove"
git mito remove ../wt6
[[ -e ../wt6 ]] && fail "wt6 still exists"
git mito remove ../wt1 && fail "removed a dirty worktree"
git mito remove --force ../wt1
git mito remove "$base" && fail "removed the base without --force"
git worktree remove ../wt3
[[ -e ../wt3 ]] && fail "git worktree remove left wt3 behind"

step "a failing on-update leaves the base as it was"
good_base=$base
cp .mito.toml ../mito.toml.good
printf '[base]\non-update = "exit 3"\n' >.mito.toml
git mito update --force && fail "update with a failing on-update succeeded"
assert_eq "$(readlink -f .mito/base)" "$good_base" "base pointer unchanged"
assert_eq "$(ls .mito/bases | wc -l)" 1 "failed base deleted"
assert_eq "$(git worktree list | grep -c '\.mito/bases/')" 1 "failed base unregistered"
mv ../mito.toml.good .mito.toml

step "concurrent updates: the newer revision wins, not the last to finish"
# A commit holding a `hold` file has a on-update that signals on the `started`
# FIFO, then blocks reading `release` until the test lets it go.
mkfifo ../started ../release
mv .mito.toml ../mito.toml.saved
cat >.mito.toml <<TOML
[base]
track = "tracked"
on-update = "if [ -e hold ]; then echo >'$work/started'; read -r _ <'$work/release'; fi"
TOML
base_commit() { git -C "$(readlink -f .mito/base)" rev-parse HEAD; }
# Commits a held commit and a free one on top, and starts updating to the held
# one in the background (logging to $1). Returns once that update is in its
# on-update, so it has resolved the held commit.
race() {
  touch hold && git add hold && git commit -qm held
  held=$(git rev-parse HEAD)
  git rm -q hold && git commit -qm free
  free=$(git rev-parse HEAD)
  git branch -f tracked "$held"
  git mito update >"$1" 2>&1 &
  pid=$!
  read -r _ <../started
}

race ../update-overtaken.log
git branch -f tracked "$free"
git mito update
echo >../release
wait "$pid"
assert_eq "$(base_commit)" "$free" "the newer base stays published"
grep -q "Discarded" ../update-overtaken.log || fail "the older, held update didn't discard its base"
assert_eq "$(ls .mito/bases | wc -l)" 1 "the discarded base is gone"

# With no other update running, a base the ref moved past still publishes,
# since it's ahead of the published one; the next update catches up.
race ../update-ahead.log
git branch -f tracked "$free"
echo >../release
wait "$pid"
assert_eq "$(base_commit)" "$held" "an update ahead of the base publishes"
git mito update
assert_eq "$(base_commit)" "$free" "the next update catches up"
assert_eq "$(ls .mito/bases | wc -l)" 1 "replaced bases are deleted"
mv ../mito.toml.saved .mito.toml
git branch -q -D tracked
rm ../started ../release

step "bases are read-only, their worktrees are not"
base=$(readlink -f .mito/base)
touch "$base/x" 2>/dev/null && fail "base is writable"
git mito add ../wt7
touch ../wt7/x || fail "worktree of a read-only base is not writable"
git mito update --force
[[ -e $base ]] && fail "read-only base was not deleted"

step "a base stays published when moving mito/base fails"
# refs/heads/mito, a branch, keeps refs/heads/mito/base from being written.
git branch -q -D mito/base
git branch -q mito
git mito update --force 2>../update-ref.log
grep -q "warning: moving the mito/base branch" ../update-ref.log || fail "no warning about mito/base"
assert_clean "$(readlink -f .mito/base)"
git mito add --detach ../wt-ref
git mito remove ../wt-ref
git branch -q -D mito
git mito update --force

step "update deletes the old base even if reporting fails"
# Writing to a full device fails, which made `println!` panic before the
# old base was deleted.
git mito update --force >/dev/full 2>/dev/null || true
assert_eq "$(ls .mito/bases | wc -l)" 1 "the old base is gone"
assert_eq "$(git worktree list | grep -c '\.mito/bases/')" 1 "one base registered"

step "concurrent updates to the same commit both succeed"
git mito update --force >../update-a.log 2>&1 &
pid=$!
git mito update --force >../update-b.log 2>&1 || fail "an update failed: $(cat ../update-b.log)"
wait "$pid" || fail "an update failed: $(cat ../update-a.log)"
assert_clean "$(readlink -f .mito/base)"
assert_eq "$(ls .mito/bases | wc -l)" 1 "replaced bases are deleted"
assert_eq "$(git worktree list | grep -c '\.mito/bases/')" 1 "one base registered"

step "a split index carries over"
# A new commit, so that `reset --hard` writes the base's index split, as
# the shared index it leaves beside it shows.
git config core.splitIndex true
git commit -q --allow-empty -m split
git mito update
ls .git/worktrees/"$(basename "$(readlink .mito/base)")"/sharedindex.* >/dev/null ||
  fail "the base's index was never split"
git mito add --detach ../wt-split
assert_clean ../wt-split
git mito remove ../wt-split
git config --unset core.splitIndex
git commit -q --allow-empty -m unsplit
git mito update

step "an interrupted update rolls back its base"
mkfifo ../started
cp .mito.toml ../mito.toml.saved
printf '[base]\non-update = "echo >%q; sleep 30"\n' "$work/started" >.mito.toml
base=$(readlink -f .mito/base)
# In its own process group, which a terminal's Ctrl-C signals as a whole.
setsid git-mito update --force >../update-int.log 2>&1 &
pid=$!
read -r _ <../started
kill -TERM -- -"$pid"
wait "$pid" && fail "an interrupted update succeeded"
assert_eq "$(readlink -f .mito/base)" "$base" "base pointer unchanged"
assert_eq "$(ls .mito/bases | wc -l)" 1 "interrupted base deleted"
assert_eq "$(git worktree list | grep -c '\.mito/bases/')" 1 "interrupted base unregistered"
mv ../mito.toml.saved .mito.toml
rm ../started

step "a same-sized change by on-update shows in worktrees"
# Within the second `reset --hard` wrote the index, so git can't tell from the
# file's mtime against the worktree's copy of the index, which is newer; only
# the base's index, refreshed after on-update, records the change.
cp .mito.toml ../mito.toml.saved
printf '[base]\non-update = "echo CHANGED >keep"\n' >.mito.toml
git mito update --force
sleep 1
git mito add --detach ../wt-racy
assert_eq "$(git -C ../wt-racy status --porcelain)" " M keep" "wt-racy sees on-update's change"
git mito remove --force ../wt-racy
mv ../mito.toml.saved .mito.toml
# Rebuilt at the same commit, a base keeps on-update's changes; a new commit
# resets them.
git commit -q --allow-empty -m racy
git mito update

step "remove --force removes the base"
base=$(readlink -f .mito/base)
git mito remove --force .mito/base
[[ -e $base ]] && fail "the base still exists"
[[ -L .mito/base ]] && fail "the base pointer still exists"
git rev-parse -q --verify mito/base >/dev/null && fail "the mito/base branch still exists"
git mito add ../wt-nobase && fail "add without a base succeeded"
git mito update
git mito add ../wt-rebased
assert_clean ../wt-rebased

step "worktree bookkeeping is consistent"
git worktree prune --dry-run -v 2>&1 | grep -q . && fail "stale worktree registrations"
git fsck --no-progress

echo
echo "scenario passed"
