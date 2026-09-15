//! Real compiled-runner conformance against the source `runner.spec.ts` matrix.

#![cfg(windows)]

use std::{ffi::OsStr, path::Path, process::Command, sync::Arc};

use seekdeep_sandbox_windows_acl::{
    AclWriteGrant, GrantBindings, temp_write_sid, workspace_write_sid,
};
use seekdeep_sandbox_windows_acl_native::WindowsBindings;

mod windows_support;

use windows_support::{node_path, prerequisites_available, pwsh_path};

const RUNNER: &str = env!("CARGO_BIN_EXE_windows-acl-run");

fn ps_literal(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn grant_binding() -> Arc<dyn GrantBindings> {
    Arc::new(WindowsBindings)
}

fn run_runner<I, S>(args: I) -> std::process::Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(RUNNER)
        .args(args)
        .output()
        .expect("compiled windows-acl-run must launch")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_exit(output: &std::process::Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout:\n{}\nstderr:\n{}",
        stdout(output),
        stderr(output)
    );
}

struct RunnerFixture {
    _scratch: tempfile::TempDir,
    _isolated_temp: tempfile::TempDir,
    workspace: std::path::PathBuf,
    temp: std::path::PathBuf,
    secret: std::path::PathBuf,
    escape: std::path::PathBuf,
    pwsh: String,
    node: String,
}

impl RunnerFixture {
    fn new() -> Self {
        let scratch = tempfile::Builder::new()
            .prefix("seekdeep-acl-runner-")
            .tempdir()
            .unwrap();
        let isolated_temp = tempfile::Builder::new()
            .prefix("seekdeep-acl-runner-temp-")
            .tempdir()
            .unwrap();
        let workspace = scratch.path().join("writable");
        std::fs::create_dir(&workspace).unwrap();
        let secret = scratch.path().join("secret.txt");
        std::fs::write(&secret, "read boundary").unwrap();
        let escape = scratch.path().join("escaped.txt");
        let temp = isolated_temp.path().to_owned();
        Self {
            _scratch: scratch,
            _isolated_temp: isolated_temp,
            workspace,
            temp,
            secret,
            escape,
            pwsh: pwsh_path().expect("prerequisites checked").to_owned(),
            node: node_path().expect("prerequisites checked").to_owned(),
        }
    }

    fn prefix(&self, mode: &str) -> Vec<String> {
        vec![
            "--workspace".into(),
            path_text(&self.workspace),
            "--temp".into(),
            path_text(&self.temp),
            "--mode".into(),
            mode.into(),
            "--".into(),
        ]
    }

    fn pwsh(&self, mode: &str, script: String) -> std::process::Output {
        let mut args = self.prefix(mode);
        args.extend([
            self.pwsh.clone(),
            "/NoLogo".into(),
            "/NonInteractive".into(),
            "/NoProfile".into(),
            "/Command".into(),
            script,
        ]);
        run_runner(args)
    }
}

#[test]
fn workspace_write_confines_writes_but_preserves_reads() {
    if !prerequisites_available() {
        return;
    }
    let fixture = RunnerFixture::new();
    let target = fixture.workspace.join("child-wrote.txt");
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue';\
         'LANGMODE: ' + $ExecutionContext.SessionState.LanguageMode;\
         try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'TARGET-WRITE: OK'}}catch{{'TARGET-WRITE: DENIED'}};\
         try{{Set-Content -LiteralPath (Join-Path $env:TEMP 'child-wrote.txt') -Value ok -ErrorAction Stop;'TEMP-WRITE: OK'}}catch{{'TEMP-WRITE: DENIED'}};\
         try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'ESCAPE-WRITE: OK (ESCAPE!)'}}catch{{'ESCAPE-WRITE: DENIED'}};\
         try{{Get-Content -LiteralPath '{}' -ErrorAction Stop | Out-Null;'SECRET-READ: OK'}}catch{{'SECRET-READ: DENIED'}};\
         try{{Get-CimInstance Win32_OperatingSystem -ErrorAction Stop | Out-Null;'CIM: OK'}}catch{{'CIM: DENIED'}}",
        ps_literal(&target),
        ps_literal(&fixture.escape),
        ps_literal(&fixture.secret),
    );
    let output = fixture.pwsh("workspace-write", script);
    assert_exit(&output, 0);
    let stdout = stdout(&output);
    assert!(stdout.contains("LANGMODE: FullLanguage"), "{stdout}");
    assert!(stdout.contains("TARGET-WRITE: OK"), "{stdout}");
    assert!(stdout.contains("TEMP-WRITE: OK"), "{stdout}");
    assert!(stdout.contains("ESCAPE-WRITE: DENIED"), "{stdout}");
    assert!(stdout.contains("SECRET-READ: OK"), "{stdout}");
    assert!(stdout.contains("CIM: DENIED"), "{stdout}");
    assert!(!fixture.escape.exists());
    assert!(target.exists());
}

