//! Port of `src/os/systemd.zig`.
//!
//! STATUS: ported

/// Returns true if the program was launched as a systemd service.
///
/// On Linux, this returns true if the program was launched as a systemd
/// service. It will return false if Ghostty was launched any other way.
///
/// For other platforms and app runtimes, this returns false.
pub fn launched_by_systemd() -> bool {
    #[cfg(target_os = "linux")]
    {
        // On Linux, systemd sets the `INVOCATION_ID` (v232+) and the
        // `JOURNAL_STREAM` (v231+) environment variables. If these
        // environment variables are not present we were not launched by
        // systemd.
        if std::env::var_os("INVOCATION_ID").is_none() {
            return false;
        }
        if std::env::var_os("JOURNAL_STREAM").is_none() {
            return false;
        }

        // If `INVOCATION_ID` and `JOURNAL_STREAM` are present, check to make
        // sure that our parent process is actually `systemd`, not some other
        // terminal emulator that doesn't clean up those environment
        // variables.
        // SAFETY: getppid has no preconditions.
        let ppid = unsafe { libc::getppid() };
        if ppid == 1 {
            return true;
        }

        // If the parent PID is not 1 we need to check to see if we were
        // launched by a user systemd daemon. Do that by checking the
        // `/proc/<ppid>/comm` to see if it ends with `systemd`.
        let comm_path = format!("/proc/{ppid}/comm");
        let mut comm_file = match std::fs::File::open(&comm_path) {
            Ok(f) => f,
            Err(_) => {
                log::error!(target: "systemd", "unable to open '{comm_path}' for reading");
                return false;
            }
        };

        // The maximum length of the command name is defined by
        // `TASK_COMM_LEN` in the Linux kernel. This is usually 16
        // bytes at the time of writing (Jun 2025) so its set to that.
        // Also, since we only care to compare to "systemd", anything
        // longer can be assumed to not be systemd.
        const TASK_COMM_LEN: usize = 16;
        let mut comm_data_buf = [0u8; TASK_COMM_LEN];
        let mut comm_size = 0;
        while comm_size < comm_data_buf.len() {
            match std::io::Read::read(&mut comm_file, &mut comm_data_buf[comm_size..]) {
                Ok(0) => break,
                Ok(n) => comm_size += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    log::error!(target: "systemd", "problems reading from '{comm_path}'");
                    return false;
                }
            }
        }
        let mut comm_data = &comm_data_buf[..comm_size];
        while let Some(rest) = comm_data.strip_suffix(b"\n") {
            comm_data = rest;
        }

        return comm_data == b"systemd";
    }

    // No other system supports systemd so always return false.
    #[allow(unreachable_code)]
    false
}

/// systemd notifications. Used by Ghostty to inform systemd of the state of
/// the process. Currently only used to notify systemd that we are ready and
/// that configuration reloading has started.
///
/// See: https://www.freedesktop.org/software/systemd/man/latest/sd_notify.html
///
/// These functions were re-implemented instead of using the `libsystemd`
/// library to avoid the complexity of another external dependency, as well
/// as to ensure minimal impact on non-Linux systems (like FreeBSD) that will
/// never support `systemd`.
///
/// Linux systems that do not use `systemd` should not be impacted as they
/// should never start Ghostty with the `NOTIFY_SOCKET` environment variable
/// set and these functions essentially become a no-op.
///
/// See `systemd`'s [Interface Portability and Stability Promise](https://systemd.io/PORTABILITY_AND_STABILITY/)
/// for assurances that the interfaces used here will be supported and stable
/// for the long term.
pub mod notify {
    /// Send the given message to the UNIX socket specified in the
    /// NOTIFY_SOCKET environment variable. If there NOTIFY_SOCKET
    /// environment variable does not exist then no message is sent.
    fn send(message: &[u8]) {
        // systemd is Linux-only so this is a no-op anywhere else
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStrExt;

            // Get the socket address that should receive notifications.
            let Some(socket_path) = std::env::var_os("NOTIFY_SOCKET") else {
                return;
            };
            send_to(socket_path.as_bytes(), message);
        }

