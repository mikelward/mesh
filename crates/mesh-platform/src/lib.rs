//! Platform portability shims.
//!
//! A single home for the `libc` constants and types whose definitions differ
//! across the platforms mesh builds for. Centralizing them here keeps each
//! cfg-gated cast in one reviewed place instead of copied into every call site —
//! historically the source of repeated macOS-only build breaks.
//!
//! Linux and macOS are built and tested; FreeBSD is compile-checked in CI, which
//! is what keeps the `not(macos)` branches below honest for it.
//!
//! Add new shims here as they come up rather than reintroducing a `#[cfg]` dance
//! at the call site.

/// The `TIOCSCTTY` ioctl request, typed for `libc::ioctl` on this platform.
///
/// `libc` defines the constant as a narrower `c_uint` on macOS but as a
/// `c_ulong` on Linux, so a direct `libc::ioctl(fd, libc::TIOCSCTTY, 0)` fails
/// to compile on one platform or the other. This exposes it already widened to
/// the request type that `ioctl` takes on the target, so callers just pass
/// `mesh_platform::TIOCSCTTY`.
#[cfg(all(not(target_env = "musl"), target_os = "macos"))]
pub const TIOCSCTTY: libc::c_ulong = libc::TIOCSCTTY as libc::c_ulong;
#[cfg(all(not(target_env = "musl"), not(target_os = "macos")))]
pub const TIOCSCTTY: libc::c_ulong = libc::TIOCSCTTY;
#[cfg(target_env = "musl")]
pub const TIOCSCTTY: libc::c_int = libc::TIOCSCTTY;

/// The `TIOCGWINSZ` ioctl request, typed the same way and for the same reason.
#[cfg(all(not(target_env = "musl"), target_os = "macos"))]
pub const TIOCGWINSZ: libc::c_ulong = libc::TIOCGWINSZ as libc::c_ulong;
#[cfg(all(not(target_env = "musl"), not(target_os = "macos")))]
pub const TIOCGWINSZ: libc::c_ulong = libc::TIOCGWINSZ;
#[cfg(target_env = "musl")]
pub const TIOCGWINSZ: libc::c_int = libc::TIOCGWINSZ;

/// The `TIOCSWINSZ` ioctl request — the write half of the pair above. Used by
/// the tests, which give a pty a known size to read back.
#[cfg(all(not(target_env = "musl"), target_os = "macos"))]
pub const TIOCSWINSZ: libc::c_ulong = libc::TIOCSWINSZ as libc::c_ulong;
#[cfg(all(not(target_env = "musl"), not(target_os = "macos")))]
pub const TIOCSWINSZ: libc::c_ulong = libc::TIOCSWINSZ;
#[cfg(target_env = "musl")]
pub const TIOCSWINSZ: libc::c_int = libc::TIOCSWINSZ;

/// One process, as the two facts needed to find a command's descendants and to
/// be sure of them later.
///
/// `started` is the platform's own record of when the process began. It is
/// carried because a pid is not an identity: between noticing a process and
/// signalling it the pid can be recycled, and a stale one names whatever took
/// it. Comparing the pair rules that out. The value is opaque and only ever
/// compared with another from the same platform — no unit is promised.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Process {
    pub pid: libc::pid_t,
    pub parent: libc::pid_t,
    /// The process group, so a caller that has already signalled a group can
    /// tell which of these it has therefore already reached.
    pub group: libc::pid_t,
    pub started: u64,
}

/// A reading of the process table, and whether it is the whole of it.
///
/// `unreadable` counts entries this user was not allowed to inspect. It is
/// carried rather than dropped because "not visible" and "not there" lead to
/// opposite conclusions: a caller that treats a short table as complete decides
/// there is nothing left to find. A descendant of this run *can* land in that
/// count — a command is free to exec a privileged helper and change its
/// credentials, and under `hidepid` its `stat` then stops being readable — so
/// the caller is told rather than reassured.
#[derive(Clone, Debug, Default)]
pub struct Table {
    pub processes: Vec<Process>,
    pub unreadable: usize,
}