#[test]
fn read_only_denies_writes_but_keeps_reads_and_null_redirection() {
    if !prerequisites_available() {
        return;
    }
    let fixture = RunnerFixture::new();
    let workspace_target = fixture.workspace.join("readonly-child-wrote.txt");
    let temp_target = fixture.temp.join("readonly-child-wrote.txt");
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue';\
         'LANGMODE: ' + $ExecutionContext.SessionState.LanguageMode;\
         try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'TARGET-WRITE: OK'}}catch{{'TARGET-WRITE: DENIED'}};\
         try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'TEMP-WRITE: OK'}}catch{{'TEMP-WRITE: DENIED'}};\
         try{{Set-Content -LiteralPath 'NUL' -Value ok -ErrorAction Stop;'NUL-WRITE: OK'}}catch{{'NUL-WRITE: DENIED'}};\
         echo hi > $null;'DOLLAR-NULL: OK';\
         try{{Get-Content -LiteralPath '{}' -ErrorAction Stop | Out-Null;'SECRET-READ: OK'}}catch{{'SECRET-READ: DENIED'}};\
         try{{Get-CimInstance Win32_OperatingSystem -ErrorAction Stop | Out-Null;'CIM: OK'}}catch{{'CIM: DENIED'}}",
        ps_literal(&workspace_target),
        ps_literal(&temp_target),
        ps_literal(&fixture.secret),
    );
    let output = fixture.pwsh("read-only", script);
    assert_exit(&output, 0);
    let stdout = stdout(&output);
    assert!(stdout.contains("LANGMODE: ConstrainedLanguage"), "{stdout}");
    assert!(stdout.contains("TARGET-WRITE: DENIED"), "{stdout}");
    assert!(stdout.contains("TEMP-WRITE: DENIED"), "{stdout}");
    assert!(stdout.contains("NUL-WRITE: DENIED"), "{stdout}");
    assert!(stdout.contains("DOLLAR-NULL: OK"), "{stdout}");
    assert!(stdout.contains("SECRET-READ: OK"), "{stdout}");
    assert!(stdout.contains("CIM: DENIED"), "{stdout}");
    assert!(!workspace_target.exists());
}

#[test]
fn workspace_grant_carries_delete_and_file_delete_child_rights() {
    if !prerequisites_available() {
        return;
    }
    let fixture = RunnerFixture::new();
    let victim = fixture.workspace.join("delete-me.txt");
    let directory = fixture.workspace.join("rename-me");
    let renamed = fixture.workspace.join("renamed-by-child");
    std::fs::write(&victim, "remove me").unwrap();
    std::fs::create_dir(&directory).unwrap();
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue';\
         try{{Remove-Item -LiteralPath '{}' -ErrorAction Stop;'DELETE-FILE: OK'}}catch{{'DELETE-FILE: DENIED'}};\
         try{{Rename-Item -LiteralPath '{}' -NewName 'renamed-by-child' -ErrorAction Stop;'RENAME-DIR: OK'}}catch{{'RENAME-DIR: DENIED'}}",
        ps_literal(&victim),
        ps_literal(&directory),
    );
    let output = fixture.pwsh("workspace-write", script);
    assert_exit(&output, 0);
    let stdout = stdout(&output);
    assert!(stdout.contains("DELETE-FILE: OK"), "{stdout}");
    assert!(stdout.contains("RENAME-DIR: OK"), "{stdout}");
    assert!(!victim.exists());
    assert!(renamed.exists());
}

