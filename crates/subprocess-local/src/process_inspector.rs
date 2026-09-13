//! Platform process-table inspection for terminal readiness and safe teardown.

#[cfg(unix)]
use std::fs::File;
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::Arc,
};

use seekdeep_subprocess::{ProcessGroupId, ProcessId, SubprocessTerminalSignal};

const JAVASCRIPT_MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

fn parse_safe_i64(value: &str) -> Option<i64> {
    let value = value.parse::<i64>().ok()?;
    (value.abs() <= JAVASCRIPT_MAX_SAFE_INTEGER).then_some(value)
}

/// Process start token paired with a pid to fence PID reuse.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessStartIdentity(String);

impl ProcessStartIdentity {
    /// Wraps an exact platform start token.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrowed token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// PID plus exact start token.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessIdentity {
    /// Operating-system pid.
    pub pid: ProcessId,
    /// Platform start token.
    pub started: ProcessStartIdentity,
}

/// Extensible host-platform selection used by the injectable inspector factory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectorPlatform {
    /// Linux `/proc` implementation.
    Linux,
    /// macOS `ps` implementation.
    MacOs,
    /// Unknown platform spelling retained for the diagnostic.
    Unknown(String),
}

/// Extensible Linux syscall-table architecture selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectorArchitecture {
    /// x86-64 Linux syscall numbers.
    X86_64,
    /// `AArch64` Linux syscall numbers.
    Aarch64,
    /// Unsupported architecture; stdin waiting fails closed.
    Unknown(String),
}

/// Closed signal vocabulary at the injectable operating-system boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InspectorSignal {
    /// Interrupt.
    Sigint,
    /// Graceful termination.
    Sigterm,
    /// Uncatchable termination.
    Sigkill,
    /// Terminal stop.
    Sigtstp,
    /// Hangup.
    Sighup,
}

/// Testable filesystem, process-table, memory, command, and signal boundary.
pub trait ProcessInspectorInternals: std::fmt::Debug + Send + Sync {
    /// Reads one UTF-8 process metadata file.
    ///
    /// # Errors
    ///
    /// Returns injected filesystem failures.
    fn read_file(&self, path: &str) -> io::Result<String>;
    /// Lists one process metadata directory.
    ///
    /// # Errors
    ///
    /// Returns injected filesystem failures.
    fn read_dir(&self, path: &str) -> io::Result<Vec<String>>;
    /// Reads process memory at an exact byte address.
    ///
    /// # Errors
    ///
    /// Returns injected open, read, or permission failures.
    fn read_memory(&self, pid: ProcessId, address: u64, length: usize) -> io::Result<Vec<u8>>;
    /// Executes one fixed host command and returns stdout.
    ///
    /// # Errors
    ///
    /// Returns injected launch, exit-status, or output failures.
    fn exec(&self, file: &str, args: &[String]) -> io::Result<String>;
    /// Delivers one signal to a positive process or negative process-group id.
    ///
    /// # Errors
    ///
    /// Returns injected signal-delivery failures.
    fn kill(&self, pid: i64, signal: InspectorSignal) -> io::Result<()>;
}

/// Real host boundary used in production.
#[derive(Debug, Default)]
pub struct HostProcessInspectorInternals;

impl ProcessInspectorInternals for HostProcessInspectorInternals {
    fn read_file(&self, path: &str) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn read_dir(&self, path: &str) -> io::Result<Vec<String>> {
        Ok(std::fs::read_dir(path)?
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect())
    }

    fn read_memory(&self, pid: ProcessId, address: u64, length: usize) -> io::Result<Vec<u8>> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt as _;
            let file = File::open(format!("/proc/{}/mem", pid.as_i64()))?;
            let mut buffer = vec![0_u8; length];
            let read = file.read_at(&mut buffer, address)?;
            buffer.truncate(read);
            Ok(buffer)
        }
        #[cfg(not(unix))]
        {
            let _ = (pid, address, length);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "process memory is unsupported",
            ))
        }
    }

    fn exec(&self, file: &str, args: &[String]) -> io::Result<String> {
        let output = std::process::Command::new(file).args(args).output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "{file} exited with {}",
                output.status
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn kill(&self, pid: i64, signal: InspectorSignal) -> io::Result<()> {
        host_kill(pid, signal)
    }
}

