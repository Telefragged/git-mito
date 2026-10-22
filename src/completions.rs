//! Shell completion for both `git-mito` and `git mito`.
//!
//! `clap_complete`'s dynamic completion registers a completer for the
//! `git-mito` binary. Each shell's git completion finds external commands its
//! own way, so bash and zsh get a little glue appended. fish's git completion
//! runs `complete -C 'git-<command> …'` for every git-* command on PATH, which
//! needs none.

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;

use clap_complete::CompleteEnv;
use clap_complete::env::{Bash, EnvCompleter, Fish, Shells, Zsh};

/// git's completion calls `_git_<command>`, with the words of the whole git
/// command line from index `__git_cmd_idx` on. The local copies leave git's
/// own variables intact. bash-completion loads this file for `git mito` by
/// its `git-mito` name.
const BASH_GLUE: &str = r#"
_git_mito() {
    local COMP_WORDS=(git-mito "${COMP_WORDS[@]:__git_cmd_idx+1}")
    local COMP_CWORD=$((COMP_CWORD - __git_cmd_idx))
    _clap_complete_git_mito git-mito "${COMP_WORDS[COMP_CWORD]}"
}
"#;

/// zsh's _git calls `_git-<command>` with the words from the command on.
/// Autoloaded from fpath as `_git-mito`, this file is that function's first
/// call, so it completes too.
const ZSH_GLUE: &str = r#"
_git-mito() {
    words[1]=git-mito
    _clap_dynamic_completer_git_mito
}
[[ $zsh_eval_context[-1] == loadautofunc ]] && _git-mito "$@"
"#;

/// Prints a shell's registration script when `COMPLETE=<shell>` is set, or
/// completions when the shell calls back; exits in both cases.
pub fn complete(command: impl Fn() -> clap::Command) {
    CompleteEnv::with_factory(|| {
        let mut command = command();
        // Adds the help and version flags, so they are hidden too.
        command.build();
        hide_flags(command)
    })
    .shells(Shells(&[
        &WithGit(&Bash, BASH_GLUE),
        &Fish,
        &WithGit(&Zsh, ZSH_GLUE),
    ]))
    .complete();
}

/// Marks every flag hidden. A hidden candidate is offered only when no
/// visible one matches (see `CompletionCandidate::hide`), so flags come up
/// once a `-` is typed, or where nothing else completes, as in git's
/// completion.
fn hide_flags(command: clap::Command) -> clap::Command {
    command
        .mut_args(|arg| {
            if arg.is_positional() {
                arg
            } else {
                arg.hide(true)
            }
        })
        .mut_subcommands(hide_flags)
}

/// A shell's completer, with glue appended to its registration that hooks
/// `git-mito`'s completion into the shell's git completion.
struct WithGit(&'static dyn EnvCompleter, &'static str);

impl EnvCompleter for WithGit {
    fn name(&self) -> &'static str {
        self.0.name()
    }

    fn is(&self, name: &str) -> bool {
        self.0.is(name)
    }

    fn write_registration(
        &self,
        var: &str,
        name: &str,
        bin: &str,
        completer: &str,
        buf: &mut dyn Write,
    ) -> std::io::Result<()> {
        self.0.write_registration(var, name, bin, completer, buf)?;
        buf.write_all(self.1.as_bytes())
    }

    fn write_complete(
        &self,
        cmd: &mut clap::Command,
        args: Vec<OsString>,
        current_dir: Option<&Path>,
        buf: &mut dyn Write,
    ) -> std::io::Result<()> {
        self.0.write_complete(cmd, args, current_dir, buf)
    }
}