#[test]
fn seam_managed_sids_trust_only_caller_materialized_grants() {
    if !prerequisites_available() {
        return;
    }
    let fixture = RunnerFixture::new();
    let workspace = fixture._scratch.path().join("seam-workspace");
    let private_temp = fixture.temp.join("private-subdir");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&private_temp).unwrap();
    let workspace_sid = workspace_write_sid(&path_text(&workspace));
    let private_sid = temp_write_sid(&path_text(&private_temp));
    let mut grant = AclWriteGrant::create(&private_sid, grant_binding()).unwrap();
    grant.add(&private_temp, false).unwrap();

    let workspace_target = workspace.join("server-granted.txt");
    let private_target = private_temp.join("server-granted.txt");
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue';\
         try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'WORKSPACE-WRITE: OK'}}catch{{'WORKSPACE-WRITE: DENIED'}};\
         try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'PRIVATE-TEMP-WRITE: OK'}}catch{{'PRIVATE-TEMP-WRITE: DENIED'}};\
         'TEMP-ENV: ' + $env:TEMP;'TMP-ENV: ' + $env:TMP",
        ps_literal(&workspace_target),
        ps_literal(&private_target),
    );
    let args = [
        "--workspace",
        &path_text(&workspace),
        "--temp",
        &path_text(&private_temp),
        "--mode",
        "workspace-write",
        "--write-sid",
        &workspace_sid,
        "--temp-write-sid",
        &private_sid,
        "--",
        fixture.pwsh.as_str(),
        "/NoLogo",
        "/NonInteractive",
        "/NoProfile",
        "/Command",
        &script,
    ];
    let output = run_runner(args);
    let cleanup = grant.dispose();
    assert_exit(&output, 0);
    cleanup.unwrap();
    let stdout = stdout(&output);
    assert!(stdout.contains("WORKSPACE-WRITE: DENIED"), "{stdout}");
    assert!(stdout.contains("PRIVATE-TEMP-WRITE: OK"), "{stdout}");
    assert!(
        stdout.contains(&format!("TEMP-ENV: {}", path_text(&private_temp))),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("TMP-ENV: {}", path_text(&private_temp))),
        "{stdout}"
    );
    assert!(!workspace_target.exists());
    assert!(private_target.exists());
}