/// Does this error mean the process is simply **gone**?
///
/// Two spellings, because `/proc` gives a different one depending on *when* the
/// process died. Gone before the open: `ENOENT`, the directory is not there.
/// Gone between the open and the read: `ESRCH`, since the file is a view of a
/// task that no longer exists — and `read` is where that lands, so
/// [`std::fs::read`] can return it having opened the file successfully.
///
/// Only `ENOENT` was handled, which made a whole-table read fail whenever any
/// process on the host exited inside that window. It went unnoticed on a quiet
/// machine and turned up under a loaded CI runner, where processes come and go
/// throughout the scan. `ESRCH` has no [`std::io::ErrorKind`] of its own, so the
/// raw number is what there is to match on.
#[cfg(target_os = "linux")]
fn vanished(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
}

/// Every process this user can see.
///
/// An error means the table could not be read *at all* — not that there was
/// nothing in it. The two are indistinguishable from an empty list and they are
/// not the same: the caller degrades to the process group alone when this
/// fails, which is a weaker limit than it reports enforcing, and the user is
/// entitled to hear that rather than have it swallowed.
///
/// A single entry that cannot be read is *not* an error and is skipped: a
/// process is free to exit between being listed and being looked up, which is
/// the ordinary case rather than a failure.
#[cfg(target_os = "linux")]
pub fn processes() -> std::io::Result<Table> {
    let entries = std::fs::read_dir("/proc")?;
    let mut out = Vec::new();
    let mut unreadable = 0;
    for entry in entries {
        // Propagated, not flattened away: `read_dir` can fail *while* iterating,
        // and dropping that yields a partial table indistinguishable from a
        // complete one. A caller that trusts a short answer stops looking.
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(pid) = name.parse::<libc::pid_t>() else {
            continue;
        };
        // Read as *bytes*. A process may set its own name to anything, and the
        // kernel copies it into this file verbatim, so a single oddly-named
        // process anywhere on the host would otherwise fail the whole scan as
        // invalid UTF-8 — and since a failed scan degrades the limit, one
        // stranger could disable nested enforcement for everybody.
        let stat = match std::fs::read(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            // The process exited between being listed and being read. This is
            // the ordinary case, not a failure.
            Err(err) if vanished(&err) => continue,
            // A process this user is not allowed to inspect. Under `hidepid`
            // that is every process belonging to somebody else, so failing the
            // whole read here would disable the limit outright on exactly the
            // systems most careful about process visibility. It is *counted*
            // rather than ignored, because one of them can be this run's own: a
            // command that execs a privileged helper and changes its
            // credentials is still a descendant, and is now unreadable.
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                unreadable += 1;
                continue;
            }
            // Anything else is a real failure of the table, and a short table
            // read as a complete one is what lets a live group go unswept.
            Err(err) => return Err(err),
        };
        // The second field is the executable's name, in parentheses and with no
        // escaping — it can hold spaces and parentheses of its own — so the
        // fields after it are counted from the *last* `)` rather than by
        // splitting the whole line.
        let Some(close) = stat.iter().rposition(|byte| *byte == b')') else {
            continue;
        };
        // Everything after the name is ASCII: single-letter state and decimal
        // numbers. Only that tail is turned into text, so the name's bytes are
        // never required to be anything in particular.
        let Ok(rest) = std::str::from_utf8(&stat[close + 1..]) else {
            continue;
        };
        // Indexed rather than stepped through, so each one can be checked
        // against `proc(5)` by counting: this slice starts at `state`, which is
        // field 3, so an index here is its field number minus three. Walking the
        // iterator instead put `starttime` one place late and read `vsize` — a
        // number that changes whenever the process allocates, which is exactly
        // what a command's `SIGTERM` handler may do while the grace runs.
        const PPID: usize = 4 - 3;
        const PGRP: usize = 5 - 3;
        const STARTTIME: usize = 22 - 3;
        let fields: Vec<&str> = rest.split_whitespace().collect();
        let (Some(Ok(parent)), Some(Ok(group)), Some(Ok(started))) = (
            fields.get(PPID).map(|field| field.parse()),
            fields.get(PGRP).map(|field| field.parse()),
            fields.get(STARTTIME).map(|field| field.parse()),
        ) else {
            continue;
        };
        out.push(Process {
            pid,
            parent,
            group,
            started,
        });
    }
    Ok(Table {
        processes: out,
        unreadable,
    })
}

