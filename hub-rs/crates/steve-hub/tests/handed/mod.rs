//! A Hub's port that the test holds itself (#429): bound before the Hub starts and handed to it as
//! a listening socket (`steve_hub::config::Handed`, systemd's socket activation), so no other test
//! can take it between choosing it and the Hub binding it, nor while the Hub restarts.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::Command;

/// A listening socket on a free loopback port, kept for as long as the test needs the port.
pub struct Socket(std::net::TcpListener);

impl Socket {
    pub fn new() -> Socket {
        Socket(std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback port"))
    }

    pub fn port(&self) -> u16 {
        self.0.local_addr().expect("socket address").port()
    }
}

/// A command that starts `program` with sockets handed to it: use it as the [`Command`] it
/// derefs to. Copies of the sockets that its child inherits live as long as this.
pub struct Handing {
    command: Command,
    _copies: Vec<OwnedFd>,
}

impl std::ops::Deref for Handing {
    type Target = Command;
    fn deref(&self) -> &Command {
        &self.command
    }
}

impl std::ops::DerefMut for Handing {
    fn deref_mut(&mut self) -> &mut Command {
        &mut self.command
    }
}

/// `program` with `sockets` as its listening sockets, each under its name (`LISTEN_FDNAMES`).
///
/// The child inherits copies of the sockets (made without close-on-exec, above descriptor
/// 3 + their count), and `bash` puts them on descriptors 3.. as it execs `program` in the same
/// process: the child's id is the program's, the `LISTEN_PID` the Hub expects. No `pre_exec`, so
/// std spawns with `posix_spawn`: a `fork` in a multithreaded test can crash on macOS before it
/// gets to `exec`.
pub fn command(program: &str, sockets: &[(&str, &Socket)]) -> Handing {
    let n = sockets.len() as libc::c_int;
    let copies: Vec<OwnedFd> = sockets
        .iter()
        .map(|(_, s)| {
            // SAFETY: a copy of a socket this process owns, at a free descriptor of 3 + n or more;
            // the copy is owned (and closed) by the returned value
            let fd = unsafe { libc::fcntl(s.0.as_raw_fd(), libc::F_DUPFD, 3 + n) };
            assert!(fd >= 0, "{}", std::io::Error::last_os_error());
            unsafe { OwnedFd::from_raw_fd(fd) }
        })
        .collect();
    let names: Vec<&str> = sockets.iter().map(|(name, _)| *name).collect();
    let mut script = String::from("LISTEN_PID=$$; export LISTEN_PID; exec \"$0\"");
    for (i, c) in copies.iter().enumerate() {
        script += &format!(" {}<&{}", 3 + i, c.as_raw_fd());
    }
    for c in &copies {
        script += &format!(" {}<&-", c.as_raw_fd());
    }
    let mut command = Command::new("/bin/bash");
    command
        .arg("-c")
        .arg(script)
        .arg(program)
        .env("LISTEN_FDS", n.to_string())
        .env("LISTEN_FDNAMES", names.join(":"));
    Handing {
        command,
        _copies: copies,
    }
}