/// Injectable process operations used by a local PTY session.
pub trait ProcessInspector: std::fmt::Debug + Send + Sync {
    /// Foreground process group attached to the shell terminal.
    fn foreground_pgid(&self, shell_pid: ProcessId) -> Option<ProcessGroupId>;
    /// Fallible foreground lookup for injected or remote host boundaries.
    ///
    /// # Errors
    ///
    /// Returns a boundary-specific process-table failure.
    fn try_foreground_pgid(&self, shell_pid: ProcessId) -> anyhow::Result<Option<ProcessGroupId>> {
        Ok(self.foreground_pgid(shell_pid))
    }
    /// Whether a group thread is blocked waiting for terminal stdin.
    fn is_stdin_waiting(&self, pgid: ProcessGroupId) -> bool;
    /// Fallible stdin-wait lookup for injected or remote host boundaries.
    ///
    /// # Errors
    ///
    /// Returns a boundary-specific syscall-inspection failure.
    fn try_is_stdin_waiting(&self, pgid: ProcessGroupId) -> anyhow::Result<bool> {
        Ok(self.is_stdin_waiting(pgid))
    }
    /// Root and transitive descendants, children first.
    fn process_tree(&self, root_pid: ProcessId) -> Vec<ProcessIdentity>;
    /// Fallible rooted-tree scan for injected or remote host boundaries.
    ///
    /// # Errors
    ///
    /// Returns a boundary-specific process-table failure.
    fn try_process_tree(&self, root_pid: ProcessId) -> anyhow::Result<Vec<ProcessIdentity>> {
        Ok(self.process_tree(root_pid))
    }
    /// Current members of one POSIX process session.
    fn process_session(&self, session_id: ProcessId) -> Vec<ProcessIdentity>;
    /// Fallible session scan for injected or remote host boundaries.
    ///
    /// # Errors
    ///
    /// Returns a boundary-specific process-table failure.
    fn try_process_session(&self, session_id: ProcessId) -> anyhow::Result<Vec<ProcessIdentity>> {
        Ok(self.process_session(session_id))
    }
    /// Whether the exact identity remains non-quiescent.
    fn is_alive(&self, identity: &ProcessIdentity) -> bool;
    /// Fallible exact-identity liveness probe for injected or remote host boundaries.
    ///
    /// # Errors
    ///
    /// Returns a boundary-specific identity-inspection failure.
    fn try_is_alive(&self, identity: &ProcessIdentity) -> anyhow::Result<bool> {
        Ok(self.is_alive(identity))
    }
    /// Signals a process group.
    ///
    /// # Errors
    ///
    /// Returns host signal-delivery failures.
    fn signal_group(
        &self,
        pgid: ProcessGroupId,
        signal: SubprocessTerminalSignal,
    ) -> anyhow::Result<()>;
    /// Signals an exact identity if it is still alive.
    ///
    /// # Errors
    ///
    /// Returns host signal-delivery failures.
    fn signal_process(&self, identity: &ProcessIdentity, force: bool) -> anyhow::Result<()>;
}

/// Parsed fields consumed from Linux `/proc/<pid>/stat`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcStat {
    /// Process id.
    pub pid: ProcessId,
    /// Parent process id.
    pub parent_pid: ProcessId,
    /// Process group id.
    pub process_group_id: ProcessGroupId,
    /// Session id.
    pub session_id: ProcessId,
    /// Single-character process state.
    pub state: char,
    /// Positive foreground terminal process group.
    pub terminal_process_group_id: Option<ProcessGroupId>,
    /// Start-time tick token.
    pub started: ProcessStartIdentity,
}

