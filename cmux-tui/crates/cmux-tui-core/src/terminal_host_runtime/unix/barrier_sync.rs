//! Ordered syncs for terminal-host discovery files.
//!
//! The host record, the publication lock and the deprecated workspace
//! mirror do not need a drive-cache flush: a record lost to a power cut
//! makes its terminal `host_lost` at startup, which is already handled,
//! and terminal ids are random with no never-reuse promise. They need only
//! write order (data before the rename that publishes it), which
//! F_BARRIERFSYNC gives at a fraction of F_FULLFSYNC's cost (0.5 ms against
//! 3.9 ms on a Mac mini). The exit record, the SQLite registry and the
//! session journal keep the full sync.

use std::fs::File;
use std::io;
use std::path::Path;

/// Order `file`'s writes before later writes. Falls back to the full sync
/// where the filesystem refuses the barrier, and on other platforms.
pub(super) fn barrier_sync(file: &File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: fcntl on a descriptor this File owns, with no pointer
        // argument.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_BARRIERFSYNC) } != -1 {
            return Ok(());
        }
    }
    file.sync_all()
}

/// [`barrier_sync`] for the directory entry changes in `dir`.
pub(super) fn barrier_sync_dir(dir: &Path) -> io::Result<()> {
    barrier_sync(&File::open(dir)?)
}