#[test]
fn sibling_temp_capabilities_are_isolated_while_workspace_is_shared() {
    if !prerequisites_available() {
        return;
    }
    let fixture = RunnerFixture::new();
    let temp_a = fixture.temp.join("session-a");
    let temp_b = fixture.temp.join("session-b");
    std::fs::create_dir(&temp_a).unwrap();
    std::fs::create_dir(&temp_b).unwrap();
    let workspace_sid = workspace_write_sid(&path_text(&fixture.workspace));
    let sid_a = temp_write_sid(&path_text(&temp_a));
    let sid_b = temp_write_sid(&path_text(&temp_b));
    let mut workspace_grant = AclWriteGrant::create(&workspace_sid, grant_binding()).unwrap();
    let mut grant_a = AclWriteGrant::create(&sid_a, grant_binding()).unwrap();
    let mut grant_b = AclWriteGrant::create(&sid_b, grant_binding()).unwrap();
    workspace_grant.add(&fixture.workspace, false).unwrap();
    grant_a.add(&temp_a, false).unwrap();
    grant_b.add(&temp_b, false).unwrap();

    let shared = fixture.workspace.join("shared-between-sessions.txt");
    let probe = "const fs=require('node:fs');const targets=[['OWN',process.argv[1]],['SIBLING',process.argv[2]],['WORKSPACE',process.argv[3]]];if(process.argv[4])targets.push(['SIBLING-EXISTING',process.argv[4]]);for(const [name,target] of targets){try{fs.writeFileSync(target,name);console.log(name+': OK')}catch{console.log(name+': DENIED')}}";
    let run_session =
        |temp: &Path, sid: &str, own: &Path, sibling: &Path, existing: Option<&Path>| {
            let mut args = vec![
                "--workspace".into(),
                path_text(&fixture.workspace),
                "--temp".into(),
                path_text(temp),
                "--mode".into(),
                "workspace-write".into(),
                "--write-sid".into(),
                workspace_sid.clone(),
                "--temp-write-sid".into(),
                sid.to_owned(),
                "--".into(),
                fixture.node.clone(),
                "-e".into(),
                probe.into(),
                path_text(own),
                path_text(sibling),
                path_text(&shared),
            ];
            if let Some(existing) = existing {
                args.push(path_text(existing));
            }
            run_runner(args)
        };

    let own_a = temp_a.join("a.txt");
    let output_a = run_session(&temp_a, &sid_a, &own_a, &temp_b.join("a-escaped.txt"), None);
    assert_exit(&output_a, 0);
    assert!(stdout(&output_a).contains("OWN: OK"));
    assert!(stdout(&output_a).contains("SIBLING: DENIED"));
    assert!(stdout(&output_a).contains("WORKSPACE: OK"));

    let output_b = run_session(
        &temp_b,
        &sid_b,
        &temp_b.join("b.txt"),
        &temp_a.join("b-escaped.txt"),
        Some(&own_a),
    );
    let cleanup_workspace = workspace_grant.dispose();
    let cleanup_a = grant_a.dispose();
    let cleanup_b = grant_b.dispose();
    assert_exit(&output_b, 0);
    cleanup_workspace.unwrap();
    cleanup_a.unwrap();
    cleanup_b.unwrap();
    let stdout_b = stdout(&output_b);
    assert!(stdout_b.contains("OWN: OK"), "{stdout_b}");
    assert!(stdout_b.contains("SIBLING: DENIED"), "{stdout_b}");
    assert!(stdout_b.contains("SIBLING-EXISTING: DENIED"), "{stdout_b}");
    assert!(stdout_b.contains("WORKSPACE: OK"), "{stdout_b}");
    assert!(!temp_b.join("a-escaped.txt").exists());
    assert!(!temp_a.join("b-escaped.txt").exists());
    assert_eq!(std::fs::read_to_string(own_a).unwrap(), "OWN");
}

#[test]
fn agentless_calls_use_fresh_private_temp_directories_and_remove_them() {
    if !prerequisites_available() {
        return;
    }
    let fixture = RunnerFixture::new();
    let mut captured = Vec::new();
    for name in ["agentless-temp-a.txt", "agentless-temp-b.txt"] {
        let capture = fixture.workspace.join(name);
        let mut args = fixture.prefix("workspace-write");
        args.extend([
            fixture.node.clone(),
            "-e".into(),
            "require('node:fs').writeFileSync(process.argv[1],process.env.TEMP)".into(),
            path_text(&capture),
        ]);
        let output = run_runner(args);
        assert_exit(&output, 0);
        captured.push(std::fs::read_to_string(capture).unwrap());
    }
    assert_ne!(captured[0], captured[1]);
    assert!(
        captured
            .iter()
            .all(|path| path.starts_with(&path_text(&fixture.temp)))
    );
    assert!(captured.iter().all(|path| !Path::new(path).exists()));
}

#[test]
fn agentless_overlap_fails_before_spawning_the_command() {
    if !prerequisites_available() {
        return;
    }
    let fixture = RunnerFixture::new();
    let workspace = fixture._scratch.path().join("overlap-workspace");
    let nested_temp = workspace.join("temp");
    let marker = workspace.join("command-ran.txt");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&nested_temp).unwrap();
    let output = run_runner([
        "--workspace",
        &path_text(&workspace),
        "--temp",
        &path_text(&nested_temp),
        "--mode",
        "workspace-write",
        "--",
        fixture.node.as_str(),
        "-e",
        "require('node:fs').writeFileSync(process.argv[1],'ran')",
        &path_text(&marker),
    ]);
    assert_exit(&output, 127);
    assert!(
        stderr(&output)
            .contains("windows-acl-run: Windows ACL temp root must be outside the workspace")
    );
    assert!(!marker.exists());
}

