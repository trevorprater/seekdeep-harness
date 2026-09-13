//! Pinned ripgrep package acquisition and verified closure staging.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use seekdeep_python_release::{
    executable::Target,
    node_runtime::DistributionFetcher,
    ripgrep::{self, RipgrepPackage},
};
use sha2::{Digest as _, Sha512};

#[allow(dead_code)]
#[path = "common/node_fixture.rs"]
mod node_fixture;

#[derive(Default)]
struct FixtureFetcher {
    responses: BTreeMap<String, PathBuf>,
    requests: RefCell<Vec<String>>,
}

impl DistributionFetcher for FixtureFetcher {
    fn download(&self, url: &str, destination: &Path) -> anyhow::Result<()> {
        self.requests.borrow_mut().push(url.to_owned());
        let source = self
            .responses
            .get(url)
            .ok_or_else(|| anyhow::anyhow!("unexpected download {url}"))?;
        fs::copy(source, destination)?;
        Ok(())
    }
}

fn standard_members(target: &Target) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "package/bin/rg",
            node_fixture::native_header(target, false).to_vec(),
        ),
        ("package/LICENSE", b"fixture ripgrep license\n".to_vec()),
        (
            "package/package.json",
            b"{\"name\":\"@vscode/ripgrep-fixture\",\"version\":\"1.18.0\"}\n".to_vec(),
        ),
        ("package/README.md", b"fixture\n".to_vec()),
    ]
}

