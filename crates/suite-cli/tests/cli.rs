//! The `stockroom` binary end to end, always with `--root` and the fake release server. `HOME`
//! and the XDG variables point into the test folder too, and are checked to stay empty.
//! The root follows Linux conventions on every host (`--os linux`), as the fixture's apps are
//! Linux tarballs of shell scripts.

// Test code: a failed unwrap is a failed test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use suite_catalog::AssetKind;
use suite_testkit::{Fixture, fake_app};
use tempfile::TempDir;

struct Cli {
    dir: TempDir,
    fx: Fixture,
}

impl Cli {
    fn new() -> Self {
        let mut fx = Fixture::new();
        fx.add_release("demo", "1.0.0", AssetKind::TarGz, fake_app("demo", "1.0.0"));
        fx.publish();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("fake-home")).unwrap();
        Self { dir, fx }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("root")
    }

    fn home(&self) -> PathBuf {
        self.root().join("home")
    }

    fn cmd(&self, args: &[&str], catalog: Option<&str>) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_stockroom"));
        let fake_home = self.dir.path().join("fake-home");
        c.env("HOME", &fake_home)
            .env("XDG_CONFIG_HOME", fake_home.join(".config"))
            .env("XDG_DATA_HOME", fake_home.join(".local/share"))
            .env("XDG_CACHE_HOME", fake_home.join(".cache"))
            .env_remove("STOCKROOM_FAILPOINT")
            .env_remove("STOCKROOM_LOG")
            .arg("--root")
            .arg(self.root())
            .args(["--os", "linux"])
            .arg("--catalog")
            .arg(catalog.map_or_else(|| self.fx.catalog_url(), str::to_owned))
            .args(args);
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args, None).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(out.status.success(), "{args:?}: {}{}", text(&out.stdout), text(&out.stderr));
        text(&out.stdout)
    }

    fn assert_real_home_untouched(&self) {
        let fake_home = self.dir.path().join("fake-home");
        assert_eq!(std::fs::read_dir(&fake_home).unwrap().count(), 0, "something was written to $HOME");
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn run_exe(p: &Path) -> String {
    text(&Command::new(p).output().unwrap().stdout).trim().to_owned()
}

// Without a command `stockroom` opens the app window, which tests never do (it would use the
// real home and display); the window itself is tested headless in suite-ui.
#[test]
fn install_offers_the_compact_window() {
    let out = Command::new(env!("CARGO_BIN_EXE_stockroom")).args(["install", "--help"]).output().unwrap();
    assert!(out.status.success());
    assert!(text(&out.stdout).contains("--ui"), "{}", text(&out.stdout));
}

#[test]
fn full_lifecycle() {
    let mut cli = Cli::new();
    let list = cli.ok(&["list"]);
    assert!(list.contains("demo") && list.contains("available 1.0.0"), "{list}");

    let out = cli.ok(&["install", "demo"]);
    assert!(out.contains("demo 1.0.0 installed"), "{out}");
    let bin = cli.home().join(".local/bin/demo");
    assert_eq!(run_exe(&bin), "demo 1.0.0");
    let status = cli.ok(&["status"]);
    assert!(status.contains("demo       1.0.0 (archive, stable channel"), "{status}");

    cli.fx.add_release("demo", "1.1.0", AssetKind::TarGz, fake_app("demo", "1.1.0"));
    cli.fx.publish();
    assert!(cli.ok(&["list"]).contains("installed 1.0.0 (update 1.1.0)"));
    let out = cli.ok(&["update", "--all"]);
    assert!(out.contains("demo: 1.0.0 -> 1.1.0"), "{out}");
    assert_eq!(run_exe(&bin), "demo 1.1.0");
    assert!(cli.ok(&["update", "demo"]).contains("already up to date"));

    assert!(cli.ok(&["rollback", "demo"]).contains("demo: 1.1.0 -> 1.0.0"));
    assert_eq!(run_exe(&bin), "demo 1.0.0");

    // File associations are a Windows registry matter; here they are refused, plainly.
    let out = cli.run(&["files", "demo", "on"]);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("registered by the app itself"), "{}", text(&out.stderr));

    cli.ok(&["policy", "demo", "ask"]);
    assert!(cli.ok(&["status"]).contains("updates wait for you"));
    let out = cli.ok(&["update", "--background", "--force"]);
    assert!(out.contains("1.1.0 is ready; it waits for you"), "{out}");

    let data = cli.home().join(".config/demo");
    std::fs::create_dir_all(&data).unwrap();
    let out = cli.ok(&["remove", "demo", "--purge"]);
    assert!(out.contains("removed"), "{out}");
    assert!(!data.exists());
    assert!(std::fs::symlink_metadata(&bin).is_err());
    assert!(cli.ok(&["status"]).contains("no apps installed"));
    let out = cli.run(&["remove", "demo"]);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("not installed"));
    cli.assert_real_home_untouched();
}

