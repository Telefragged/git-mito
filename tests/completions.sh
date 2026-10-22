#!/usr/bin/env bash
# Checks that bash, zsh and fish complete both `git-mito` and `git mito`
# through each shell's own git completion, with the scripts installed under
# <prefix>/share the way the package installs them.
#
# usage: tests/completions.sh <prefix> <bash_completion>
#   <prefix>           has bin/git-mito and share/{bash-completion,zsh,fish}
#   <bash_completion>  bash-completion's main script
# bash, zsh, fish and git must be on PATH.
set -euo pipefail

prefix=$(realpath "$1")
bash_completion=$2
export PATH=$prefix/bin:$PATH XDG_DATA_DIRS=$prefix/share
HOME=$(realpath "$(mktemp -d)")
export HOME

# `remove` completes linked worktrees, but not bases.
git init -q "$HOME/repo"
cd "$HOME/repo"
git -c user.name=t -c user.email=t commit -q --allow-empty -m init
git worktree add -q ../b
git worktree add -q .mito/worktrees/a
git worktree add -q --detach .mito/bases/x

# Each case is a command line, cursor at the end, and the expected matches.
# Flags only come up once a `-` is typed, or where nothing else completes.
cases=(
  'git-mito |add update remove help'
  'git mito |add update remove help'
  'git mito remove --|--force --help'
  'git mito update |--force --help'
  'git mito re|remove'
  'git mito add --d|--detach'
  'git -C . mito up|update'
  'git mito remove |../b .mito/worktrees/a'
  'git mito remove .m|.mito/worktrees/a'
  "git-mito remove /|$HOME/b $HOME/repo/.mito/worktrees/a"
)

# Calls the completion function bash would run for $1 and prints COMPREPLY.
bash_complete() {
  bash --norc --noprofile -s -- "$bash_completion" "$1" <<'EOF'
source "$1"
line=$2
read -ra COMP_WORDS <<<"$line"
[[ $line == *' ' ]] && COMP_WORDS+=('')
COMP_CWORD=$((${#COMP_WORDS[@]} - 1)) COMP_LINE=$line COMP_POINT=${#line} COMP_TYPE=9
_comp_load -- "${COMP_WORDS[0]}"
spec=$(complete -p "${COMP_WORDS[0]}")
fn=${spec##*-F } fn=${fn%% *}
"$fn" "${COMP_WORDS[0]}" "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD - 1]}"
echo "${COMPREPLY[*]}"
EOF
}

# Types $1 into an interactive zsh, presses Tab and prints the matches. The
# markers are built at runtime, so the terminal's echo of the setup commands
# never matches them.
zsh_complete() {
  zsh -f -s -- "$prefix/share/zsh/site-functions" "$1" <<'EOF'
zmodload zsh/zpty
zpty z zsh -f -i
zpty -w z "fpath=($1 \$fpath); autoload -U compinit; compinit -u"
zpty -w z 'm=match; compadd() { builtin compadd -O _m "$@"; builtin compadd "$@"; print -rl -- "${_m[@]/#/${(U)m}:}"; }'
zpty -w z 'comppostfuncs=(_done); _done() { print -r -- ${(U)m}-DONE; }; print ${(U)m}-READY'
while zpty -r z out; do [[ $out == MATCH-READY* ]] && break; done
zpty -w -n z "$2"$'\t'
typeset -U matches=()
while zpty -r z out; do
  [[ $out == *MATCH-DONE* ]] && break
  [[ $out == *MATCH:* ]] && matches+=("${${out#*MATCH:}%%[[:cntrl:] ]*}")
done
zpty -d z
print -r -- $matches
EOF
}

fish_complete() {
  fish -c 'complete -C $argv[1] | string replace -r "\t.*" ""' "$1"
}

# The shells order their matches differently, so compare them sorted.
sorted() { tr ' ' '\n' | sort | paste -sd ' '; }

failed=0
for case in "${cases[@]}"; do
  line=${case%%|*}
  want=$(sorted <<<"${case#*|}")
  for shell in bash zsh fish; do
    got=$("${shell}_complete" "$line" | sorted)
    if [[ $got != "$want" ]]; then
      echo "FAIL $shell [$line]: got '$got', want '$want'"
      failed=1
    fi
  done
done
((failed == 0)) && echo "completions: all cases pass"
exit "$failed"
