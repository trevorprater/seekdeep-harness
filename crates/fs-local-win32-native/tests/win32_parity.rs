//! Host-independent Win32 binding orchestration parity.

use std::{path::Path, sync::Mutex};

use seekdeep_fs_local_win32_native::{
    Win32FileApi, copy_file_dacl_with, namespaced_path, read_file_dacl_with, replace_file_with,
};

#[derive(Debug)]
struct State {
    descriptor: Vec<u8>,
    probe_needed: Option<u32>,
    read_success: bool,
    set_success: bool,
    replace_success: bool,
    last_error: u32,
    calls: Vec<String>,
    installed: Vec<(String, u32, Vec<u8>)>,
    replacements: Vec<(String, String)>,
}

#[derive(Debug)]
struct FakeApi(Mutex<State>);

impl FakeApi {
    fn successful(descriptor: &[u8]) -> Self {
        Self(Mutex::new(State {
            descriptor: descriptor.to_vec(),
            probe_needed: None,
            read_success: true,
            set_success: true,
            replace_success: true,
            last_error: 0,
            calls: Vec::new(),
            installed: Vec::new(),
            replacements: Vec::new(),
        }))
    }
}

impl Win32FileApi for FakeApi {
    fn get_file_security(
        &self,
        path: &str,
        requested: u32,
        descriptor: Option<&mut [u8]>,
        needed: &mut u32,
    ) -> bool {
        let mut state = self.0.lock().unwrap();
        state.calls.push(format!("get:{path}:{requested}"));
        *needed = state
            .probe_needed
            .unwrap_or_else(|| u32::try_from(state.descriptor.len()).unwrap());
        let Some(output) = descriptor else {
            return false;
        };
        if !state.read_success {
            return false;
        }
        let length = output.len().min(state.descriptor.len());
        output[..length].copy_from_slice(&state.descriptor[..length]);
        true
    }

    fn set_file_security(&self, path: &str, information: u32, descriptor: &[u8]) -> bool {
        let mut state = self.0.lock().unwrap();
        state.calls.push(format!("set:{path}:{information}"));
        state
            .installed
            .push((path.to_owned(), information, descriptor.to_vec()));
        state.set_success
    }

    fn replace_file(&self, replaced: &str, replacement: &str) -> bool {
        let mut state = self.0.lock().unwrap();
        state
            .calls
            .push(format!("replace:{replaced}:{replacement}"));
        state
            .replacements
            .push((replaced.to_owned(), replacement.to_owned()));
        state.replace_success
    }

    fn last_error(&self) -> u32 {
        self.0.lock().unwrap().last_error
    }
}

#[test]
fn reads_installs_and_replaces_with_namespaced_paths_and_exact_descriptor() {
    let api = FakeApi::successful(&[1, 2, 3, 4]);
    assert_eq!(
        read_file_dacl_with(&api, Path::new(r"C:\source")).unwrap(),
        [1, 2, 3, 4]
    );
    copy_file_dacl_with(&api, Path::new(r"C:\source"), Path::new(r"C:\temp")).unwrap();
    replace_file_with(&api, Path::new(r"C:\target"), Path::new(r"C:\temp")).unwrap();
    let state = api.0.lock().unwrap();
    assert_eq!(state.installed.len(), 1);
    assert_eq!(state.installed[0].0, r"\\?\C:\temp");
    assert_eq!(state.installed[0].1, 0x8000_0004);
    assert_eq!(state.installed[0].2, [1, 2, 3, 4]);
    assert_eq!(
        state.replacements,
        [(r"\\?\C:\target".to_owned(), r"\\?\C:\temp".to_owned())]
    );
    assert_eq!(
        namespaced_path(Path::new(r"\\server\share\file")),
        r"\\?\UNC\server\share\file"
    );
}

#[test]
fn descriptor_probe_and_read_failures_preserve_raw_win32_codes() {
    for code in [2_u32, 3, 5, 9_999] {
        let api = FakeApi::successful(&[1]);
        {
            let mut state = api.0.lock().unwrap();
            state.probe_needed = Some(0);
            state.last_error = code;
        }
        assert_eq!(
            read_file_dacl_with(&api, Path::new("source"))
                .unwrap_err()
                .raw_os_error(),
            Some(i32::try_from(code).unwrap())
        );
    }

    let api = FakeApi::successful(&[1, 2]);
    {
        let mut state = api.0.lock().unwrap();
        state.read_success = false;
        state.last_error = 5;
    }
    assert_eq!(
        read_file_dacl_with(&api, Path::new("source"))
            .unwrap_err()
            .raw_os_error(),
        Some(5)
    );
}

#[test]
fn installation_and_replacement_failures_preserve_raw_win32_codes() {
    let set_api = FakeApi::successful(&[1]);
    {
        let mut state = set_api.0.lock().unwrap();
        state.set_success = false;
        state.last_error = 5;
    }
    assert_eq!(
        copy_file_dacl_with(&set_api, Path::new("source"), Path::new("temp"))
            .unwrap_err()
            .raw_os_error(),
        Some(5)
    );

    let replace_api = FakeApi::successful(&[1]);
    {
        let mut state = replace_api.0.lock().unwrap();
        state.replace_success = false;
        state.last_error = 2;
    }
    assert_eq!(
        replace_file_with(&replace_api, Path::new("target"), Path::new("temp"))
            .unwrap_err()
            .raw_os_error(),
        Some(2)
    );
}
