//! Cached host identity and local process-existence probes.

use std::sync::LazyLock;

/// Existence, not whether a process is running (zombies still exist).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Liveness {
    Alive,
    Gone,
    Unknown,
}

/// Probe one positive PID. Invalid or unrepresentable IDs are unknown, never
/// interpreted as process groups or truncated to a different process.
pub fn process_liveness(pid: i64) -> Liveness {
    match positive_pid(pid) {
        Some(pid) => probe(pid),
        None => Liveness::Unknown,
    }
}

/// Probe a process group by its positive group ID. This includes zombies.
pub fn group_liveness(pgid: i64) -> Liveness {
    match positive_pid(pgid) {
        Some(pgid) => probe(-pgid),
        None => Liveness::Unknown,
    }
}

fn positive_pid(pid: i64) -> Option<libc::pid_t> {
    libc::pid_t::try_from(pid).ok().filter(|pid| *pid > 0)
}

fn probe(pid: libc::pid_t) -> Liveness {
    // SAFETY: kill takes scalar arguments; signal 0 delivers no signal.
    if unsafe { libc::kill(pid, 0) } == 0 {
        Liveness::Alive
    } else {
        probe_error(std::io::Error::last_os_error().raw_os_error())
    }
}

fn probe_error(errno: Option<i32>) -> Liveness {
    match errno {
        Some(libc::EPERM) => Liveness::Alive,
        Some(libc::ESRCH) => Liveness::Gone,
        _ => Liveness::Unknown,
    }
}

/// The local hostname, cached for the lifetime of this process, including
/// failures. Missing, empty or truncated hostnames never establish identity.
pub fn hostname() -> Option<&'static str> {
    static HOSTNAME: LazyLock<Option<String>> = LazyLock::new(read_hostname);
    HOSTNAME.as_deref()
}

fn read_hostname() -> Option<String> {
    // Linux permits 64 bytes and macOS 255, excluding the terminating NUL.
    let mut buffer = [0u8; 256];
    // SAFETY: buffer is writable for exactly the size passed to gethostname.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return None;
    }
    hostname_from_bytes(&buffer)
}

fn hostname_from_bytes(buffer: &[u8]) -> Option<String> {
    let end = buffer.iter().position(|byte| *byte == 0)?;
    let name = String::from_utf8_lossy(&buffer[..end]);
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_pid_is_alive() {
        assert_eq!(
            process_liveness(i64::from(std::process::id())),
            Liveness::Alive
        );
    }

    #[test]
    fn own_group_is_alive() {
        // SAFETY: getpgrp takes no arguments and does not modify process state.
        let pgid = unsafe { libc::getpgrp() };
        assert_eq!(group_liveness(i64::from(pgid)), Liveness::Alive);
    }

    #[test]
    fn reaped_child_is_gone() {
        use std::os::unix::process::CommandExt;

        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = i64::from(child.id());
        child.wait().unwrap();
        assert_eq!(process_liveness(pid), Liveness::Gone);
        assert_eq!(group_liveness(pid), Liveness::Gone);
    }

    #[test]
    fn invalid_ids_are_unknown() {
        for pid in [i64::MIN, -1, 0, i64::from(libc::pid_t::MAX) + 1, i64::MAX] {
            assert_eq!(process_liveness(pid), Liveness::Unknown, "pid {pid}");
            assert_eq!(group_liveness(pid), Liveness::Unknown, "pgid {pid}");
        }
    }

    #[test]
    fn permission_denied_means_alive() {
        assert_eq!(probe_error(Some(libc::EPERM)), Liveness::Alive);
        assert_eq!(probe_error(Some(libc::ESRCH)), Liveness::Gone);
        assert_eq!(probe_error(Some(libc::EINVAL)), Liveness::Unknown);
        assert_eq!(probe_error(None), Liveness::Unknown);
    }

    #[test]
    fn hostname_requires_nonempty_terminated_name() {
        for bytes in [b"".as_slice(), b"\0", b" \t\n\0", b"truncated"] {
            assert_eq!(hostname_from_bytes(bytes), None);
        }
        assert_eq!(
            hostname_from_bytes(b" host.local\n\0ignored").as_deref(),
            Some("host.local")
        );
    }
}