/// Builds a registry-shaped tarball and the package descriptor whose digest matches it.
fn package_archive(
    root: &Path,
    target: &Target,
    members: &[(&str, Vec<u8>)],
) -> (RipgrepPackage, PathBuf) {
    let staging = root.join("staging");
    if staging.exists() {
        fs::remove_dir_all(&staging).unwrap();
    }
    for (name, content) in members {
        let path = staging.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        #[cfg(unix)]
        if name.ends_with("/rg") {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let archive = root.join(format!("{}.tgz", target.platform_arch()));
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(&staging)
        .arg("package")
        .status()
        .unwrap();
    assert!(status.success());
    let mut package = RipgrepPackage::pinned(target).unwrap();
    package.archive_sha512 = format!("{:x}", Sha512::digest(fs::read(&archive).unwrap()));
    (package, archive)
}

#[test]
fn pinned_packages_identify_the_lockfile_platform_tarballs() {
    for (spec, platform) in [
        ("node24-macos-arm64", "darwin-arm64"),
        ("node24-macos-x64", "darwin-x64"),
        ("node24-linux-arm64", "linux-arm64"),
        ("node24-linux-x64", "linux-x64"),
    ] {
        let target = Target::parse(spec).unwrap();
        let package = RipgrepPackage::pinned(&target).unwrap();
        assert_eq!(package.name(), format!("@vscode/ripgrep-{platform}"));
        assert_eq!(package.version, ripgrep::PACKAGE_VERSION);
        assert_eq!(package.archive(), format!("ripgrep-{platform}-1.18.0.tgz"));
        assert_eq!(
            package.url(),
            format!(
                "https://registry.npmjs.org/@vscode/ripgrep-{platform}/-/ripgrep-{platform}-1.18.0.tgz"
            )
        );
        assert_eq!(package.archive_sha512.len(), 128);
        assert!(
            package
                .archive_sha512
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        );
    }
}

#[test]
fn acquisition_verifies_the_digest_extracts_only_the_binary_and_license_and_reuses_the_cache() {
    let root = tempfile::tempdir().unwrap();
    let target = Target::parse("node24-linux-x64").unwrap();
    let (package, archive) = package_archive(root.path(), &target, &standard_members(&target));
    let mut fetcher = FixtureFetcher::default();
    fetcher.responses.insert(package.url(), archive);
    let cache = root.path().join("cache");
    let acquired =
        ripgrep::acquire_packages(std::slice::from_ref(&package), &cache, &fetcher).unwrap();
    assert_eq!(acquired.len(), 1);
    let directory = acquired[0].directory();
    let mut names = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["LICENSE", "rg"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(directory.join("rg"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
    let cached = cache.join(ripgrep::PACKAGE_VERSION).join(package.archive());
    assert!(cached.is_file());

    let again =
        ripgrep::acquire_packages(std::slice::from_ref(&package), &cache, &fetcher).unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!(
        fetcher.requests.borrow().len(),
        1,
        "a verified cached tarball is reused"
    );

    fs::write(&cached, b"tampered").unwrap();
    let error = ripgrep::acquire_packages(&[package], &cache, &fetcher).unwrap_err();
    assert!(error.to_string().contains("digest mismatch"), "{error:#}");
}

#[test]
fn acquisition_rejects_a_wrong_digest_a_missing_license_and_a_foreign_binary() {
    let root = tempfile::tempdir().unwrap();
    let target = Target::parse("node24-macos-arm64").unwrap();
    let cache = root.path().join("cache");

    let (_fixture, archive) = package_archive(
        &root.path().join("pinned"),
        &target,
        &standard_members(&target),
    );
    let pinned = RipgrepPackage::pinned(&target).unwrap();
    let mut fetcher = FixtureFetcher::default();
    fetcher.responses.insert(pinned.url(), archive);
    let error = ripgrep::acquire_packages(&[pinned], &cache, &fetcher).unwrap_err();
    assert!(error.to_string().contains("digest mismatch"), "{error:#}");

    let members = standard_members(&target)
        .into_iter()
        .filter(|(name, _)| *name != "package/LICENSE")
        .collect::<Vec<_>>();
    let (package, archive) = package_archive(&root.path().join("no-license"), &target, &members);
    let mut fetcher = FixtureFetcher::default();
    fetcher.responses.insert(package.url(), archive);
    let error = ripgrep::acquire_packages(&[package], &cache, &fetcher).unwrap_err();
    assert!(
        error.to_string().contains("exactly one package/LICENSE"),
        "{error:#}"
    );

    let mut members = standard_members(&target);
    members[0].1 =
        node_fixture::native_header(&Target::parse("node24-linux-x64").unwrap(), false).to_vec();
    let (package, archive) = package_archive(&root.path().join("foreign"), &target, &members);
    let mut fetcher = FixtureFetcher::default();
    fetcher.responses.insert(package.url(), archive);
    assert!(ripgrep::acquire_packages(&[package], &cache, &fetcher).is_err());
}

#[test]
fn staging_records_the_acquired_package_and_verified_copies_reject_tampering() {
    let root = tempfile::tempdir().unwrap();
    let target = Target::parse("node24-linux-arm64").unwrap();
    let (package, archive) = package_archive(root.path(), &target, &standard_members(&target));
    let mut fetcher = FixtureFetcher::default();
    fetcher.responses.insert(package.url(), archive);
    let acquired = ripgrep::acquire_packages(
        std::slice::from_ref(&package),
        &root.path().join("cache"),
        &fetcher,
    )
    .unwrap();
    let closure = root.path().join("closure");
    ripgrep::stage_package(&acquired[0], &closure).unwrap();
    let provenance = ripgrep::verify_directory_against(&closure, &package).unwrap();
    assert_eq!(provenance.package, package.name());
    assert_eq!(provenance.archive_sha512, package.archive_sha512);
    assert_eq!(provenance.url, package.url());
    assert_eq!(
        (provenance.executable.as_str(), provenance.license.as_str()),
        ("rg", "LICENSE")
    );
    // The pinned verification refuses a closure built from a package with another digest.
    let error = ripgrep::verify_directory(&closure, &target).unwrap_err();
    assert!(
        error.to_string().contains("does not identify the pinned"),
        "{error:#}"
    );
    let error = ripgrep::stage_package(&acquired[0], &closure).unwrap_err();
    assert!(error.to_string().contains("already exists"), "{error:#}");

    // A closure with pinned provenance verifies, copies, and is found beside an executable.
    let bin = root.path().join("bin");
    let pinned_closure = bin.join(ripgrep::DIRECTORY).join(target.platform_arch());
    node_fixture::make_ripgrep_assets(&pinned_closure, &target);
    let executable = bin.join("seekdeep");
    fs::write(&executable, b"fixture").unwrap();
    assert_eq!(
        ripgrep::adjacent_directory(&executable, &target).unwrap(),
        pinned_closure
    );
    let copy = root.path().join("copy");
    ripgrep::copy_directory(&pinned_closure, &copy, &target).unwrap();
    ripgrep::verify_directory(&copy, &target).unwrap();
    assert!(ripgrep::verify_directory(&copy, &Target::parse("node24-linux-x64").unwrap()).is_err());
    fs::write(copy.join("rg"), b"replaced").unwrap();
    let error = ripgrep::verify_directory(&copy, &target).unwrap_err();
    assert!(
        error.to_string().contains("missing or modified"),
        "{error:#}"
    );
    node_fixture::make_ripgrep_assets(&copy, &target);
    fs::write(copy.join("extra"), b"x").unwrap();
    let error = ripgrep::verify_directory(&copy, &target).unwrap_err();
    assert!(error.to_string().contains("unlisted"), "{error:#}");

    let flat = root.path().join("flat");
    fs::create_dir_all(&flat).unwrap();
    node_fixture::make_ripgrep_assets(&flat.join(ripgrep::DIRECTORY), &target);
    fs::write(flat.join("seekdeep"), b"fixture").unwrap();
    assert_eq!(
        ripgrep::adjacent_directory(&flat.join("seekdeep"), &target).unwrap(),
        flat.join(ripgrep::DIRECTORY)
    );
    assert!(ripgrep::adjacent_directory(&root.path().join("nowhere/seekdeep"), &target).is_err());
}