        #[cfg(not(target_os = "linux"))]
        let _ = message;
    }

    /// Send `message` to the notify socket at `socket_path` (the value of
    /// `NOTIFY_SOCKET`).
    #[cfg(target_os = "linux")]
    fn send_to(socket_path: &[u8], message: &[u8]) {
        {
            // If the socket address is an empty string return.
            if socket_path.is_empty() {
                return;
            }

            // The socket address must be a path or an abstract socket.
            if socket_path[0] != b'/' && socket_path[0] != b'@' {
                log::warn!(
                    target: "systemd",
                    "only AF_UNIX sockets with path or abstract namespace addresses are supported!"
                );
                return;
            }

            // SAFETY: sockaddr_un is plain old data.
            let mut socket_address: libc::sockaddr_un = unsafe { std::mem::zeroed() };

            // Error out if the supplied socket path is too long.
            if socket_address.sun_path.len() < socket_path.len() {
                log::warn!(target: "systemd", "NOTIFY_SOCKET path is too long!");
                return;
            }

            socket_address.sun_family = libc::AF_UNIX as libc::sa_family_t;

            for (d, s) in socket_address.sun_path.iter_mut().zip(socket_path) {
                *d = *s as libc::c_char;
            }
            if socket_path.len() < socket_address.sun_path.len() {
                socket_address.sun_path[socket_path.len()] = 0;
            }

            // SAFETY: plain socket syscall.
            let socket =
                unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
            if socket < 0 {
                log::warn!(target: "systemd", "creating socket failed: {}", std::io::Error::last_os_error());
                return;
            }

            struct CloseOnDrop(libc::c_int);
            impl Drop for CloseOnDrop {
                fn drop(&mut self) {
                    // SAFETY: closing our socket.
                    unsafe { libc::close(self.0) };
                }
            }
            let _guard = CloseOnDrop(socket);

            let addr_len =
                std::mem::offset_of!(libc::sockaddr_un, sun_path) + socket_address.sun_path.len();
            // SAFETY: valid socket and address.
            let rc = unsafe {
                libc::connect(
                    socket,
                    &socket_address as *const libc::sockaddr_un as *const libc::sockaddr,
                    addr_len as libc::socklen_t,
                )
            };
            if rc != 0 {
                log::warn!(target: "systemd", "unable to connect to notify socket: {}", std::io::Error::last_os_error());
                return;
            }

            // SAFETY: valid socket and buffer.
            let rc = unsafe { libc::write(socket, message.as_ptr().cast(), message.len()) };
            if rc < 0 {
                log::warn!(target: "systemd", "unable to write to notify socket: {}", std::io::Error::last_os_error());
                return;
            }
            let written = rc as usize;
            if written < message.len() {
                log::warn!(target: "systemd", "short write to notify socket: {} < {}", written, message.len());
            }
        }
    }

    /// Tell systemd that we are ready or that we are finished reloading.
    /// See: https://www.freedesktop.org/software/systemd/man/latest/sd_notify.html#READY=1
    pub fn ready() {
        if !cfg!(target_os = "linux") {
            return;
        }

        send(b"READY=1");
    }

    /// Tell systemd that we have started reloading our configuration.
    /// See: https://www.freedesktop.org/software/systemd/man/latest/sd_notify.html#RELOADING=1
    /// and: https://www.freedesktop.org/software/systemd/man/latest/sd_notify.html#MONOTONIC_USEC=%E2%80%A6
    pub fn reloading() {
        #[cfg(target_os = "linux")]
        {
            let mut ts = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: valid out pointer.
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
            let now = ts.tv_sec as i64 * 1_000_000 + ts.tv_nsec as i64 / 1_000;

            let message = format!("RELOADING=1\nMONOTONIC_USEC={now}");
            send(message.as_bytes());
        }
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn send_to_for_test(socket_path: &[u8], message: &[u8]) {
        send_to(socket_path, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launched_by_systemd_requires_env() {
        if std::env::var_os("INVOCATION_ID").is_none()
            || std::env::var_os("JOURNAL_STREAM").is_none()
        {
            assert!(!launched_by_systemd());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn notify_sends_to_socket() {
        use std::os::unix::net::UnixDatagram;

        let tmp = crate::os::temp_dir::TempDir::init().unwrap();
        let sock_path = tmp.path().join("notify.sock");
        let receiver = UnixDatagram::bind(&sock_path).unwrap();

        let path = sock_path.as_os_str().as_encoded_bytes();
        notify::send_to_for_test(path, b"READY=1");
        notify::send_to_for_test(path, b"STATUS=test");
        // Unsupported addresses are ignored.
        notify::send_to_for_test(b"relative.sock", b"nope");

        let mut buf = [0u8; 128];
        let n = receiver.recv(&mut buf).unwrap();
        assert_eq!(b"READY=1", &buf[..n]);
        let n = receiver.recv(&mut buf).unwrap();
        assert_eq!(b"STATUS=test", &buf[..n]);

        // Without NOTIFY_SOCKET these are no-ops.
        if std::env::var_os("NOTIFY_SOCKET").is_none() {
            notify::ready();
            notify::reloading();
        }
    }
}
