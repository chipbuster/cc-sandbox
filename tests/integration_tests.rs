/// Integration tests for cc-sandbox.
///
/// Most tests require:
///   - Root access (to mount a loopback filesystem)
///   - mkfs.btrfs on PATH (from btrfs-progs)
///
/// Tests that also need Docker are marked with `// REQUIRES: docker`.
///
/// When requirements are not met, tests return early (they pass vacuously).
/// This is intentional — the spec says "CI needs Docker and a CoW filesystem
/// available; this is a hard requirement", so the CI environment must supply
/// them. The skip path exists for local dev without those resources.
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BINARY: &str = env!("CARGO_BIN_EXE_cc-sandbox");

// ── Prerequisites ──────────────────────────────────────────────────────────

fn is_root() -> bool {
    unsafe { libc::getuid() == 0 }
}

fn has_tool(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn can_run_integration_tests() -> bool {
    if !is_root() {
        eprintln!("SKIP: must run as root (need loopback mount)");
        return false;
    }
    if !has_tool("mkfs.btrfs") {
        eprintln!("SKIP: mkfs.btrfs not found (install btrfs-progs)");
        return false;
    }
    true
}

fn has_docker() -> bool {
    has_tool("docker") && Command::new("docker").arg("info").output().map(|o| o.status.success()).unwrap_or(false)
}

// ── TestEnv ────────────────────────────────────────────────────────────────

/// Manages a btrfs loopback mount, a test project, a shadow root, fake tool
/// scripts, and a per-test config directory.  Automatically unmounts and
/// removes everything on drop.
struct TestEnv {
    /// The btrfs loopback mount point.
    pub mount_point: PathBuf,
    /// The source project directory (on the btrfs mount).
    pub source: PathBuf,
    /// Where shadows are stored (on the btrfs mount).
    pub shadow_root: PathBuf,
    /// Directory containing fake `devcontainer` and `docker` scripts.
    pub fake_tools: PathBuf,
    /// Value to use for XDG_CONFIG_HOME when spawning the binary.
    pub config_home: PathBuf,
    /// Temp directory holding everything; dropped last (after umount).
    _base: tempfile::TempDir,
}

impl TestEnv {
    /// Set up the full test environment.  Returns `None` if prerequisites
    /// are not met (see `can_run_integration_tests`).
    fn setup() -> Option<Self> {
        if !can_run_integration_tests() {
            return None;
        }

        let base = tempfile::tempdir().expect("Failed to create temp dir");

        // ── btrfs loopback ────────────────────────────────────────────────
        let img = base.path().join("btrfs.img");
        let mnt = base.path().join("mnt");

        if !Command::new("dd")
            .args([
                "if=/dev/zero",
                &format!("of={}", img.display()),
                "bs=1M",
                "count=256",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?
            .success()
        {
            eprintln!("SKIP: dd failed");
            return None;
        }

        if !Command::new("mkfs.btrfs")
            .arg(&img)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?
            .success()
        {
            eprintln!("SKIP: mkfs.btrfs failed");
            return None;
        }

        fs::create_dir_all(&mnt).ok()?;

        if !Command::new("mount")
            .args([
                "-o",
                "loop",
                &img.display().to_string(),
                &mnt.display().to_string(),
            ])
            .status()
            .ok()?
            .success()
        {
            eprintln!("SKIP: mount failed");
            return None;
        }

        // ── test project ──────────────────────────────────────────────────
        let source = mnt.join("testproject");
        fs::create_dir_all(source.join(".devcontainer")).ok()?;
        fs::write(
            source.join(".devcontainer/devcontainer.json"),
            r#"{"image": "debian:bookworm-slim"}"#,
        )
        .ok()?;
        fs::write(source.join("hello.txt"), "hello world\n").ok()?;
        fs::write(source.join("data.bin"), b"\x00\x01\x02\x03").ok()?;

        // ── shadow root ───────────────────────────────────────────────────
        let shadow_root = mnt.join(".cc-sandbox");
        fs::create_dir_all(&shadow_root).ok()?;

        // ── fake tools ────────────────────────────────────────────────────
        let fake_tools = base.path().join("fake_tools");
        fs::create_dir_all(&fake_tools).ok()?;
        Self::write_fake_tool(&fake_tools.join("devcontainer"), "#!/bin/sh\nexit 0\n");
        // Fake docker: `docker ps` returns empty (no containers), rm is no-op.
        Self::write_fake_tool(
            &fake_tools.join("docker"),
            "#!/bin/sh\necho ''\nexit 0\n",
        );

        // ── config ────────────────────────────────────────────────────────
        let device_id = get_device_id(&mnt);
        let config_home = base.path().join("config");
        let config_dir = config_home.join("cc-sandbox");
        fs::create_dir_all(&config_dir).ok()?;
        fs::write(
            config_dir.join("config.toml"),
            format!(
                "[[filesystem]]\nmount_point = \"{}\"\ndevice_id = {}\nshadow_root = \"{}\"\n\n\
                 [agent]\ncommand = [\"true\"]\n\n[shell]\ncommand = [\"true\"]\n",
                mnt.display(),
                device_id,
                shadow_root.display()
            ),
        )
        .ok()?;

        Some(TestEnv {
            mount_point: mnt,
            source,
            shadow_root,
            fake_tools,
            config_home,
            _base: base,
        })
    }

    fn write_fake_tool(path: &Path, content: &str) {
        fs::write(path, content).expect("write fake tool");
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .expect("chmod fake tool");
    }

    /// Return a Command for the binary, with the test config and fake tools.
    fn cmd(&self) -> Command {
        self.cmd_with_path(true)
    }

    /// Return a Command with real tools (for tests that need actual Docker).
    fn real_cmd(&self) -> Command {
        self.cmd_with_path(false)
    }

    fn cmd_with_path(&self, fake_tools: bool) -> Command {
        let mut cmd = Command::new(BINARY);
        cmd.env("XDG_CONFIG_HOME", &self.config_home);
        if fake_tools {
            let old_path = std::env::var("PATH").unwrap_or_default();
            cmd.env(
                "PATH",
                format!("{}:{old_path}", self.fake_tools.display()),
            );
        }
        cmd
    }

    /// Shadow path for a given name suffix (created with `--name <suffix>`).
    fn shadow_path(&self, suffix: &str) -> PathBuf {
        self.shadow_root
            .join(format!("testproject-{suffix}"))
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        // Must unmount before _base (TempDir) is dropped, otherwise
        // the image file is deleted while still mounted.
        let _ = Command::new("umount").arg(&self.mount_point).status();
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn get_device_id(path: &Path) -> u64 {
    use nix::sys::stat::stat;
    stat(path).expect("stat failed").st_dev
}

/// Hash the contents of all files under `dir` (excluding .cc-sandbox-meta.json).
/// Used to assert source directory is unchanged after an operation.
fn hash_dir(dir: &Path) -> String {
    let find_out = Command::new("find")
        .args([
            dir.as_os_str(),
            std::ffi::OsStr::new("-type"),
            std::ffi::OsStr::new("f"),
            std::ffi::OsStr::new("!"),
            std::ffi::OsStr::new("-name"),
            std::ffi::OsStr::new(".cc-sandbox-meta.json"),
        ])
        .output()
        .expect("find failed");

    let mut files: Vec<String> = String::from_utf8_lossy(&find_out.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect();
    files.sort();

    let mut hasher_input: Vec<u8> = Vec::new();
    for f in &files {
        hasher_input.extend_from_slice(f.as_bytes());
        hasher_input.push(b'\n');
        if let Ok(content) = fs::read(f) {
            // Include length as a header so empty files don't silently merge.
            hasher_input.extend_from_slice(format!("{}\n", content.len()).as_bytes());
            hasher_input.extend_from_slice(&content);
        }
    }

    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("sha256sum failed");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&hasher_input)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// After `start`, the shadow must be a CoW reflink copy.  Modifying a file
/// in the shadow must NOT affect the source.
#[test]
fn test_shadow_reflink_cow() {
    let Some(env) = TestEnv::setup() else { return };

    let status = env
        .cmd()
        .args(["start", &env.source.display().to_string(), "--name", "cow"])
        .status()
        .expect("spawn failed");
    assert!(status.success(), "start failed: {status}");

    let shadow = env.shadow_path("cow");
    assert!(shadow.exists(), "shadow directory not created");

    // Modify the file inside the shadow.
    let shadow_file = shadow.join("hello.txt");
    let source_file = env.source.join("hello.txt");
    fs::write(&shadow_file, "modified in shadow\n").unwrap();

    // Source must be unchanged.
    let source_content = fs::read_to_string(&source_file).unwrap();
    assert_eq!(
        source_content, "hello world\n",
        "SAFETY: modifying the shadow must not affect the source (CoW broken)"
    );
}

/// `list` must show the shadow's relative name, source path, and status.
#[test]
fn test_list_shows_shadow() {
    let Some(env) = TestEnv::setup() else { return };

    env.cmd()
        .args(["start", &env.source.display().to_string(), "--name", "listed"])
        .status()
        .expect("spawn failed");

    let out = env
        .cmd()
        .arg("list")
        .output()
        .expect("spawn failed");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("testproject-listed"),
        "list output missing shadow name:\n{stdout}"
    );
    assert!(
        stdout.contains(&env.source.display().to_string()),
        "list output missing source path:\n{stdout}"
    );
}

/// `path` must print the shadow directory path and leave the source unchanged.
#[test]
fn test_path_returns_correct_path_and_leaves_source_unchanged() {
    let Some(env) = TestEnv::setup() else { return };

    env.cmd()
        .args(["start", &env.source.display().to_string(), "--name", "pathtest"])
        .status()
        .expect("spawn failed");

    let source_hash_before = hash_dir(&env.source);

    let out = env
        .cmd()
        .args(["path", "testproject-pathtest"])
        .output()
        .expect("spawn failed");
    assert!(out.status.success(), "path command failed");

    let printed = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let expected = env.shadow_path("pathtest").display().to_string();
    assert_eq!(
        printed, expected,
        "path printed wrong value"
    );

    // SAFETY: path must not modify the source directory.
    let source_hash_after = hash_dir(&env.source);
    assert_eq!(
        source_hash_before, source_hash_after,
        "SAFETY: path command modified the source directory"
    );
}

/// `reject --yes` must discard the shadow and leave the source entirely
/// unchanged — including after changes were made in the shadow.
#[test]
fn test_reject_never_modifies_source() {
    let Some(env) = TestEnv::setup() else { return };

    env.cmd()
        .args(["start", &env.source.display().to_string(), "--name", "rejected"])
        .status()
        .expect("spawn failed");

    // Hash source before any changes.
    let source_hash_before = hash_dir(&env.source);

    let shadow = env.shadow_path("rejected");

    // Make several changes inside the shadow.
    fs::write(shadow.join("hello.txt"), "changed in shadow\n").unwrap();
    fs::write(shadow.join("new_file.txt"), "brand new\n").unwrap();
    fs::remove_file(shadow.join("data.bin")).unwrap();

    let status = env
        .cmd()
        .args(["reject", "--yes", "testproject-rejected"])
        .status()
        .expect("spawn failed");
    assert!(status.success(), "reject failed: {status}");

    // Shadow must be gone.
    assert!(
        !shadow.exists(),
        "shadow still exists after reject"
    );

    // SAFETY: source must be byte-for-byte identical to before.
    let source_hash_after = hash_dir(&env.source);
    assert_eq!(
        source_hash_before, source_hash_after,
        "SAFETY: reject modified the source directory"
    );
}

/// `accept --yes` must rsync changes from shadow to source, must NOT copy the
/// meta file to source, and must delete the shadow on success.
#[test]
fn test_accept_merges_and_deletes_shadow() {
    let Some(env) = TestEnv::setup() else { return };

    env.cmd()
        .args(["start", &env.source.display().to_string(), "--name", "accepted"])
        .status()
        .expect("spawn failed");

    let shadow = env.shadow_path("accepted");

    // Make changes in the shadow.
    fs::write(shadow.join("hello.txt"), "updated content\n").unwrap();
    fs::write(shadow.join("brand_new.txt"), "new file\n").unwrap();
    fs::remove_file(shadow.join("data.bin")).unwrap();

    let status = env
        .cmd()
        .args(["accept", "--yes", "testproject-accepted"])
        .status()
        .expect("spawn failed");
    assert!(status.success(), "accept failed: {status}");

    // Shadow must be gone.
    assert!(
        !shadow.exists(),
        "shadow still exists after accept"
    );

    // Source must reflect the changes.
    assert_eq!(
        fs::read_to_string(env.source.join("hello.txt")).unwrap(),
        "updated content\n",
        "accept did not sync modified file"
    );
    assert!(
        env.source.join("brand_new.txt").exists(),
        "accept did not sync new file"
    );
    assert!(
        !env.source.join("data.bin").exists(),
        "accept did not propagate deletion (--delete not working)"
    );

    // SAFETY: .cc-sandbox-meta.json must NOT appear in the source.
    assert!(
        !env.source.join(".cc-sandbox-meta.json").exists(),
        "SAFETY: accept synced the meta file into the source directory"
    );
}

/// If rsync fails during `accept`, the shadow must be preserved so the user
/// can inspect and retry.
#[test]
fn test_accept_preserves_shadow_when_rsync_fails() {
    let Some(env) = TestEnv::setup() else { return };

    env.cmd()
        .args(["start", &env.source.display().to_string(), "--name", "failaccept"])
        .status()
        .expect("spawn failed");

    let shadow = env.shadow_path("failaccept");
    fs::write(shadow.join("hello.txt"), "changed\n").unwrap();

    // Make source read-only so rsync cannot write to it.
    fs::set_permissions(&env.source, fs::Permissions::from_mode(0o555))
        .unwrap();

    let status = env
        .cmd()
        .args(["accept", "--yes", "testproject-failaccept"])
        .status()
        .expect("spawn failed");

    // Restore permissions before any assertions so cleanup doesn't fail.
    fs::set_permissions(&env.source, fs::Permissions::from_mode(0o755))
        .unwrap();

    assert!(
        !status.success(),
        "accept should have failed when source is read-only"
    );

    // SAFETY: shadow must be preserved when rsync fails.
    assert!(
        shadow.exists(),
        "SAFETY: accept deleted the shadow even though rsync failed"
    );
}

/// When two shadows have the same leaf name, resolving by that leaf alone
/// must be refused with an error listing both matches.
#[test]
fn test_name_resolution_refuses_ambiguity() {
    let Some(env) = TestEnv::setup() else { return };

    // Create a second source project alongside the first.
    let source2 = env.mount_point.join("otherproject");
    fs::create_dir_all(source2.join(".devcontainer")).unwrap();
    fs::write(
        source2.join(".devcontainer/devcontainer.json"),
        r#"{"image": "debian:bookworm-slim"}"#,
    )
    .unwrap();
    fs::write(source2.join("file.txt"), "other\n").unwrap();

    // Create shadows with the same --name suffix for both projects.
    env.cmd()
        .args(["start", &env.source.display().to_string(), "--name", "dup"])
        .status()
        .expect("spawn failed");

    env.cmd()
        .args(["start", &source2.display().to_string(), "--name", "dup"])
        .status()
        .expect("spawn failed");

    // Both shadows exist with leaves `testproject-dup` and `otherproject-dup`.
    // The leaf names are different, so a search for the full leaf resolves
    // unambiguously.  Test that resolving by an unambiguous full leaf works:
    let status = env
        .cmd()
        .args(["path", "testproject-dup"])
        .status()
        .expect("spawn failed");
    assert!(status.success(), "full-leaf resolve should succeed");

    // Now create a scenario with two shadows having the SAME leaf name by
    // placing two source dirs in different subdirectories.
    let subdir1 = env.mount_point.join("sub1").join("myproject");
    let subdir2 = env.mount_point.join("sub2").join("myproject");
    for dir in [&subdir1, &subdir2] {
        fs::create_dir_all(dir.join(".devcontainer")).unwrap();
        fs::write(
            dir.join(".devcontainer/devcontainer.json"),
            r#"{"image": "debian:bookworm-slim"}"#,
        )
        .unwrap();
        fs::write(dir.join("x.txt"), "x\n").unwrap();
    }

    env.cmd()
        .args(["start", &subdir1.display().to_string(), "--name", "same"])
        .status()
        .expect("spawn failed");
    env.cmd()
        .args(["start", &subdir2.display().to_string(), "--name", "same"])
        .status()
        .expect("spawn failed");

    // Both shadows have the leaf `myproject-same`. Resolving by that leaf
    // must fail.
    let out = env
        .cmd()
        .args(["path", "myproject-same"])
        .output()
        .expect("spawn failed");
    assert!(
        !out.status.success(),
        "ambiguous name resolution should fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Ambiguous"),
        "error should mention 'Ambiguous':\n{stderr}"
    );
}

/// `accept` must clean up empty intermediate directories under the shadow
/// root after removing the shadow.
#[test]
fn test_accept_cleans_empty_parent_dirs() {
    let Some(env) = TestEnv::setup() else { return };

    // Source is in a subdirectory so there's an intermediate dir to clean up.
    let nested_source = env.mount_point.join("nested").join("deep").join("project");
    fs::create_dir_all(nested_source.join(".devcontainer")).unwrap();
    fs::write(
        nested_source.join(".devcontainer/devcontainer.json"),
        r#"{"image": "debian:bookworm-slim"}"#,
    )
    .unwrap();
    fs::write(nested_source.join("file.txt"), "content\n").unwrap();

    env.cmd()
        .args(["start", &nested_source.display().to_string(), "--name", "deep"])
        .status()
        .expect("spawn failed");

    // Shadow should be at shadow_root/nested/deep/project-deep
    let shadow = env
        .shadow_root
        .join("nested")
        .join("deep")
        .join("project-deep");
    assert!(shadow.exists(), "shadow not created");

    env.cmd()
        .args(["accept", "--yes", "project-deep"])
        .status()
        .expect("spawn failed");

    // The shadow itself and all empty parents up to shadow_root must be gone.
    assert!(!shadow.exists(), "shadow not removed after accept");
    assert!(
        !env.shadow_root.join("nested").exists(),
        "empty intermediate dirs should be cleaned up"
    );
}

/// Full start-to-accept cycle with real Docker.
/// REQUIRES: docker
#[test]
fn test_full_cycle_with_docker() {
    let Some(env) = TestEnv::setup() else { return };
    if !has_docker() {
        eprintln!("SKIP: Docker not available");
        return;
    }

    // Override config to run `true` as the agent (fast, no claude needed).
    let config_dir = env.config_home.join("cc-sandbox");
    let device_id = get_device_id(&env.mount_point);
    fs::write(
        config_dir.join("config.toml"),
        format!(
            "[[filesystem]]\nmount_point = \"{}\"\ndevice_id = {}\nshadow_root = \"{}\"\n\n\
             [agent]\ncommand = [\"true\"]\n\n[shell]\ncommand = [\"true\"]\n",
            env.mount_point.display(),
            device_id,
            env.shadow_root.display()
        ),
    )
    .unwrap();

    let status = env
        .real_cmd()
        .args(["start", &env.source.display().to_string(), "--name", "docker"])
        .status()
        .expect("spawn failed");
    assert!(status.success(), "start with real docker failed: {status}");

    let shadow = env.shadow_path("docker");
    assert!(shadow.exists(), "shadow not created");

    // Make a change in the shadow, then accept.
    fs::write(shadow.join("hello.txt"), "from docker test\n").unwrap();

    let status = env
        .real_cmd()
        .args(["accept", "--yes", "testproject-docker"])
        .status()
        .expect("spawn failed");
    assert!(status.success(), "accept failed: {status}");

    assert_eq!(
        fs::read_to_string(env.source.join("hello.txt")).unwrap(),
        "from docker test\n"
    );
    assert!(!shadow.exists());
}
