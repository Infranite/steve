//! A Hub's port that the test holds itself (#429): bound before the Hub starts and handed to it as
//! a listening socket (`steve_hub::config::Handed`, systemd's socket activation), so no other test
//! can take it between choosing it and the Hub binding it, nor while the Hub restarts.

use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
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

/// `program`, started with `sockets` as its listening sockets, each under its name
/// (`LISTEN_FDNAMES`). The program runs in the process `Command` starts (`sh` execs it), so the
/// child's id is the program's.
pub fn command(program: &str, sockets: &[(&str, &Socket)]) -> Command {
    const MAX: usize = 4;
    assert!(sockets.len() <= MAX, "at most {MAX} sockets");
    let mut fds = [-1; MAX];
    for (fd, (_, s)) in fds.iter_mut().zip(sockets) {
        *fd = s.0.as_raw_fd();
    }
    let n = sockets.len();
    let names: Vec<&str> = sockets.iter().map(|(name, _)| *name).collect();
    // LISTEN_PID is the process that is to take the sockets: the shell, which becomes `program`
    let mut c = Command::new("/bin/sh");
    c.args([
        "-c",
        "LISTEN_PID=$$; export LISTEN_PID; exec \"$0\"",
        program,
    ])
    .env("LISTEN_FDS", n.to_string())
    .env("LISTEN_FDNAMES", names.join(":"));
    // SAFETY: only fcntl, dup2 and close between fork and exec (async-signal-safe, no allocation):
    // first every socket out of the way of descriptors 3.., then each onto its own one
    unsafe {
        c.pre_exec(move || {
            let mut moved = [-1; MAX];
            for (m, fd) in moved.iter_mut().zip(fds).take(n) {
                *m = libc::fcntl(fd, libc::F_DUPFD, (3 + n) as libc::c_int);
                if *m < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            for (i, fd) in moved.iter().enumerate().take(n) {
                if libc::dup2(*fd, 3 + i as libc::c_int) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(*fd);
            }
            Ok(())
        });
    }
    c
}