#[test]
fn confined_children_allow_inherited_grandchild_stdio_but_deny_piped_capture() {
    if !prerequisites_available() {
        return;
    }
    let fixture = RunnerFixture::new();
    let probe = "const {spawnSync}=require('node:child_process');const t=(name,opts)=>{const s=spawnSync(process.execPath,['-e','1'],{encoding:'utf8',...opts});console.log(name+':'+(s.status===0?'OK':'DENIED'))};t('inherit',{stdio:'inherit'});t('ignore',{stdio:'ignore'});t('pipe',{stdio:'pipe'});";
    for mode in ["workspace-write", "read-only"] {
        let mut args = fixture.prefix(mode);
        args.extend([fixture.node.clone(), "-e".into(), probe.into()]);
        let output = run_runner(args);
        assert_exit(&output, 0);
        let stdout = stdout(&output);
        assert!(stdout.contains("inherit:OK"), "mode={mode}\n{stdout}");
        assert!(stdout.contains("ignore:OK"), "mode={mode}\n{stdout}");
        assert!(stdout.contains("pipe:DENIED"), "mode={mode}\n{stdout}");
    }
}

#[test]
fn standing_workspace_grant_is_inert_after_downgrade_and_reused_after_upgrade() {
    if !prerequisites_available() {
        return;
    }
    let fixture = RunnerFixture::new();
    let private_temp = fixture.temp.join("mode-switch-temp");
    std::fs::create_dir(&private_temp).unwrap();
    let workspace_sid = workspace_write_sid(&path_text(&fixture.workspace));
    let private_sid = temp_write_sid(&path_text(&private_temp));
    let mut grant = AclWriteGrant::create(&workspace_sid, grant_binding()).unwrap();
    grant.add(&fixture.workspace, false).unwrap();

    let downgraded_target = fixture.workspace.join("downgraded.txt");
    let downgrade = fixture.pwsh(
        "read-only",
        format!(
            "$ErrorActionPreference='SilentlyContinue';try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'DOWNGRADE-WRITE: OK (LEAK!)'}}catch{{'DOWNGRADE-WRITE: DENIED'}}",
            ps_literal(&downgraded_target)
        ),
    );
    assert_exit(&downgrade, 0);
    assert!(stdout(&downgrade).contains("DOWNGRADE-WRITE: DENIED"));
    assert!(!downgraded_target.exists());

    let upgraded_target = fixture.workspace.join("reupgraded.txt");
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue';try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'REUPGRADE-WRITE: OK'}}catch{{'REUPGRADE-WRITE: DENIED'}}",
        ps_literal(&upgraded_target)
    );
    let output = run_runner([
        "--workspace",
        &path_text(&fixture.workspace),
        "--temp",
        &path_text(&private_temp),
        "--mode",
        "workspace-write",
        "--write-sid",
        &workspace_sid,
        "--temp-write-sid",
        &private_sid,
        "--",
        fixture.pwsh.as_str(),
        "/NoLogo",
        "/NonInteractive",
        "/NoProfile",
        "/Command",
        &script,
    ]);
    let cleanup = grant.dispose();
    assert_exit(&output, 0);
    cleanup.unwrap();
    assert!(stdout(&output).contains("REUPGRADE-WRITE: OK"));
    assert!(upgraded_target.exists());
}

#[test]
fn ambient_public_tree_write_is_denied_in_both_modes() {
    if !prerequisites_available() {
        return;
    }
    let fixture = RunnerFixture::new();
    let public_root = std::env::var_os("PUBLIC")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Users\Public"));
    let Ok(public_probe) = tempfile::Builder::new()
        .prefix("seekdeep-acl-public-")
        .tempdir_in(public_root)
    else {
        eprintln!("skipping public-tree probe because the host Public directory is unavailable");
        return;
    };
    let target = public_probe.path().join("public-escaped.txt");
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue';try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'PUBLIC-WRITE: OK (ESCAPE!)'}}catch{{'PUBLIC-WRITE: DENIED'}}",
        ps_literal(&target)
    );
    for mode in ["read-only", "workspace-write"] {
        let output = fixture.pwsh(mode, script.clone());
        assert_exit(&output, 0);
        assert!(
            stdout(&output).contains("PUBLIC-WRITE: DENIED"),
            "mode={mode}\n{}",
            stdout(&output)
        );
        assert!(!target.exists(), "mode={mode}");
    }
}