/// Parses Linux stat text without being confused by parentheses in `comm`.
#[must_use]
pub fn parse_proc_stat(text: &str) -> Option<ProcStat> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    if open == 0 || close <= open {
        return None;
    }
    let pid = parse_safe_i64(text[..open].trim())?;
    let fields = text
        .get(close + 1..)?
        .split_whitespace()
        .collect::<Vec<_>>();
    let state = fields.first()?.chars().collect::<Vec<_>>();
    if state.len() != 1 || fields.len() <= 19 {
        return None;
    }
    let parent_pid = parse_safe_i64(fields[1])?;
    let process_group_id = parse_safe_i64(fields[2])?;
    let session_id = parse_safe_i64(fields[3])?;
    let terminal_group = parse_safe_i64(fields[5])?;
    Some(ProcStat {
        pid: ProcessId::new(pid),
        parent_pid: ProcessId::new(parent_pid),
        process_group_id: ProcessGroupId::new(process_group_id),
        session_id: ProcessId::new(session_id),
        state: state[0],
        terminal_process_group_id: (terminal_group > 0)
            .then_some(ProcessGroupId::new(terminal_group)),
        started: ProcessStartIdentity::new(fields[19]),
    })
}

fn read_linux_stat(internals: &dyn ProcessInspectorInternals, pid: ProcessId) -> Option<ProcStat> {
    parse_proc_stat(
        &internals
            .read_file(&format!("/proc/{}/stat", pid.as_i64()))
            .ok()?,
    )
}

fn numeric_entries(internals: &dyn ProcessInspectorInternals, path: &str) -> Vec<ProcessId> {
    internals.read_dir(path).map_or_else(
        |_| Vec::new(),
        |entries| {
            entries
                .into_iter()
                .filter_map(|entry| entry.parse::<i64>().ok())
                .map(ProcessId::new)
                .collect()
        },
    )
}

/// Reports live members, zombie-only quiescence, or an unobservable group.
#[must_use]
pub fn linux_process_group_has_live_members(
    process_group_id: ProcessGroupId,
    internals: &dyn ProcessInspectorInternals,
) -> Option<bool> {
    let entries = internals.read_dir("/proc").ok()?;
    let mut matched = false;
    for pid in entries
        .into_iter()
        .filter_map(|entry| entry.parse::<i64>().ok())
        .map(ProcessId::new)
    {
        let Some(stat) = read_linux_stat(internals, pid) else {
            continue;
        };
        if stat.process_group_id != process_group_id {
            continue;
        }
        matched = true;
        if !matches!(stat.state, 'Z' | 'X' | 'x') {
            return Some(true);
        }
    }
    matched.then_some(false)
}

#[derive(Clone, Copy, Debug)]
struct SyscallTable {
    read: i64,
    select: Option<i64>,
    pselect: i64,
    poll: Option<i64>,
    ppoll: i64,
    epoll_wait: Option<i64>,
    epoll_pwait: i64,
}

fn syscall_table(architecture: &InspectorArchitecture) -> Option<SyscallTable> {
    match architecture {
        InspectorArchitecture::X86_64 => Some(SyscallTable {
            read: 0,
            select: Some(23),
            pselect: 270,
            poll: Some(7),
            ppoll: 271,
            epoll_wait: Some(232),
            epoll_pwait: 281,
        }),
        InspectorArchitecture::Aarch64 => Some(SyscallTable {
            read: 63,
            select: None,
            pselect: 72,
            poll: None,
            ppoll: 73,
            epoll_wait: None,
            epoll_pwait: 22,
        }),
        InspectorArchitecture::Unknown(_) => None,
    }
}

#[derive(Debug)]
struct SyscallInfo {
    number: i64,
    args: [u64; 6],
}

