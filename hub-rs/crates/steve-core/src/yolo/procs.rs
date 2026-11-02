//! Which processes a session's agent started, for `kill <pid>` (guard): the agent adapter's
//! descendants, plus everything still in its process group (a backgrounded server whose shell has
//! exited is re-parented to launchd but keeps the group). Read from `ps` when a request comes in.

use std::collections::{HashMap, HashSet};
use std::process::Command;

/// One `ps` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
}

/// Parses `ps -A -o pid=,ppid=,pgid=`.
pub fn parse_ps(out: &str) -> Vec<Proc> {
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace().map(|x| x.parse::<u32>().ok());
            Some(Proc {
                pid: it.next()??,
                ppid: it.next()??,
                pgid: it.next()??,
            })
        })
        .collect()
}

/// The processes `root` started: its descendants and the other members of its process group (the
/// adapter is spawned as a group leader). `root` itself is not in the list.
pub fn started_by(table: &[Proc], root: u32) -> Vec<u32> {
    if root <= 1 {
        return Vec::new();
    }
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in table {
        kids.entry(p.ppid).or_default().push(p.pid);
    }
    let mut seen: HashSet<u32> = HashSet::new();
    let mut stack = vec![root];
    while let Some(p) = stack.pop() {
        for &c in kids.get(&p).into_iter().flatten() {
            if c != root && seen.insert(c) {
                stack.push(c);
            }
        }
    }
    for p in table {
        if p.pgid == root && p.pid != root {
            seen.insert(p.pid);
        }
    }
    let mut v: Vec<u32> = seen.into_iter().filter(|p| *p > 1).collect();
    v.sort_unstable();
    v
}

/// [`started_by`] on the live process table; empty when `ps` fails.
pub fn live_started_by(root: u32) -> Vec<u32> {
    match Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,pgid="])
        .output()
    {
        Ok(o) if o.status.success() => {
            started_by(&parse_ps(&String::from_utf8_lossy(&o.stdout)), root)
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descendants_and_the_process_group() {
        let t = parse_ps(
            "  1 0 1\n 100 1 100\n 101 100 100\n 102 101 100\n 103 1 100\n 200 1 200\n 201 200 200\n 104 102 104\n bad line\n",
        );
        // 101, 102 by parent; 103 re-parented but still in the group; 104 its own group but a child
        assert_eq!(started_by(&t, 100), vec![101, 102, 103, 104]);
        assert!(!started_by(&t, 100).contains(&201));
        assert!(started_by(&t, 1).is_empty());
    }

    #[test]
    fn the_live_table_sees_our_own_child() {
        let mut c = Command::new("sleep").arg("5").spawn().unwrap();
        let me = std::process::id();
        let found = live_started_by(me).contains(&c.id());
        let _ = c.kill();
        let _ = c.wait();
        assert!(found);
    }
}