#[test]
fn everyone_modify_directory_remains_a_reported_partial_boundary() {
    if !prerequisites_available() {
        return;
    }
    let fixture = RunnerFixture::new();
    let world_writable = fixture._scratch.path().join("world-writable");
    std::fs::create_dir(&world_writable).unwrap();
    let grant = Command::new("icacls.exe")
        .args([
            world_writable.as_os_str(),
            OsStr::new("/grant"),
            OsStr::new("*S-1-1-0:(OI)(CI)(M)"),
        ])
        .output()
        .unwrap();
    assert!(
        grant.status.success(),
        "icacls stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&grant.stdout),
        String::from_utf8_lossy(&grant.stderr)
    );

    for mode in ["read-only", "workspace-write"] {
        let target = world_writable.join(format!("{mode}.txt"));
        let mut args = fixture.prefix(mode);
        args.extend([
            fixture.node.clone(),
            "-e".into(),
            "require('node:fs').writeFileSync(process.argv[1],'written')".into(),
            path_text(&target),
        ]);
        let output = run_runner(args);
        assert_exit(&output, 0);
        assert!(target.exists(), "mode={mode}");
    }
}

#[test]
fn workspace_hard_link_exposes_the_documented_external_alias_boundary() {
    if !prerequisites_available() {
        return;
    }
    let fixture = RunnerFixture::new();
    let workspace = fixture._scratch.path().join("hardlink-workspace");
    let temp = fixture._scratch.path().join("hardlink-temp");
    let external = fixture._scratch.path().join("hardlink-target.txt");
    let alias = workspace.join("hardlink-alias.txt");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&temp).unwrap();
    std::fs::write(&external, "original").unwrap();
    std::fs::hard_link(&external, &alias).unwrap();
    let output = run_runner([
        "--workspace",
        &path_text(&workspace),
        "--temp",
        &path_text(&temp),
        "--mode",
        "workspace-write",
        "--",
        fixture.node.as_str(),
        "-e",
        "require('node:fs').writeFileSync(process.argv[1],'mutated')",
        &path_text(&alias),
    ]);
    assert_exit(&output, 0);
    assert_eq!(std::fs::read_to_string(external).unwrap(), "mutated");
}

#[test]
fn runner_failures_use_the_stable_signature_exit_and_do_not_run_the_command() {
    if !prerequisites_available() {
        return;
    }
    let fixture = RunnerFixture::new();
    let output = run_runner([
        "--workspace",
        &path_text(&fixture.workspace),
        "--temp",
        &path_text(&fixture.temp),
        "--mode",
        "workspace-write",
    ]);
    assert_exit(&output, 127);
    assert!(stderr(&output).contains("windows-acl-run: "));
}

#[test]
fn seam_sid_flags_must_be_paired_and_match_their_owning_paths() {
    if !prerequisites_available() {
        return;
    }
    let fixture = RunnerFixture::new();
    let workspace_sid = workspace_write_sid(&path_text(&fixture.workspace));
    let temp_sid = temp_write_sid(&path_text(&fixture.temp));
    let cases = [
        vec!["--write-sid".to_owned(), workspace_sid.clone()],
        vec![
            "--write-sid".into(),
            "S-1-4-1-2".into(),
            "--temp-write-sid".into(),
            temp_sid.clone(),
        ],
        vec![
            "--write-sid".into(),
            workspace_sid,
            "--temp-write-sid".into(),
            "S-1-4-1-2-1".into(),
        ],
    ];
    for case in cases {
        let mut args = vec![
            "--workspace".into(),
            path_text(&fixture.workspace),
            "--temp".into(),
            path_text(&fixture.temp),
            "--mode".into(),
            "workspace-write".into(),
        ];
        args.extend(case);
        args.extend([
            "--".into(),
            fixture.node.clone(),
            "-e".into(),
            "process.exit(99)".into(),
        ]);
        let output = run_runner(args);
        assert_exit(&output, 127);
        assert!(stderr(&output).contains("windows-acl-run: "));
    }
}
