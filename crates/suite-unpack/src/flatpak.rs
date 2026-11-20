//! Flatpak bundles are installed by `flatpak` itself (per user); Stockroom only tracks the
//! version. Commands go through the [`Runner`], so tests use a fake.

use std::path::Path;

use suite_platform::runner::{Runner, args};

use crate::UnpackError;

fn run(runner: &dyn Runner, a: &[&std::ffi::OsStr]) -> Result<String, UnpackError> {
    let out = runner.run("flatpak", &args(a)).map_err(|e| UnpackError::Command(format!("flatpak: {e}")))?;
    if out.success {
        Ok(out.stdout)
    } else {
        Err(UnpackError::Command(format!("flatpak failed: {}", out.stderr.trim())))
    }
}

/// `flatpak install --user --bundle <file>`, non-interactively.
pub fn install_bundle(runner: &dyn Runner, bundle: &Path) -> Result<(), UnpackError> {
    let a: [&std::ffi::OsStr; 6] = [
        "install".as_ref(),
        "--user".as_ref(),
        "--noninteractive".as_ref(),
        "-y".as_ref(),
        "--bundle".as_ref(),
        bundle.as_os_str(),
    ];
    run(runner, &a).map(drop)
}

/// The version of a per-user Flatpak install, from `flatpak info --user`.
pub fn installed_version(runner: &dyn Runner, app_id: &str) -> Option<String> {
    let out = run(runner, &["info".as_ref(), "--user".as_ref(), app_id.as_ref()]).ok()?;
    parse_info_version(&out)
}

/// The `Version:` field of `flatpak info` output.
pub fn parse_info_version(info: &str) -> Option<String> {
    info.lines()
        .find_map(|l| l.trim().strip_prefix("Version:").map(|v| v.trim().to_owned()))
        .filter(|v| !v.is_empty())
}

/// `flatpak uninstall --user <id>`.
pub fn uninstall(runner: &dyn Runner, app_id: &str) -> Result<(), UnpackError> {
    run(
        runner,
        &[
            "uninstall".as_ref(),
            "--user".as_ref(),
            "--noninteractive".as_ref(),
            "-y".as_ref(),
            app_id.as_ref(),
        ],
    )
    .map(drop)
}

#[cfg(test)]
mod tests {
    use suite_platform::{FakeRunner, Output};

    use super::*;

    #[test]
    fn install_and_query_through_the_runner() {
        let r = FakeRunner::new();
        r.on(&["flatpak", "install"], Output::ok(""));
        r.on(
            &["flatpak", "info", "--user", "app.x.X"],
            Output::ok("\nX - thing\n\n          ID: app.x.X\n     Version: 1.2.3\n"),
        );
        install_bundle(&r, Path::new("/tmp/x.flatpak")).unwrap();
        assert!(r.ran(&[
            "flatpak",
            "install",
            "--user",
            "--noninteractive",
            "-y",
            "--bundle",
            "/tmp/x.flatpak"
        ]));
        assert_eq!(installed_version(&r, "app.x.X").as_deref(), Some("1.2.3"));
        assert_eq!(installed_version(&r, "app.y.Y"), None);
        r.on(&["flatpak", "uninstall"], Output::failed(1, "not installed"));
        assert!(uninstall(&r, "app.x.X").is_err());
    }
}