/// The same, from `libproc`: the pid list, then one lookup each.
///
/// `proc_listpids` is asked for its size first, and the *second* call's own
/// answer decides how much of the buffer is read — the table can grow in
/// between.
#[cfg(target_os = "macos")]
pub fn processes() -> std::io::Result<Table> {
    // `PROC_ALL_PIDS` from `<sys/proc_info.h>`. `libc` exposes the call and the
    // flavor but not this selector, and a plain API constant is safe to name
    // here in a way a struct layout would not be.
    const ALL_PIDS: u32 = 1;
    // SAFETY: a null buffer of length zero asks for the size that would be
    // needed, which is how this call is documented to be sized.
    let sized = unsafe { libc::proc_listpids(ALL_PIDS, 0, std::ptr::null_mut(), 0) };
    if sized <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Retried rather than sized once. The table can grow between the two calls,
    // and a buffer filled exactly to its capacity is indistinguishable from one
    // that was big enough — the pids that did not fit are simply absent, and a
    // short list read as a complete one is what lets a live group go unswept.
    // Room is added each time round so a machine that is steadily starting
    // processes cannot keep the answer permanently truncated.
    //
    // Counted in *entries*, never in bytes. The call is told the size of the
    // allocation itself, measured from it rather than computed alongside it, so
    // the two cannot drift: growing a byte figure that is not a whole number of
    // `pid_t`s hands the kernel a length longer than the buffer it writes into.
    let mut entries = sized as usize / std::mem::size_of::<libc::pid_t>();
    let pids = loop {
        let mut pids = vec![0 as libc::pid_t; entries];
        let bytes = std::mem::size_of_val(pids.as_slice());
        let Ok(room) = libc::c_int::try_from(bytes) else {
            return Err(std::io::Error::other(
                "the process table did not fit in any buffer this can size",
            ));
        };
        // SAFETY: `room` is the size of this very allocation.
        let filled = unsafe { libc::proc_listpids(ALL_PIDS, 0, pids.as_mut_ptr().cast(), room) };
        if filled <= 0 {
            return Err(std::io::Error::last_os_error());
        }
        // Only a *short* answer proves the buffer was big enough. One that fills
        // it exactly may have had more to say.
        if filled < room {
            pids.truncate(filled as usize / std::mem::size_of::<libc::pid_t>());
            break pids;
        }
        let Some(bigger) = entries.checked_add(entries / 2 + 1024) else {
            return Err(std::io::Error::other(
                "the process table did not fit in any buffer this can size",
            ));
        };
        entries = bigger;
    };
    let mut processes = Vec::new();
    let mut unreadable = 0;
    for pid in pids.into_iter().filter(|&pid| pid > 0) {
        match bsd_info(pid) {
            Lookup::Found(process) => processes.push(process),
            Lookup::Denied => unreadable += 1,
            Lookup::Gone => {}
            Lookup::Failed(err) => return Err(err),
        }
    }
    Ok(Table {
        processes,
        unreadable,
    })
}

/// One process's parent and start time, or `None` where it has already gone.
#[cfg(target_os = "macos")]
fn bsd_info(pid: libc::pid_t) -> Lookup {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let want = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `info` is a whole `proc_bsdinfo` and `want` is its size, which is
    // what the call is told to fill. A short answer is rejected below.
    let read =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), want) };
    if read == want {
        return Lookup::Found(Process {
            pid,
            parent: info.pbi_ppid as libc::pid_t,
            group: info.pbi_pgid as libc::pid_t,
            // Microseconds since the epoch: unique enough for the identity check
            // this is carried for.
            started: info
                .pbi_start_tvsec
                .wrapping_mul(1_000_000)
                .wrapping_add(info.pbi_start_tvusec),
        });
    }
    // A positive answer that is merely *short* is not a failure the call reported,
    // so `errno` has not been set for it and still holds whatever the previous
    // pid in this loop left there — reading it would classify this lookup by
    // that one's outcome. Only a zero-or-negative result means the call spoke.
    if read > 0 {
        return Lookup::Failed(std::io::Error::other(
            "proc_pidinfo filled less than a whole proc_bsdinfo",
        ));
    }
    // Told apart rather than lumped together, because they mean opposite things
    // to the caller: a pid that vanished between the listing and the lookup is
    // the ordinary race and says nothing, while one this user may not inspect
    // means the table is short in a way that matters. Counting the first as the
    // second would put the degraded-enforcement warning on every ordinary run,
    // since something on the machine is always exiting.
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EPERM | libc::EACCES) => Lookup::Denied,
        // The one documented way a pid stops being there between the listing
        // and the lookup. Everything else — an unexpected errno, or a short
        // answer with none set — is a failure of the read rather than a race,
        // and calling it a race would turn it into a partial table reported as
        // a whole one.
        Some(libc::ESRCH) => Lookup::Gone,
        Some(code) => Lookup::Failed(std::io::Error::from_raw_os_error(code)),
        None => Lookup::Failed(std::io::Error::other(
            "proc_pidinfo failed without saying why",
        )),
    }
}