#[test]
fn catalog_from_a_file() {
    let cli = Cli::new();
    let file = cli.dir.path().join("catalog.json");
    let json = cli.fx.catalog.to_json_pretty().unwrap();
    std::fs::write(&file, &json).unwrap();
    let out = cli.cmd(&["install", "demo"], Some(file.to_str().unwrap())).output().unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    // A list that breaks the rules is refused.
    let json = String::from_utf8(json).unwrap();
    assert!(json.contains(r#""id": "demo""#));
    std::fs::write(&file, json.replace(r#""id": "demo""#, r#""id": "../evil""#)).unwrap();
    let out = cli.cmd(&["list"], Some(file.to_str().unwrap())).output().unwrap();
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("breaks a rule"), "{}", text(&out.stderr));
}

#[test]
fn schedule_and_doctor_stay_inside_the_root() {
    let cli = Cli::new();
    let out = cli.ok(&["schedule", "on"]);
    assert!(out.contains("background updates on (XDG autostart entry)"), "{out}");
    let entry = cli.home().join(".config/autostart/com.amazynk.Stockroom.update.desktop");
    assert!(std::fs::read_to_string(&entry).unwrap().contains("update --background"));
    assert!(cli.ok(&["status"]).contains("background updates: on"));
    let doc = cli.ok(&["doctor"]);
    assert!(doc.contains("ok   catalog"), "{doc}");
    assert!(doc.contains("ok   scheduler"), "{doc}");
    cli.ok(&["schedule", "off"]);
    assert!(!entry.exists());
    // Taking Stockroom off the computer turns the schedule off and says what stays.
    cli.ok(&["install", "demo"]);
    cli.ok(&["schedule", "on"]);
    let out = cli.ok(&["uninstall"]);
    assert!(out.contains("the daily check is off"), "{out}");
    assert!(out.contains("1 app(s) it installed are still here"), "{out}");
    assert!(!entry.exists());
    assert_eq!(run_exe(&cli.home().join(".local/bin/demo")), "demo 1.0.0");
    cli.assert_real_home_untouched();
}

#[test]
fn doctor_fails_without_a_reachable_catalog() {
    let cli = Cli::new();
    cli.fx.server.remove("/catalog.json");
    let out = cli.run(&["doctor"]);
    assert!(!out.status.success());
    assert!(text(&out.stdout).contains("FAIL catalog"));
}

#[test]
fn a_real_crash_mid_update_is_repaired_on_the_next_run() {
    let mut cli = Cli::new();
    cli.ok(&["install", "demo"]);
    cli.fx.add_release("demo", "1.1.0", AssetKind::TarGz, fake_app("demo", "1.1.0"));
    cli.fx.publish();
    for fp in ["after-unpack", "after-commit"] {
        let out = cli.cmd(&["update", "demo"], None).env("STOCKROOM_FAILPOINT", fp).output().unwrap();
        assert!(!out.status.success(), "{fp}: the process should have aborted");
        let status = cli.ok(&["status"]);
        let expected = if fp == "after-unpack" { "demo       1.0.0" } else { "demo       1.1.0" };
        assert!(status.contains(expected), "{fp}: {status}");
        let current = cli.home().join(".local/share/stockroom/apps/demo/current/demo");
        assert_eq!(run_exe(&current), expected.replace("       ", " "));
        let names: Vec<String> = std::fs::read_dir(cli.home().join(".local/share/stockroom/apps/demo"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(!names.iter().any(|n| n.starts_with(".staging")), "{fp}: {names:?}");
    }
    let out = cli.ok(&["update", "demo"]);
    assert!(out.contains("already up to date") || out.contains("-> 1.1.0"), "{out}");
    assert_eq!(run_exe(&cli.home().join(".local/bin/demo")), "demo 1.1.0");
}

#[test]
fn unreleased_apps_are_coming_soon() {
    let mut cli = Cli::new();
    let mut later = suite_testkit::app_entry("later");
    later.status = suite_catalog::AppStatus::Unreleased;
    cli.fx.catalog.apps.push(later);
    cli.fx.publish();
    assert!(cli.ok(&["list"]).contains("coming soon"));
    let out = cli.run(&["install", "later"]);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("coming soon"), "{}", text(&out.stderr));
}