fn read_syscall(
    internals: &dyn ProcessInspectorInternals,
    pid: ProcessId,
    tid: ProcessId,
) -> Option<SyscallInfo> {
    let text = internals
        .read_file(&format!(
            "/proc/{}/task/{}/syscall",
            pid.as_i64(),
            tid.as_i64()
        ))
        .ok()?;
    let text = text.trim();
    if text == "running" || text.starts_with("-1 ") {
        return None;
    }
    let mut fields = text.split_whitespace();
    let number = parse_safe_i64(fields.next()?)?;
    let mut args = [0_u64; 6];
    for argument in &mut args {
        *argument = u64::from_str_radix(fields.next()?.trim_start_matches("0x"), 16).ok()?;
        if *argument > u64::try_from(JAVASCRIPT_MAX_SAFE_INTEGER).ok()? {
            return None;
        }
    }
    Some(SyscallInfo { number, args })
}

fn fd_set_has_stdin(
    internals: &dyn ProcessInspectorInternals,
    pid: ProcessId,
    address: u64,
) -> bool {
    address != 0
        && internals
            .read_memory(pid, address, 8)
            .ok()
            .and_then(|bytes| bytes.first().copied())
            .is_some_and(|byte| byte & 1 == 1)
}

fn poll_has_stdin(
    internals: &dyn ProcessInspectorInternals,
    pid: ProcessId,
    address: u64,
    count: u64,
) -> bool {
    if address == 0 || count == 0 {
        return false;
    }
    let length = usize::try_from(count.min(1024).saturating_mul(8)).unwrap_or(usize::MAX);
    internals
        .read_memory(pid, address, length)
        .is_ok_and(|memory| {
            memory.chunks_exact(8).any(|entry| {
                i32::from_le_bytes(entry[0..4].try_into().unwrap_or_default()) == 0
                    && i16::from_le_bytes(entry[4..6].try_into().unwrap_or_default()) & 0x001 != 0
            })
        })
}

fn epoll_has_stdin(
    internals: &dyn ProcessInspectorInternals,
    pid: ProcessId,
    epoll_fd: u64,
) -> bool {
    internals
        .read_file(&format!("/proc/{}/fdinfo/{epoll_fd}", pid.as_i64()))
        .is_ok_and(|text| {
            text.lines().any(|line| {
                line.trim()
                    .strip_prefix("tfd:")
                    .and_then(|rest| rest.split_whitespace().next())
                    == Some("0")
            })
        })
}

fn syscall_waits_on_stdin(
    internals: &dyn ProcessInspectorInternals,
    pid: ProcessId,
    syscall: &SyscallInfo,
    table: SyscallTable,
) -> bool {
    let [first, second, third, ..] = syscall.args;
    if syscall.number == table.read {
        return first == 0;
    }
    if Some(syscall.number) == table.select || syscall.number == table.pselect {
        return first >= 1 && fd_set_has_stdin(internals, pid, second);
    }
    if Some(syscall.number) == table.poll || syscall.number == table.ppoll {
        return second >= 1 && poll_has_stdin(internals, pid, first, second);
    }
    if Some(syscall.number) == table.epoll_wait || syscall.number == table.epoll_pwait {
        return third >= 1 && epoll_has_stdin(internals, pid, first);
    }
    false
}

#[derive(Clone, Debug)]
struct ProcessTreeEntry {
    identity: ProcessIdentity,
    parent_pid: ProcessId,
}

fn rooted_process_tree(entries: &[ProcessTreeEntry], root_pid: ProcessId) -> Vec<ProcessIdentity> {
    let by_pid = entries
        .iter()
        .map(|entry| (entry.identity.pid, entry))
        .collect::<BTreeMap<_, _>>();
    let Some(root) = by_pid.get(&root_pid).copied() else {
        return Vec::new();
    };
    let mut by_parent = BTreeMap::<ProcessId, Vec<&ProcessTreeEntry>>::new();
    for entry in entries {
        by_parent.entry(entry.parent_pid).or_default().push(entry);
    }
    let mut result = Vec::new();
    visit_tree(root, &by_parent, &mut BTreeSet::new(), &mut result);
    result
}