/// What one process lookup found.
#[cfg(target_os = "macos")]
enum Lookup {
    Found(Process),
    /// Exited between being listed and being looked up — the ordinary race.
    Gone,
    /// Present but not inspectable by this user.
    Denied,
    /// The lookup itself failed, which is neither of the above.
    Failed(std::io::Error),
}

/// The same, read from `sysctl`'s process table.
///
/// Sized, then filled, then *retried* if it did not fit — the same shape as the
/// macOS path above and for the same reason. The table can grow between the two
/// calls, and `sysctl` answers `ENOMEM` when what it has to say no longer fits
/// what it was handed. That is a table that grew, not a failure of the read, and
/// reporting it as one degrades the limit to the process group alone on exactly
/// the busy host where a nested command is most likely to be running.
#[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
pub fn processes() -> std::io::Result<Table> {
    let mut query = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_ALL, 0];
    let mut wanted = 0;
    // SAFETY: `query` is a four-element MIB, and a null buffer with a zero
    // length is the documented way to ask `sysctl` for the size it would need.
    let sized = unsafe {
        libc::sysctl(
            query.as_mut_ptr(),
            query.len() as libc::c_uint,
            std::ptr::null_mut(),
            &raw mut wanted,
            std::ptr::null_mut(),
            0,
        )
    };
    if sized != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // A successful call that reports nothing has not set `errno`, so asking why
    // would answer for whatever failed last — the machine cannot have no
    // processes, and saying so plainly beats reporting a stale reason.
    if wanted == 0 {
        return Err(std::io::Error::other("the process table came back empty"));
    }
    // Counted in *entries*, and the byte figure handed to the kernel is measured
    // from the allocation rather than computed alongside it. A byte count that
    // is not a whole number of `kinfo_proc`s would otherwise round down when the
    // buffer is sized and stay whole when the length is passed, telling `sysctl`
    // it has more room than it does.
    let mut entries = wanted / std::mem::size_of::<libc::kinfo_proc>() + 1;
    loop {
        let mut table: Vec<libc::kinfo_proc> = Vec::with_capacity(entries);
        let mut size = entries * std::mem::size_of::<libc::kinfo_proc>();
        // SAFETY: `size` is the size of this very allocation, and `sysctl`
        // writes at most that much and reports back how much it used.
        let filled = unsafe {
            libc::sysctl(
                query.as_mut_ptr(),
                query.len() as libc::c_uint,
                table.as_mut_ptr().cast(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if filled == 0 {
            // SAFETY: `sysctl` reported `size` bytes written, and the elements
            // it wrote are whole `kinfo_proc`s from the start of the buffer.
            unsafe { table.set_len(size / std::mem::size_of::<libc::kinfo_proc>()) };
            return Ok(Table {
                processes: table.iter().map(read_kinfo).collect(),
                unreadable: 0,
            });
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ENOMEM) {
            return Err(err);
        }
        // Grown from the larger of what was asked for and what the failed call
        // says it needs, plus room of its own: `size` after `ENOMEM` is what
        // would have fitted a moment ago, and the table can grow again before
        // the next call. Without the extra, a host steadily starting processes
        // could keep this looping.
        let needed = size.max(entries * std::mem::size_of::<libc::kinfo_proc>())
            / std::mem::size_of::<libc::kinfo_proc>();
        let Some(bigger) = needed.checked_add(needed / 2 + 64) else {
            return Err(std::io::Error::other(
                "the process table did not fit in any buffer this can size",
            ));
        };
        entries = bigger;
    }
}

/// FreeBSD keeps all three at the top level, with the same `timeval` shape.
#[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
fn read_kinfo(entry: &libc::kinfo_proc) -> Process {
    Process {
        pid: entry.ki_pid,
        parent: entry.ki_ppid,
        group: entry.ki_pgid,
        started: (entry.ki_start.tv_sec as u64)
            .wrapping_mul(1_000_000)
            .wrapping_add(entry.ki_start.tv_usec as u64),
    }
}

/// The entries two readings of the process table agree on, taken from the
/// **later** one.
///
/// Agreement is on identity — pid and start together — because that is the pair
/// a stranger inheriting a recycled pid cannot satisfy. One read is not a
/// moment: the table is walked entry by entry, so a parent can be read, its pid
/// freed and reused, and a child of the *new* holder read afterwards. Two scans
/// do not make that atomic; they narrow the window from the whole walk to the
/// gap between them, and an entry in only one is dropped — which loses a
/// process born mid-read, the safe direction.
///
/// Which read supplies the rows is the whole of it, not a detail of argument
/// order. A process may legitimately `setpgid` between the two, and its *new*
/// group is exactly what a caller sweeping groups needs to know: keeping the
/// earlier row would confirm the process and then act on the group it has just
/// left. The arguments are in the order the reads were taken, so a call site
/// reads chronologically and a swap is visible against the names it passes.
pub fn agreed(earlier: &[Process], later: &[Process]) -> Vec<Process> {
    later
        .iter()
        .copied()
        .filter(|entry| {
            earlier
                .iter()
                .any(|seen| seen.pid == entry.pid && seen.started == entry.started)
        })
        .collect()
}

/// `root` and everything descended from it, parents before children.
///
/// One snapshot of the table, so it is a picture of a moment rather than a live
/// view — which is what the caller needs, because the ancestry that names these
/// processes is destroyed by the very signalling that follows it.
///
/// **Parents first.** A caller signalling the list in order cannot leave a
/// wrapper running long enough to notice its own child died and carry on as
/// though the command had finished.
///
/// This is safe on a platform whose enumeration is unavailable or wrong, which
/// is what makes the two that CI only compile-checks acceptable: nothing is
/// reachable from `root` through parent links that are absent or nonsense, so a
/// bad answer degrades to `root` alone — never to somebody else's process.
pub fn descendants(root: libc::pid_t) -> std::io::Result<Table> {
    // Read twice and keep only what both agree on: a single walk would let a
    // stranger holding a recycled pid be adopted into this run's tree, whose
    // group the sweep would then take for its own. Dating the parent link is not
    // enough on its own, since a replacement that started before the child still
    // satisfies the ordering. See [`agreed`] for why the later read wins.
    let read = processes()?;
    let confirm = processes()?;
    let table = agreed(&read.processes, &confirm.processes);
    Ok(Table {
        processes: walk(root, &table),
        unreadable: read.unreadable.max(confirm.unreadable),
    })
}

/// `root` and its descendants within one reading of the table, parents first.
///
/// Split from [`descendants`] because the reachability rules are the whole of
/// it and they are only arguable against a table you can write down.
fn walk(root: libc::pid_t, table: &[Process]) -> Vec<Process> {
    let mut tree: Vec<Process> = table
        .iter()
        .copied()
        .filter(|entry| entry.pid == root)
        .collect();
    let mut frontier: Vec<Process> = tree.clone();
    // Bounded by the table: a parent link that points into a cycle cannot make
    // this run longer than there are processes to visit.
    for _ in 0..table.len() {
        let mut next = Vec::new();
        for entry in table {
            if entry.pid == root || tree.iter().any(|seen| seen.pid == entry.pid) {
                continue;
            }
            // The parent link is matched by number *and* dated. Reading `/proc`
            // is not one atomic moment: a parent can be read early in the scan,
            // its pid freed and reused, and a child of the new holder read
            // later — so a numeric match alone adopts a stranger, whose group
            // the sweep would then take for this run's own. A process cannot
            // predate its own parent, so a child recorded as starting before
            // the parent it names is not that parent's.
            let descends = frontier
                .iter()
                .any(|parent| parent.pid == entry.parent && entry.started >= parent.started);
            if descends {
                tree.push(*entry);
                next.push(*entry);
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    tree
}

#[cfg(test)]
mod tests {
    use super::{Process, agreed, descendants, processes, walk};

    /// A middle link that dies between the two reads costs its subtree, and
    /// which read supplies the rows makes no difference to that.
    ///
    /// The tempting reading is that taking rows from the later scan erases the
    /// ancestry: the survivor is reparented to init there, so its recorded
    /// parent is 1 rather than the leader it was forked by. But a process is
    /// only reparented when its parent has *gone*, and a parent that has gone is
    /// missing from the later scan — so agreement drops it, and the link the
    /// earlier row preserves points at a process no longer in the table. Both
    /// orderings lose the subtree, for the same reason and at the same moment.
    #[test]
    fn a_middle_link_that_dies_between_the_reads_is_lost_either_way() {
        let earlier = [
            entry(10, 1, 10, 100),
            entry(20, 10, 20, 200),
            entry(30, 20, 20, 300),
        ];
        // 20 has gone; 30 survives in 20's group, reparented to init.
        let later = [entry(10, 1, 10, 100), entry(30, 1, 20, 300)];
        let from_later = walk(10, &agreed(&earlier, &later));
        assert!(
            !from_later.iter().any(|seen| seen.pid == 30),
            "the survivor was reached through a parent that had gone"
        );
        // The same walk over the rows the earlier scan would have supplied.
        let from_earlier = walk(10, &agreed(&later, &earlier));
        assert!(
            !from_earlier.iter().any(|seen| seen.pid == 30),
            "the earlier rows reached what the later rows could not"
        );
    }

    /// A survivor whose new parent is itself in the tree is still reached — and
    /// only the later row can say so.
    ///
    /// Reparenting does not always go to init: an ancestor that has claimed
    /// `PR_SET_CHILD_SUBREAPER` collects the orphan instead, and that ancestor
    /// can be part of this run. The earlier row still names the dead parent and
    /// leads nowhere; the later row names the reaper and leads home.
    #[test]
    fn a_survivor_adopted_by_a_reaper_inside_the_run_is_still_reached() {
        let earlier = [
            entry(10, 1, 10, 100),
            entry(20, 10, 20, 200),
            entry(30, 20, 20, 300),
        ];
        // 20 has gone and 10, a subreaper, has taken 30 on.
        let later = [entry(10, 1, 10, 100), entry(30, 10, 20, 300)];
        let found: Vec<libc::pid_t> = walk(10, &agreed(&earlier, &later))
            .iter()
            .map(|seen| seen.pid)
            .collect();
        assert_eq!(found, vec![10, 30], "the adopted survivor was lost");
    }

    /// One row of a process table, for the agreement tests.
    fn entry(pid: libc::pid_t, parent: libc::pid_t, group: libc::pid_t, started: u64) -> Process {
        Process {
            pid,
            parent,
            group,
            started,
        }
    }

    /// A process that changes group between the two reads is recorded as it now
    /// is, not as it was.
    ///
    /// This is the whole reason the later read supplies the rows. A descendant
    /// that calls `setpgid(0, 0)` and then leaves a survivor behind in its new
    /// group is only reachable if the snapshot names that group; recording the
    /// group it left gives the sweep no leader to recognize the new one by, and
    /// the survivor outlives the limit.
    #[test]
    fn agreement_records_the_group_a_process_has_moved_to() {
        let earlier = [entry(10, 1, 10, 100), entry(21, 10, 10, 210)];
        let later = [entry(10, 1, 10, 100), entry(21, 10, 21, 210)];
        let rows = agreed(&earlier, &later);
        assert!(
            rows.iter().any(|seen| seen.pid == 21 && seen.group == 21),
            "the group the process has just left was recorded"
        );
    }

    /// Only what both reads hold survives, and identity is the pair.
    ///
    /// A pid alone is not one: a process that exits between the reads and has
    /// its pid handed to a stranger appears in both under the same number, and
    /// admitting it adopts somebody else's process into this run's tree.
    #[test]
    fn agreement_needs_both_reads_and_the_whole_identity() {
        let earlier = [entry(10, 1, 10, 100), entry(20, 10, 20, 200)];
        let later = [
            // 20 has gone; a stranger wears its pid, having started later.
            entry(10, 1, 10, 100),
            entry(20, 1, 20, 999),
            // Born after the first read, so this run cannot vouch for it.
            entry(30, 10, 30, 300),
        ];
        let found: Vec<libc::pid_t> = agreed(&earlier, &later)
            .iter()
            .map(|seen| seen.pid)
            .collect();
        assert_eq!(found, vec![10], "a process neither read confirmed was kept");
    }

    /// A child and a grandchild are both found, and this process is not — the
    /// walk goes down from the root and never up.
    ///
    /// The only platform under test is the one CI runs; the other two are
    /// compile-checked, which is why [`descendants`] is written to be harmless
    /// rather than merely correct.
    #[test]
    fn a_process_tree_is_walked_downwards() {
        // SAFETY: the child does nothing but sleep and leave through `_exit`, so
        // it runs no destructor this process still owns.
        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork failed");
        if child == 0 {
            // SAFETY: as above, for the grandchild.
            let grandchild = unsafe { libc::fork() };
            if grandchild >= 0 {
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
            // SAFETY: leaving without unwinding.
            unsafe { libc::_exit(0) };
        }
        // The grandchild has to exist before the table is read, and the only
        // thing that says so is the table itself.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut tree = descendants(child)
            .expect("the process table is readable")
            .processes;
        while tree.len() < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
            tree = descendants(child)
                .expect("the process table is readable")
                .processes;
        }

        // SAFETY: scalar arguments naming this test's own child.
        let cleanup = || unsafe {
            libc::kill(child, libc::SIGKILL);
            libc::waitpid(child, std::ptr::null_mut(), 0);
        };
        let found: Vec<libc::pid_t> = tree.iter().map(|entry| entry.pid).collect();
        // SAFETY: `getpid` takes no arguments and cannot fail.
        let me = unsafe { libc::getpid() };
        let walked_up = found.contains(&me);
        let root_first = tree.first().map(|entry| entry.pid) == Some(child);
        let depth = tree.len();
        cleanup();

        assert!(!walked_up, "the walk reached this process: {found:?}");
        assert!(root_first, "the root is not listed first: {found:?}");
        assert_eq!(depth, 2, "child and grandchild expected: {found:?}");
    }

    /// A process that goes during the scan is skipped, however `/proc` spells
    /// it.
    ///
    /// The spelling depends on *when* it went: `ENOENT` before the open, `ESRCH`
    /// between the open and the read. Only the first was handled, so any process
    /// on the host exiting inside that window failed the whole table read — rare
    /// on a quiet machine, reproducible on a loaded CI runner. Anything else is
    /// still a real failure, since a short table read as a complete one is what
    /// lets a live group go unswept.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_process_that_goes_mid_read_is_not_a_failed_table() {
        for code in [libc::ENOENT, libc::ESRCH] {
            assert!(
                super::vanished(&std::io::Error::from_raw_os_error(code)),
                "raw {code} should read as a process that has gone"
            );
        }
        for code in [libc::EACCES, libc::EIO] {
            assert!(
                !super::vanished(&std::io::Error::from_raw_os_error(code)),
                "raw {code} is a real failure, not a process that has gone"
            );
        }
    }

    /// A process's recorded start does not move while it runs.
    ///
    /// The point of carrying it: `kill_stray_groups` tells a survivor from
    /// whatever inherited its pid by this number, and one that drifts makes the
    /// escalation skip the very process it is chasing. `/proc`'s `starttime`
    /// sits one place before `vsize`, and reading that one instead passed every
    /// other test here while failing exactly when a command allocated during its
    /// grace — so this allocates on purpose.
    #[test]
    fn a_recorded_start_does_not_drift() {
        // SAFETY: `getpid` takes no arguments and cannot fail.
        let me = unsafe { libc::getpid() };
        let started = || {
            processes()
                .expect("the process table is readable")
                .processes
                .into_iter()
                .find(|entry| entry.pid == me)
                .map(|entry| entry.started)
        };
        let before = started();
        assert!(before.is_some(), "this process is missing from the table");
        // Big enough to move `vsize`, and touched at both ends so it cannot be
        // optimized away.
        let mut hog = vec![0u8; 64 * 1024 * 1024];
        hog[0] = 1;
        let last = hog.len() - 1;
        hog[last] = 1;
        assert_eq!(started(), before, "the recorded start moved");
        drop(hog);
    }

    /// The table names this process, with the parent and group the kernel
    /// reports.
    ///
    /// Every field here is read out of `/proc/[pid]/stat` by position, and a
    /// position is the one thing a comment cannot check. Each is pinned against
    /// the syscall that answers the same question, so miscounting the fields
    /// fails here rather than somewhere downstream that only misbehaves under a
    /// signal.
    #[test]
    fn the_table_includes_this_process() {
        // SAFETY: none of these take arguments and none can fail.
        let (me, parent, group) = unsafe { (libc::getpid(), libc::getppid(), libc::getpgrp()) };
        let table = processes()
            .expect("the process table is readable")
            .processes;
        let mine = table.iter().find(|entry| entry.pid == me);
        assert_eq!(
            mine.map(|entry| entry.parent),
            Some(parent),
            "this process is missing or has the wrong parent"
        );
        assert_eq!(
            mine.map(|entry| entry.group),
            Some(group),
            "the recorded process group is not the one the kernel reports"
        );
    }

    /// The recorded group is the process group, in a case where nothing else
    /// shares its value.
    ///
    /// Checking it against this process is not enough: a test binary is
    /// commonly its own session leader, so `pgrp` and `sid` hold the same
    /// number and reading the wrong one of the two passes. The child here puts
    /// itself in a group of its own while staying in this session, which is the
    /// only arrangement that tells the adjacent fields apart.
    #[test]
    fn a_process_group_is_recorded_where_it_differs() {
        // SAFETY: the child changes its own group, sleeps, and leaves through
        // `_exit`, running no destructor this process still owns.
        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork failed");
        if child == 0 {
            // SAFETY: both name the calling process and neither can block.
            unsafe {
                libc::setpgid(0, 0);
                libc::_exit(if libc::getpgrp() == libc::getpid() {
                    // Held until the parent has read the table.
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    0
                } else {
                    1
                });
            }
        }
        // The group change has to have happened before the table is read, and
        // the only thing that says so is the table itself.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let group = || {
            processes()
                .expect("the process table is readable")
                .processes
                .into_iter()
                .find(|entry| entry.pid == child)
                .map(|entry| entry.group)
        };
        let mut found = group();
        while found != Some(child) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
            found = group();
        }

        // SAFETY: scalar arguments naming this test's own child.
        unsafe {
            libc::kill(child, libc::SIGKILL);
            libc::waitpid(child, std::ptr::null_mut(), 0);
        }
        assert_eq!(
            found,
            Some(child),
            "the child leads its own group, but that is not what was recorded"
        );
    }

    /// A process started later has a later recorded start.
    ///
    /// `a_recorded_start_does_not_drift` catches a field that *moves*; this one
    /// catches a field that is stable but is not a start time at all. A child's
    /// `vsize` is a copy of its parent's, so reading that field passes the drift
    /// test and fails here — which is what the field being read by position
    /// costs, and why the ordering is asserted rather than assumed.
    #[test]
    fn a_later_process_has_a_later_start() {
        // SAFETY: `getpid` takes no arguments and cannot fail.
        let me = unsafe { libc::getpid() };
        // The clock this is recorded against ticks in hundredths of a second, so
        // the child has to begin in a later tick for the comparison to mean
        // anything.
        std::thread::sleep(std::time::Duration::from_millis(50));
        // SAFETY: the child sleeps and leaves through `_exit`, running no
        // destructor this process still owns.
        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork failed");
        if child == 0 {
            std::thread::sleep(std::time::Duration::from_secs(5));
            // SAFETY: leaving without unwinding.
            unsafe { libc::_exit(0) };
        }

        let table = processes()
            .expect("the process table is readable")
            .processes;
        // SAFETY: scalar arguments naming this test's own child.
        let cleanup = || unsafe {
            libc::kill(child, libc::SIGKILL);
            libc::waitpid(child, std::ptr::null_mut(), 0);
        };
        let started = |pid| {
            table
                .iter()
                .find(|entry| entry.pid == pid)
                .map(|entry| entry.started)
        };
        let (mine, theirs) = (started(me), started(child));
        cleanup();

        let (Some(mine), Some(theirs)) = (mine, theirs) else {
            panic!("this process or its child is missing from the table");
        };
        assert!(
            theirs > mine,
            "the child's recorded start ({theirs}) is not after this process's ({mine})"
        );
    }
}
