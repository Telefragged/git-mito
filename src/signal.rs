//! Interruption. A signal that kills mito outright skips the rollback of
//! whatever worktree it is half-way through making, leaving it registered
//! (locked, for a base) and its subvolume behind. Instead, SIGINT, SIGTERM and
//! SIGHUP are caught and only noted: the child git or on-update command gets
//! them too, if sent to the process group as a terminal does, and fails; a
//! blocking `flock` fails with EINTR; and [`check`], in
//! [`WorktreeGuard::commit`](crate::worktree::WorktreeGuard::commit), refuses to keep the
//! worktree, so the error path rolls back as for any other failure.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn note(_: nix::libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Catches the signals that would interrupt mito. A handler, rather than
/// ignoring them, so that children still get the default disposition. No
/// `SA_RESTART`: a blocking lock wait must end. A signal mito was started
/// with ignored, as by `nohup`, stays ignored, for its children too.
pub fn install() -> Result<()> {
    let action = SigAction::new(SigHandler::Handler(note), SaFlags::empty(), SigSet::empty());
    for signal in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
        // SAFETY: the handler only stores to an atomic, and an ignored signal
        // gets its own disposition back.
        let old = unsafe { sigaction(signal, &action) }?;
        if matches!(old.handler(), SigHandler::SigIgn) {
            unsafe { sigaction(signal, &old) }?;
        }
    }
    Ok(())
}

/// Fails if a signal has arrived.
pub fn check() -> Result<()> {
    if INTERRUPTED.load(Ordering::SeqCst) {
        bail!("interrupted");
    }
    Ok(())
}