fn visit_tree(
    entry: &ProcessTreeEntry,
    by_parent: &BTreeMap<ProcessId, Vec<&ProcessTreeEntry>>,
    visited: &mut BTreeSet<ProcessId>,
    result: &mut Vec<ProcessIdentity>,
) {
    if !visited.insert(entry.identity.pid) {
        return;
    }
    for child in by_parent.get(&entry.identity.pid).into_iter().flatten() {
        visit_tree(child, by_parent, visited, result);
    }
    result.push(entry.identity.clone());
}

/// Linux `/proc` process inspector.
#[derive(Debug)]
pub struct LinuxProcessInspector {
    architecture: InspectorArchitecture,
    internals: Arc<dyn ProcessInspectorInternals>,
}

impl LinuxProcessInspector {
    fn entries(&self) -> Vec<ProcStat> {
        numeric_entries(self.internals.as_ref(), "/proc")
            .into_iter()
            .filter_map(|pid| read_linux_stat(self.internals.as_ref(), pid))
            .collect()
    }
}

impl ProcessInspector for LinuxProcessInspector {
    fn foreground_pgid(&self, shell_pid: ProcessId) -> Option<ProcessGroupId> {
        read_linux_stat(self.internals.as_ref(), shell_pid)?.terminal_process_group_id
    }

    fn is_stdin_waiting(&self, pgid: ProcessGroupId) -> bool {
        let Some(table) = syscall_table(&self.architecture) else {
            return false;
        };
        for stat in self
            .entries()
            .into_iter()
            .filter(|stat| stat.process_group_id == pgid)
        {
            for tid in numeric_entries(
                self.internals.as_ref(),
                &format!("/proc/{}/task", stat.pid.as_i64()),
            ) {
                if read_syscall(self.internals.as_ref(), stat.pid, tid).is_some_and(|syscall| {
                    syscall_waits_on_stdin(self.internals.as_ref(), stat.pid, &syscall, table)
                }) {
                    return true;
                }
            }
        }
        false
    }

    fn process_tree(&self, root_pid: ProcessId) -> Vec<ProcessIdentity> {
        rooted_process_tree(
            &self
                .entries()
                .into_iter()
                .map(|stat| ProcessTreeEntry {
                    identity: ProcessIdentity {
                        pid: stat.pid,
                        started: stat.started,
                    },
                    parent_pid: stat.parent_pid,
                })
                .collect::<Vec<_>>(),
            root_pid,
        )
    }

    fn process_session(&self, session_id: ProcessId) -> Vec<ProcessIdentity> {
        self.entries()
            .into_iter()
            .filter(|stat| stat.session_id == session_id)
            .map(|stat| ProcessIdentity {
                pid: stat.pid,
                started: stat.started,
            })
            .collect()
    }

    fn is_alive(&self, identity: &ProcessIdentity) -> bool {
        read_linux_stat(self.internals.as_ref(), identity.pid).is_some_and(|stat| {
            stat.started == identity.started && !matches!(stat.state, 'Z' | 'X' | 'x')
        })
    }

    fn signal_group(
        &self,
        pgid: ProcessGroupId,
        signal: SubprocessTerminalSignal,
    ) -> anyhow::Result<()> {
        self.internals
            .kill(-pgid.as_i64(), terminal_signal(signal))?;
        Ok(())
    }

    fn signal_process(&self, identity: &ProcessIdentity, force: bool) -> anyhow::Result<()> {
        if self.is_alive(identity) {
            self.internals.kill(
                identity.pid.as_i64(),
                if force {
                    InspectorSignal::Sigkill
                } else {
                    InspectorSignal::Sigterm
                },
            )?;
        }
        Ok(())
    }
}

/// macOS `ps`-backed process inspector.
#[derive(Debug)]
pub struct MacProcessInspector {
    internals: Arc<dyn ProcessInspectorInternals>,
}

impl MacProcessInspector {
    fn process_table(&self) -> io::Result<Vec<ProcessTreeEntry>> {
        Ok(self
            .internals
            .exec(
                "/bin/ps",
                &["-axo".to_owned(), "pid=,ppid=,lstart=".to_owned()],
            )?
            .lines()
            .filter_map(parse_ps_entry)
            .collect())
    }
}

