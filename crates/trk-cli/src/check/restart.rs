//! Re-exec at queue boundaries, carrying only the current pass's results.

use std::fs::File;
use std::io::{Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;
use trk_core::error::Result;

use crate::env::Env;

const HANDOFF: &str = "_TRK_CHECK_RESTART";

/// Called first in main, before any threads or environment readers exist.
/// The private handoff variable is restored to its original (possibly non-UTF8)
/// value before parsing arguments or spawning subprocesses.
pub(crate) fn resume() -> std::io::Result<Vec<Value>> {
    let Some(value) = std::env::var_os(HANDOFF) else {
        return Ok(vec![]);
    };
    let Some((pid, fd)) = value.to_str().and_then(|s| s.split_once(':')) else {
        return Ok(vec![]);
    };
    if pid.parse::<u32>().ok() != Some(std::process::id()) {
        return Ok(vec![]);
    }
    let fd = fd
        .parse::<i32>()
        .ok()
        .filter(|fd| *fd >= 3)
        .ok_or_else(|| std::io::Error::other("invalid check restart descriptor"))?;
    // SAFETY: fcntl only inspects the descriptor; validate before taking ownership.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: this descriptor was transferred by Executable::restart, not
    // borrowed from another Rust owner; exec discarded the previous owners.
    let file = unsafe { File::from_raw_fd(fd) };
    let (original, results): (Option<Vec<u8>>, Vec<Value>) = serde_json::from_reader(file)?;
    // SAFETY: main calls this before any other work, while single-threaded.
    unsafe {
        match original {
            Some(value) => std::env::set_var(HANDOFF, std::ffi::OsString::from_vec(value)),
            None => std::env::remove_var(HANDOFF),
        }
    }
    Ok(results)
}

pub(super) struct Executable {
    path: PathBuf,
    // Keep the original inode alive so an installer cannot recycle its identity.
    _original: File,
    identity: (u64, u64, u64, i64, i64, i64, i64),
}

impl Executable {
    pub(super) fn new() -> std::io::Result<Self> {
        let path = std::env::current_exe()?;
        let original = File::open(&path)?;
        let identity = Self::identity(&original.metadata()?);
        Ok(Self {
            path,
            _original: original,
            identity,
        })
    }

    fn identity(m: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
        (
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    }

    pub(super) fn changed(&self) -> std::io::Result<bool> {
        let current = match std::fs::metadata(&self.path) {
            Ok(m) => m,
            // An installer may temporarily remove the path before renaming.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        Ok(Self::identity(&current) != self.identity)
    }

    pub(super) fn restart(&self, env: &Env, results: &[Value]) -> Result<()> {
        // Never abandon durable ownership if a pass's cleanup/release failed.
        if env
            .store()?
            .read(None, |c| c.check_lock())?
            .is_some_and(|(pid, _, _, _)| pid == i64::from(std::process::id()))
        {
            return Err(std::io::Error::other("cannot restart while owning the check lock").into());
        }
        // Anonymous storage avoids argv/environment size limits and stale files.
        // Only this one descriptor survives exec; the new main consumes it.
        let mut file = tempfile::tempfile()?;
        serde_json::to_writer(
            &mut file,
            &(
                std::env::var_os(HANDOFF).map(|s| s.as_bytes().to_vec()),
                results,
            ),
        )?;
        file.rewind()?;
        let fd = file.as_raw_fd();
        // SAFETY: fd is a live, owned descriptor. Clear close-on-exec only on
        // the handoff file, after all subprocesses and store handles are gone.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, 0) } == -1 {
            return Err(std::io::Error::last_os_error().into());
        }
        std::io::stdout().flush()?;
        std::io::stderr().flush()?;
        let mut args = std::env::args_os();
        let mut command = Command::new(&self.path);
        if let Some(arg0) = args.next() {
            command.arg0(arg0);
        }
        let error = command
            .args(args)
            .env(HANDOFF, format!("{}:{fd}", std::process::id()))
            .exec();
        Err(error.into())
    }
}
