//! Deterministic injected-boundary parity for Linux and macOS process inspection.

use std::{collections::BTreeMap, io, sync::Arc};

use parking_lot::Mutex;
use seekdeep_subprocess::{ProcessGroupId, ProcessId, SubprocessTerminalSignal};
use seekdeep_subprocess_local::process_inspector::{
    InspectorArchitecture, InspectorPlatform, InspectorSignal, ProcessIdentity,
    ProcessInspectorInternals, ProcessStartIdentity, create_process_inspector_for,
    linux_process_group_has_live_members, parse_proc_stat,
};

#[derive(Debug, Default)]
struct FakeInternals {
    files: Mutex<BTreeMap<String, String>>,
    dirs: Mutex<BTreeMap<String, Vec<String>>>,
    memories: Mutex<BTreeMap<i64, Vec<u8>>>,
    kills: Mutex<Vec<(i64, InspectorSignal)>>,
    ps: Mutex<Option<String>>,
    tpgid: Mutex<Option<String>>,
}

impl FakeInternals {
    fn file(&self, path: &str, text: impl Into<String>) {
        self.files.lock().insert(path.to_owned(), text.into());
    }

    fn dir(&self, path: &str, entries: &[&str]) {
        self.dirs.lock().insert(
            path.to_owned(),
            entries.iter().map(|entry| (*entry).to_owned()).collect(),
        );
    }

    fn memory(&self, pid: i64, bytes: Vec<u8>) {
        self.memories.lock().insert(pid, bytes);
    }
}

impl ProcessInspectorInternals for FakeInternals {
    fn read_file(&self, path: &str) -> io::Result<String> {
        self.files
            .lock()
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.to_owned()))
    }

    fn read_dir(&self, path: &str) -> io::Result<Vec<String>> {
        self.dirs
            .lock()
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.to_owned()))
    }

    fn read_memory(&self, pid: ProcessId, address: u64, length: usize) -> io::Result<Vec<u8>> {
        let memory = self
            .memories
            .lock()
            .get(&pid.as_i64())
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "memory"))?;
        let start = usize::try_from(address).map_err(io::Error::other)?;
        if start >= memory.len() {
            return Ok(Vec::new());
        }
        Ok(memory[start..start.saturating_add(length).min(memory.len())].to_vec())
    }

    fn exec(&self, _file: &str, args: &[String]) -> io::Result<String> {
        let value = if args.iter().any(|argument| argument == "tpgid=") {
            self.tpgid.lock().clone()
        } else {
            self.ps.lock().clone()
        };
        value.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "command result"))
    }

    fn kill(&self, pid: i64, signal: InspectorSignal) -> io::Result<()> {
        self.kills.lock().push((pid, signal));
        Ok(())
    }
}

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