fn parse_ps_entry(line: &str) -> Option<ProcessTreeEntry> {
    let mut fields = line.split_whitespace();
    let pid = fields.next()?.parse::<i64>().ok()?;
    let parent = fields.next()?.parse::<i64>().ok()?;
    let started = fields.collect::<Vec<_>>().join(" ");
    (!started.is_empty()).then(|| ProcessTreeEntry {
        identity: ProcessIdentity {
            pid: ProcessId::new(pid),
            started: ProcessStartIdentity::new(started),
        },
        parent_pid: ProcessId::new(parent),
    })
}

impl ProcessInspector for MacProcessInspector {
    fn foreground_pgid(&self, shell_pid: ProcessId) -> Option<ProcessGroupId> {
        let text = self
            .internals
            .exec(
                "/bin/ps",
                &[
                    "-o".to_owned(),
                    "tpgid=".to_owned(),
                    "-p".to_owned(),
                    shell_pid.as_i64().to_string(),
                ],
            )
            .ok()?;
        let value = parse_safe_i64(text.trim())?;
        (value > 0).then_some(ProcessGroupId::new(value))
    }

    fn is_stdin_waiting(&self, _pgid: ProcessGroupId) -> bool {
        false
    }

    fn process_tree(&self, root_pid: ProcessId) -> Vec<ProcessIdentity> {
        self.process_table().map_or_else(
            |_| Vec::new(),
            |entries| rooted_process_tree(&entries, root_pid),
        )
    }

    fn process_session(&self, _session_id: ProcessId) -> Vec<ProcessIdentity> {
        Vec::new()
    }

    fn is_alive(&self, identity: &ProcessIdentity) -> bool {
        self.process_table()
            .is_ok_and(|entries| entries.iter().any(|entry| entry.identity == *identity))
    }

    fn signal_group(
        &self,
        pgid: ProcessGroupId,
        signal: SubprocessTerminalSignal,
    ) -> anyhow::Result<()> {
        self.internals
            .kill(-pgid.as_i64(), terminal_signal(signal))?;
        Ok(())
    }

    fn signal_process(&self, identity: &ProcessIdentity, force: bool) -> anyhow::Result<()> {
        if self.is_alive(identity) {
            self.internals.kill(
                identity.pid.as_i64(),
                if force {
                    InspectorSignal::Sigkill
                } else {
                    InspectorSignal::Sigterm
                },
            )?;
        }
        Ok(())
    }
}

/// Creates an explicitly selected inspector with injected host operations.
///
/// # Errors
///
/// Rejects unsupported platform spellings at the earliest resolvable point.
pub fn create_process_inspector_for(
    platform: InspectorPlatform,
    architecture: InspectorArchitecture,
    internals: Arc<dyn ProcessInspectorInternals>,
) -> anyhow::Result<Arc<dyn ProcessInspector>> {
    match platform {
        InspectorPlatform::Linux => Ok(Arc::new(LinuxProcessInspector {
            architecture,
            internals,
        })),
        InspectorPlatform::MacOs => Ok(Arc::new(MacProcessInspector { internals })),
        InspectorPlatform::Unknown(platform) => {
            anyhow::bail!(
                "subprocess-local: terminal inspection is unsupported on platform {platform}"
            )
        }
    }
}

/// Creates the current-host production inspector.
///
/// # Errors
///
/// Rejects hosts without a parity implementation.
pub fn create_process_inspector() -> anyhow::Result<Arc<dyn ProcessInspector>> {
    let platform = match std::env::consts::OS {
        "linux" => InspectorPlatform::Linux,
        "macos" => InspectorPlatform::MacOs,
        value => InspectorPlatform::Unknown(value.to_owned()),
    };
    let architecture = match std::env::consts::ARCH {
        "x86_64" => InspectorArchitecture::X86_64,
        "aarch64" => InspectorArchitecture::Aarch64,
        value => InspectorArchitecture::Unknown(value.to_owned()),
    };
    create_process_inspector_for(
        platform,
        architecture,
        Arc::new(HostProcessInspectorInternals),
    )
}

