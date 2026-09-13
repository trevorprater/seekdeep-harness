//! Pure resolution and executable PATH probe parity.

use std::{ffi::OsString, path::PathBuf};

use seekdeep_host_directory_picker_auto::{
    DirectoryPickerBackendKind, DirectoryPickerEnv, DirectoryPickerHostFacts, ResolvePlatform,
    has_linux_chooser_binary, resolve_directory_picker_backend,
};
use seekdeep_host_webserver::ListenHost;

fn attended() -> DirectoryPickerHostFacts {
    DirectoryPickerHostFacts {
        bind_host: ListenHost::Loopback,
        platform: ResolvePlatform::Darwin,
        env: DirectoryPickerEnv::default(),
        linux_chooser: false,
    }
}

#[test]
fn resolver_requires_local_non_ssh_and_a_servable_display_platform() {
    assert_eq!(
        resolve_directory_picker_backend(&attended()),
        DirectoryPickerBackendKind::Native
    );
    assert_eq!(
        resolve_directory_picker_backend(&DirectoryPickerHostFacts {
            platform: ResolvePlatform::Win32,
            ..attended()
        }),
        DirectoryPickerBackendKind::Native
    );
    assert_eq!(
        resolve_directory_picker_backend(&DirectoryPickerHostFacts {
            bind_host: ListenHost::AllInterfaces,
            ..attended()
        }),
        DirectoryPickerBackendKind::Browse
    );
    for env in [
        DirectoryPickerEnv {
            ssh_connection: Some("10.0.0.2 55 10.0.0.9 22".to_owned()),
            ..DirectoryPickerEnv::default()
        },
        DirectoryPickerEnv {
            ssh_tty: Some("/dev/pts/3".to_owned()),
            ..DirectoryPickerEnv::default()
        },
    ] {
        assert_eq!(
            resolve_directory_picker_backend(&DirectoryPickerHostFacts { env, ..attended() }),
            DirectoryPickerBackendKind::Browse
        );
    }

    let linux = DirectoryPickerHostFacts {
        platform: ResolvePlatform::Linux,
        linux_chooser: true,
        ..attended()
    };
    assert_eq!(
        resolve_directory_picker_backend(&linux),
        DirectoryPickerBackendKind::Browse
    );
    for env in [
        DirectoryPickerEnv {
            display: Some(":0".to_owned()),
            ..DirectoryPickerEnv::default()
        },
        DirectoryPickerEnv {
            wayland_display: Some("wayland-1".to_owned()),
            ..DirectoryPickerEnv::default()
        },
    ] {
        assert_eq!(
            resolve_directory_picker_backend(&DirectoryPickerHostFacts {
                env,
                ..linux.clone()
            }),
            DirectoryPickerBackendKind::Native
        );
    }
    assert_eq!(
        resolve_directory_picker_backend(&DirectoryPickerHostFacts {
            linux_chooser: false,
            env: DirectoryPickerEnv {
                display: Some(":0".to_owned()),
                ..DirectoryPickerEnv::default()
            },
            ..linux
        }),
        DirectoryPickerBackendKind::Browse
    );
    for platform in [
        ResolvePlatform::Other("freebsd".to_owned()),
        ResolvePlatform::Other("openbsd".to_owned()),
    ] {
        assert_eq!(
            resolve_directory_picker_backend(&DirectoryPickerHostFacts {
                platform,
                env: DirectoryPickerEnv {
                    display: Some(":0".to_owned()),
                    ..DirectoryPickerEnv::default()
                },
                linux_chooser: true,
                ..attended()
            }),
            DirectoryPickerBackendKind::Browse
        );
    }
}

#[test]
fn blank_exports_are_unset_but_whitespace_preserves_source_presence() {
    assert_eq!(
        resolve_directory_picker_backend(&DirectoryPickerHostFacts {
            env: DirectoryPickerEnv {
                ssh_connection: Some(String::new()),
                ssh_tty: Some(String::new()),
                ..DirectoryPickerEnv::default()
            },
            ..attended()
        }),
        DirectoryPickerBackendKind::Native
    );
    assert_eq!(
        resolve_directory_picker_backend(&DirectoryPickerHostFacts {
            env: DirectoryPickerEnv {
                ssh_connection: Some(" ".to_owned()),
                ..DirectoryPickerEnv::default()
            },
            ..attended()
        }),
        DirectoryPickerBackendKind::Browse
    );
    assert_eq!(
        resolve_directory_picker_backend(&DirectoryPickerHostFacts {
            platform: ResolvePlatform::Linux,
            linux_chooser: true,
            env: DirectoryPickerEnv {
                display: Some(String::new()),
                wayland_display: Some(String::new()),
                ..DirectoryPickerEnv::default()
            },
            ..attended()
        }),
        DirectoryPickerBackendKind::Browse
    );
}

#[test]
fn path_probe_skips_empty_segments_and_checks_zenity_before_kdialog() {
    let path = std::env::join_paths([PathBuf::new(), "/opt/none".into(), "/usr/local/bin".into()])
        .unwrap();
    let seen = std::cell::RefCell::new(Vec::new());
    assert!(has_linux_chooser_binary(Some(&path), |candidate| {
        seen.borrow_mut().push(candidate.to_path_buf());
        candidate == std::path::Path::new("/usr/local/bin/kdialog")
    }));
    assert_eq!(
        seen.into_inner(),
        [
            PathBuf::from("/opt/none/zenity"),
            PathBuf::from("/opt/none/kdialog"),
            PathBuf::from("/usr/local/bin/zenity"),
            PathBuf::from("/usr/local/bin/kdialog"),
        ]
    );
    assert!(!has_linux_chooser_binary(Some(&OsString::new()), |_| true));
    assert!(!has_linux_chooser_binary(None, |_| true));
}

#[cfg(unix)]
#[test]
fn executable_probe_accepts_only_an_executable_file() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("zenity");
    std::fs::write(&binary, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(seekdeep_host_directory_picker_auto::can_execute(&binary));
    assert!(!seekdeep_host_directory_picker_auto::can_execute(
        &root.path().join("kdialog")
    ));
    assert!(!seekdeep_host_directory_picker_auto::can_execute(
        root.path()
    ));
}