fn syscall(number: i64, args: &[u64]) -> String {
    let mut values = args.to_vec();
    values.resize(6, 0);
    format!(
        "{number} {}",
        values
            .iter()
            .take(6)
            .map(|value| format!("0x{value:x}"))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

#[test]
fn linux_group_liveness_distinguishes_zombies_and_unobservability() {
    let fake = FakeInternals::default();
    assert_eq!(
        linux_process_group_has_live_members(ProcessGroupId::new(77), &fake),
        None
    );
    fake.dir("/proc", &["self", "10", "11", "12"]);
    fake.file("/proc/10/stat", stat(10, 77, 10, -1, "500", 1, "Z"));
    fake.file("/proc/11/stat", stat(11, 77, 10, -1, "501", 1, "X"));
    fake.file("/proc/12/stat", stat(12, 88, 12, -1, "502", 1, "S"));
    assert_eq!(
        linux_process_group_has_live_members(ProcessGroupId::new(77), &fake),
        Some(false)
    );
    assert_eq!(
        linux_process_group_has_live_members(ProcessGroupId::new(99), &fake),
        None
    );
    fake.file("/proc/11/stat", stat(11, 77, 10, -1, "501", 1, "S"));
    assert_eq!(
        linux_process_group_has_live_members(ProcessGroupId::new(77), &fake),
        Some(true)
    );
}

#[test]
fn linux_tree_session_identity_and_signals_match_the_source_contract() {
    assert!(parse_proc_stat("bad").is_none());
    let fake = Arc::new(FakeInternals::default());
    fake.dir("/proc", &["x", "10", "11", "12", "13", "14"]);
    fake.file("/proc/10/stat", stat(10, 20, 30, 40, "500", 1, "S"));
    fake.file("/proc/11/stat", stat(11, 21, 30, -1, "501", 1, "S"));
    fake.file("/proc/12/stat", stat(12, 22, 30, -1, "502", 10, "S"));
    fake.file("/proc/13/stat", stat(13, 23, 30, -1, "503", 12, "S"));
    let inspector = create_process_inspector_for(
        InspectorPlatform::Linux,
        InspectorArchitecture::X86_64,
        fake.clone(),
    )
    .unwrap();
    assert_eq!(
        inspector.foreground_pgid(ProcessId::new(10)),
        Some(ProcessGroupId::new(40))
    );
    assert_eq!(
        inspector
            .process_tree(ProcessId::new(10))
            .into_iter()
            .map(|identity| identity.pid.as_i64())
            .collect::<Vec<_>>(),
        vec![13, 12, 10]
    );
    assert_eq!(
        inspector
            .process_session(ProcessId::new(30))
            .into_iter()
            .map(|identity| identity.pid.as_i64())
            .collect::<Vec<_>>(),
        vec![10, 11, 12, 13]
    );
    let current = ProcessIdentity {
        pid: ProcessId::new(10),
        started: ProcessStartIdentity::new("500"),
    };
    let recycled = ProcessIdentity {
        pid: ProcessId::new(10),
        started: ProcessStartIdentity::new("old"),
    };
    assert!(inspector.is_alive(&current));
    assert!(!inspector.is_alive(&recycled));
    inspector
        .signal_group(ProcessGroupId::new(40), SubprocessTerminalSignal::Sigint)
        .unwrap();
    inspector.signal_process(&current, false).unwrap();
    inspector.signal_process(&recycled, true).unwrap();
    assert_eq!(
        *fake.kills.lock(),
        vec![
            (-40, InspectorSignal::Sigint),
            (10, InspectorSignal::Sigterm)
        ]
    );
    fake.file("/proc/10/stat", stat(10, 20, 30, 40, "500", 1, "Z"));
    assert!(!inspector.is_alive(&current));
}

#[test]
fn linux_detects_read_select_poll_and_epoll_waits_and_fails_closed() {
    let fake = Arc::new(FakeInternals::default());
    fake.dir("/proc", &["100", "101"]);
    fake.file("/proc/100/stat", stat(100, 77, 100, 77, "1", 1, "S"));
    fake.file("/proc/101/stat", stat(101, 77, 100, 77, "2", 1, "S"));
    fake.dir("/proc/100/task", &["100"]);
    fake.dir("/proc/101/task", &["101", "102"]);
    fake.file("/proc/100/task/100/syscall", "running");
    fake.file("/proc/101/task/101/syscall", "-1 0x0");
    let inspector = create_process_inspector_for(
        InspectorPlatform::Linux,
        InspectorArchitecture::X86_64,
        fake.clone(),
    )
    .unwrap();

    fake.file("/proc/101/task/102/syscall", syscall(0, &[0]));
    assert!(inspector.is_stdin_waiting(ProcessGroupId::new(77)));

    fake.file("/proc/101/task/102/syscall", syscall(270, &[1, 0x10]));
    let mut fd_set = vec![0_u8; 0x11];
    fd_set[0x10] = 1;
    fake.memory(101, fd_set);
    assert!(inspector.is_stdin_waiting(ProcessGroupId::new(77)));

    let mut poll = vec![0_u8; 0x28];
    poll[0x20..0x24].copy_from_slice(&0_i32.to_le_bytes());
    poll[0x24..0x26].copy_from_slice(&1_i16.to_le_bytes());
    fake.memory(101, poll);
    fake.file("/proc/101/task/102/syscall", syscall(7, &[0x20, 1]));
    assert!(inspector.is_stdin_waiting(ProcessGroupId::new(77)));

    fake.file("/proc/101/task/102/syscall", syscall(232, &[5, 0, 1]));
    fake.file("/proc/101/fdinfo/5", "pos: 0\ntfd: 0 events: 19\n");
    assert!(inspector.is_stdin_waiting(ProcessGroupId::new(77)));

    let unsupported = create_process_inspector_for(
        InspectorPlatform::Linux,
        InspectorArchitecture::Unknown("mips".to_owned()),
        fake,
    )
    .unwrap();
    assert!(!unsupported.is_stdin_waiting(ProcessGroupId::new(77)));
}

#[test]
fn mac_process_table_is_cycle_safe_and_identity_fences_signals() {
    let fake = Arc::new(FakeInternals::default());
    *fake.tpgid.lock() = Some("55\n".to_owned());
    *fake.ps.lock() = Some(
        " 10 1 Mon Jul 21 10:00:00 2026\n 11 10 Mon Jul 21 10:00:01 2026\n 12 11 Mon Jul 21 10:00:02 2026\n 13 99 Mon Jul 21 10:00:03 2026\nmalformed\n"
            .to_owned(),
    );
    let inspector = create_process_inspector_for(
        InspectorPlatform::MacOs,
        InspectorArchitecture::Aarch64,
        fake.clone(),
    )
    .unwrap();
    assert_eq!(
        inspector.foreground_pgid(ProcessId::new(10)),
        Some(ProcessGroupId::new(55))
    );
    assert_eq!(
        inspector
            .process_tree(ProcessId::new(10))
            .into_iter()
            .map(|identity| identity.pid.as_i64())
            .collect::<Vec<_>>(),
        vec![12, 11, 10]
    );
    let child = ProcessIdentity {
        pid: ProcessId::new(11),
        started: ProcessStartIdentity::new("Mon Jul 21 10:00:01 2026"),
    };
    inspector
        .signal_group(ProcessGroupId::new(55), SubprocessTerminalSignal::Sigtstp)
        .unwrap();
    inspector.signal_process(&child, true).unwrap();
    assert_eq!(
        *fake.kills.lock(),
        vec![
            (-55, InspectorSignal::Sigtstp),
            (11, InspectorSignal::Sigkill)
        ]
    );
    *fake.ps.lock() =
        Some(" 10 11 Mon Jul 21 10:00:00 2026\n 11 10 Mon Jul 21 10:00:01 2026\n".to_owned());
    assert_eq!(inspector.process_tree(ProcessId::new(10)).len(), 2);
}

#[test]
fn invalid_foreground_and_unknown_platform_fail_at_the_source_boundary() {
    let fake = Arc::new(FakeInternals::default());
    *fake.tpgid.lock() = Some("-1".to_owned());
    let inspector = create_process_inspector_for(
        InspectorPlatform::MacOs,
        InspectorArchitecture::Aarch64,
        fake.clone(),
    )
    .unwrap();
    assert_eq!(inspector.foreground_pgid(ProcessId::new(1)), None);
    *fake.tpgid.lock() = None;
    assert_eq!(inspector.foreground_pgid(ProcessId::new(1)), None);
    assert_eq!(
        create_process_inspector_for(
            InspectorPlatform::Unknown("win32".to_owned()),
            InspectorArchitecture::X86_64,
            fake,
        )
        .unwrap_err()
        .to_string(),
        "subprocess-local: terminal inspection is unsupported on platform win32"
    );
}