fn terminal_signal(signal: SubprocessTerminalSignal) -> InspectorSignal {
    match signal {
        SubprocessTerminalSignal::Sigint => InspectorSignal::Sigint,
        SubprocessTerminalSignal::Sigterm => InspectorSignal::Sigterm,
        SubprocessTerminalSignal::Sigkill => InspectorSignal::Sigkill,
        SubprocessTerminalSignal::Sigtstp => InspectorSignal::Sigtstp,
        SubprocessTerminalSignal::Sighup => InspectorSignal::Sighup,
    }
}

pub(crate) fn signal_unchecked_process(pid: ProcessId, force: bool) -> anyhow::Result<()> {
    host_kill(
        pid.as_i64(),
        if force {
            InspectorSignal::Sigkill
        } else {
            InspectorSignal::Sigterm
        },
    )?;
    Ok(())
}

#[cfg(unix)]
fn host_kill(pid: i64, signal: InspectorSignal) -> io::Result<()> {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    let signal = match signal {
        InspectorSignal::Sigint => Signal::SIGINT,
        InspectorSignal::Sigterm => Signal::SIGTERM,
        InspectorSignal::Sigkill => Signal::SIGKILL,
        InspectorSignal::Sigtstp => Signal::SIGTSTP,
        InspectorSignal::Sighup => Signal::SIGHUP,
    };
    kill(
        Pid::from_raw(i32::try_from(pid).map_err(io::Error::other)?),
        Some(signal),
    )
    .map_err(io::Error::other)
}

#[cfg(not(unix))]
fn host_kill(_pid: i64, _signal: InspectorSignal) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process signalling is unsupported",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(
        pid: i64,
        pgrp: i64,
        session: i64,
        tpgid: i64,
        started: &str,
        parent: i64,
        state: &str,
    ) -> String {
        let mut rest = vec![
            state.to_owned(),
            parent.to_string(),
            pgrp.to_string(),
            session.to_string(),
            "99".to_owned(),
            tpgid.to_string(),
        ];
        while rest.len() < 19 {
            rest.push("0".to_owned());
        }
        rest.push(started.to_owned());
        format!("{pid} (command with space) {}", rest.join(" "))
    }

    #[test]
    fn parses_parenthesized_linux_stat_and_rejects_malformed_rows() {
        assert!(parse_proc_stat("bad").is_none());
        assert!(parse_proc_stat("1 () S").is_none());
        assert!(parse_proc_stat(&stat(10, 20, 30, 40, "500", 1, "SS")).is_none());
        assert_eq!(
            parse_proc_stat(&stat(10, 20, 30, 40, "500", 1, "S")),
            Some(ProcStat {
                pid: ProcessId::new(10),
                parent_pid: ProcessId::new(1),
                process_group_id: ProcessGroupId::new(20),
                session_id: ProcessId::new(30),
                state: 'S',
                terminal_process_group_id: Some(ProcessGroupId::new(40)),
                started: ProcessStartIdentity::new("500"),
            })
        );
    }

    #[test]
    fn rooted_tree_is_children_first_cycle_safe_and_scoped() {
        let entry = |pid, parent, started| ProcessTreeEntry {
            identity: ProcessIdentity {
                pid: ProcessId::new(pid),
                started: ProcessStartIdentity::new(started),
            },
            parent_pid: ProcessId::new(parent),
        };
        let entries = [
            entry(10, 11, "root"),
            entry(11, 10, "child"),
            entry(99, 1, "unrelated"),
        ];
        assert_eq!(
            rooted_process_tree(&entries, ProcessId::new(10))
                .into_iter()
                .map(|identity| identity.pid.as_i64())
                .collect::<Vec<_>>(),
            vec![11, 10]
        );
        assert!(rooted_process_tree(&entries, ProcessId::new(1)).is_empty());
    }
}
