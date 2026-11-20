//! Per-OS integration: what makes an installed app feel installed.
//!
//! - Linux: `~/.local/bin/<app>` symlink, a `.desktop` file, icons and MIME packages in
//!   `~/.local/share`, then `update-desktop-database` / `update-mime-database` if present.
//! - Windows: the launcher shim beside `current.txt`, a Start-menu `.lnk` pointing at it, an
//!   HKCU uninstall entry and, only when asked, HKCU file associations ("Open with").
//! - macOS: the `.app` bundle copied to `~/Applications` (or `/Applications`).
//!
//! Every path created is returned and recorded, so removal deletes exactly those.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

use suite_catalog::{Layout, Os};
use suite_identity::{APP_ID, PUBLISHER, SLUG};
use suite_platform::{
    Env, Hive,
    registry::{UNINSTALL_KEY, Value, delete_if_empty},
    runner::args,
    scheduler::desktop_exec_quote,
};

use crate::{InstallError, state::Created};

/// Marker key in desktop files Stockroom wrote.
pub const DESKTOP_MARKER: &str = "X-Stockroom-App";

/// What integration needs to know about one installed version.
#[derive(Debug)]
pub(crate) struct Target<'a> {
    pub env: &'a Env,
    pub app_id: &'a str,
    pub name: &'a str,
    pub pitch: &'a str,
    pub desktop_id: String,
    /// `apps/<id>`.
    pub app_dir: PathBuf,
    /// `apps/<id>/<version>`.
    pub version_dir: PathBuf,
    /// Executable (or `.app` bundle) relative to the version folder.
    pub exe_rel: Option<String>,
    pub layout: &'a Layout,
    /// Name of the `~/.local/bin` link.
    pub bin_name: String,
    pub version: &'a semver::Version,
    pub suite_exe: &'a Path,
    /// The launcher shim to copy beside `current.txt` (Windows); without one, the shortcut
    /// targets the version folder and is rewritten on every switch.
    pub shim: Option<&'a Path>,
    /// What the previous integration created: a file in the way that isn't listed here is
    /// someone else's and stays.
    pub prior: &'a [Created],
    /// Where the `.app` copy goes (macOS).
    pub mac_dest: PathBuf,
    /// Register file associations for these extensions (Windows, on request only).
    pub file_types: &'a [String],
}

impl Target<'_> {
    /// The executable through the `current` pointer, stable across updates (Unix).
    fn stable_exe(&self) -> Option<PathBuf> {
        self.exe_rel.as_ref().map(|e| join_rel(&self.app_dir.join(crate::pointer::CURRENT), e))
    }

    fn versioned_exe(&self) -> Option<PathBuf> {
        self.exe_rel.as_ref().map(|e| join_rel(&self.version_dir, e))
    }
}

pub(crate) fn join_rel(base: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(base.to_owned(), |p, c| p.join(c))
}

/// Creates or refreshes the integration for the current version.
pub(crate) fn integrate(t: &Target<'_>, mac_bundle: bool) -> Result<Vec<Created>, InstallError> {
    match t.env.os {
        Os::Linux => linux(t),
        Os::Windows => windows(t),
        Os::Macos if mac_bundle => macos(t),
        Os::Macos => Ok(Vec::new()),
    }
}

fn linux(t: &Target<'_>) -> Result<Vec<Created>, InstallError> {
    let paths = &t.env.paths;
    let mut created = Vec::new();
    let Some(exe) = t.stable_exe() else { return Ok(created) };

    // ~/.local/bin/<app> -> apps/<id>/current/<exe>
    let link = paths.bin.join(&t.bin_name);
    if place_symlink(&link, &exe, &paths.apps)? {
        created.push(Created::Symlink { path: link });
    }

    // Desktop file.
    let apps_dir = paths.xdg_data.join("applications");
    let desktop_path = apps_dir.join(format!("{}.desktop", t.desktop_id));
    let source = t.layout.desktop.as_ref().map(|d| join_rel(&t.version_dir, d)).filter(|p| p.is_file());
    let text = match source {
        Some(p) => {
            rewrite_desktop(&std::fs::read_to_string(&p).map_err(|e| InstallError::io(&p, e))?, &exe, t)
        }
        None => generated_desktop(&exe, t),
    };
    if ours_or_absent(&desktop_path) {
        crate::state::write_atomic(&desktop_path, text.as_bytes())?;
        created.push(Created::File { path: desktop_path });
    } else {
        tracing::warn!(path = %desktop_path.display(), "a desktop file not made by Stockroom is in the way; leaving it");
    }

    // Icons: the app's share/icons tree, and the layout icon if it lives elsewhere.
    let icons_src = t.version_dir.join("share").join("icons");
    let icons_dst = paths.xdg_data.join("icons");
    for rel in files_under(&icons_src) {
        let dst = icons_dst.join(&rel);
        copy_file(&icons_src.join(&rel), &dst)?;
        created.push(Created::File { path: dst });
    }
    if let Some(icon) = &t.layout.icon
        && !icon.starts_with("share/icons/")
    {
        let src = join_rel(&t.version_dir, icon);
        if src.is_file() {
            let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("png").to_owned();
            let size = if ext == "svg" { "scalable" } else { "256x256" };
            let dst =
                icons_dst.join("hicolor").join(size).join("apps").join(format!("{}.{ext}", t.desktop_id));
            copy_file(&src, &dst)?;
            created.push(Created::File { path: dst });
        }
    }

    // Shared MIME info packages.
    let mime_src = t.version_dir.join("share").join("mime").join("packages");
    let mime_dir = paths.xdg_data.join("mime");
    let mut mime_changed = false;
    for rel in files_under(&mime_src) {
        if rel.extension().is_some_and(|e| e == "xml") && rel.components().count() == 1 {
            let dst = mime_dir.join("packages").join(&rel);
            copy_file(&mime_src.join(&rel), &dst)?;
            created.push(Created::File { path: dst });
            mime_changed = true;
        }
    }

    refresh_linux_caches(t.env, mime_changed);
    Ok(created)
}

/// Runs the desktop and MIME cache tools when they exist; failures only cost freshness.
pub(crate) fn refresh_linux_caches(env: &Env, mime: bool) {
    let paths = &env.paths;
    if paths.which("update-desktop-database").is_some() {
        let _ = env.runner.run("update-desktop-database", &args([paths.xdg_data.join("applications")]));
    }
    if mime && paths.which("update-mime-database").is_some() {
        let _ = env.runner.run("update-mime-database", &args([paths.xdg_data.join("mime")]));
    }
}

/// Points `exe` at `link` unless something not ours is there. Returns whether it's ours now.
fn place_symlink(link: &Path, exe: &Path, apps_root: &Path) -> Result<bool, InstallError> {
    match std::fs::symlink_metadata(link) {
        Ok(m) if m.file_type().is_symlink() => {
            let target = std::fs::read_link(link).map_err(|e| InstallError::io(link, e))?;
            if !target.starts_with(apps_root) {
                tracing::warn!(path = %link.display(), "a command not made by Stockroom is in the way; leaving it");
                return Ok(false);
            }
        }
        Ok(_) => {
            tracing::warn!(path = %link.display(), "a file not made by Stockroom is in the way; leaving it");
            return Ok(false);
        }
        Err(_) => {}
    }
    let dir = link.parent().ok_or_else(|| InstallError::State("bin link without folder".into()))?;
    std::fs::create_dir_all(dir).map_err(|e| InstallError::io(dir, e))?;
    crate::pointer::symlink_atomic(exe, link)?;
    Ok(true)
}

pub(crate) fn ours_or_absent(desktop: &Path) -> bool {
    match std::fs::read_to_string(desktop) {
        Ok(text) => text.lines().any(|l| l.starts_with(DESKTOP_MARKER)),
        Err(_) => !desktop.exists(),
    }
}

/// Rewrites `Exec=`/`TryExec=` to the installed executable and marks the file as ours.
pub fn rewrite_desktop_text(text: &str, exe: &Path, exe_name: &str, app_id: &str) -> String {
    let quoted = desktop_exec_quote(&exe.display().to_string());
    let mut out = String::with_capacity(text.len() + 128);
    let mut in_main = false;
    let mut marked = false;
    for line in text.lines() {
        if line.starts_with('[') {
            if in_main && !marked {
                let _ = writeln!(out, "{DESKTOP_MARKER}={app_id}");
                marked = true;
            }
            in_main = line.trim() == "[Desktop Entry]";
        }
        if let Some(rest) = line.strip_prefix("Exec=") {
            let (prog, args) = split_first_token(rest);
            let base = prog.rsplit('/').next().unwrap_or(prog);
            if base == exe_name {
                let _ = writeln!(out, "Exec={quoted}{args}");
                continue;
            }
        } else if line.starts_with("TryExec=") {
            let _ = writeln!(out, "TryExec={}", exe.display());
            continue;
        } else if line.starts_with(DESKTOP_MARKER) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if in_main && !marked {
        let _ = writeln!(out, "{DESKTOP_MARKER}={app_id}");
    }
    out
}

/// Splits `prog args` where `prog` may be double-quoted; returns `prog` unquoted and the
/// rest including its leading space.
fn split_first_token(s: &str) -> (&str, &str) {
    if let Some(rest) = s.strip_prefix('"')
        && let Some(end) = rest.find('"')
    {
        return (&rest[..end], &rest[end + 1..]);
    }
    match s.find(' ') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

fn rewrite_desktop(text: &str, exe: &Path, t: &Target<'_>) -> String {
    let exe_name = exe.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    rewrite_desktop_text(text, exe, exe_name, t.app_id)
}

fn generated_desktop(exe: &Path, t: &Target<'_>) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName={}\nComment={}\nExec={} %U\nIcon={}\nTerminal=false\n\
         Categories=Utility;\n{DESKTOP_MARKER}={}\n",
        t.name,
        t.pitch,
        desktop_exec_quote(&exe.display().to_string()),
        t.desktop_id,
        t.app_id
    )
}

/// Regular files under `dir`, relative to it (symlinks are skipped).
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(dir.join(&rel)) else { continue };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let r = rel.join(e.file_name());
            if ft.is_dir() {
                stack.push(r);
            } else if ft.is_file() {
                out.push(r);
            }
        }
    }
    out.sort();
    out
}

fn copy_file(src: &Path, dst: &Path) -> Result<(), InstallError> {
    let dir = dst.parent().ok_or_else(|| InstallError::State("file without folder".into()))?;
    std::fs::create_dir_all(dir).map_err(|e| InstallError::io(dir, e))?;
    let bytes = std::fs::read(src).map_err(|e| InstallError::io(src, e))?;
    crate::state::write_atomic(dst, &bytes)
}

/// The HKCU uninstall key for an app.
pub fn uninstall_key(app_id: &str) -> String {
    format!(r"{UNINSTALL_KEY}\{APP_ID}.{app_id}")
}

/// `HKCU\Software\Classes`: per-user file types and `ProgIDs`.
pub const CLASSES: &str = r"Software\Classes";

/// The `ProgID` of an app's files.
pub fn prog_id(app_id: &str) -> String {
    format!("{APP_ID}.{app_id}")
}

/// File name of the launcher shim shipped next to Stockroom.
pub fn shim_file_name() -> String {
    format!("{SLUG}-shim{}", std::env::consts::EXE_SUFFIX)
}

fn windows(t: &Target<'_>) -> Result<Vec<Created>, InstallError> {
    let mut created = Vec::new();
    let (Some(rel), Some(exe)) = (t.exe_rel.as_deref(), t.versioned_exe()) else { return Ok(created) };

    // The launcher shim: `<app dir>\<exe name>`, a stable target across updates.
    let launcher = match t.shim {
        Some(shim) => {
            let name = rel.rsplit('/').next().unwrap_or(rel);
            let dst = t.app_dir.join(name);
            copy_file(shim, &dst)?;
            created.push(Created::File { path: dst.clone() });
            dst
        }
        None => exe.clone(),
    };

    // Start-menu shortcut, with the icon of the real program.
    let lnk = t.env.paths.start_menu.join(format!("{}.lnk", file_name_safe(t.name)));
    if !lnk.exists() || t.prior.iter().any(|c| c.path() == Some(lnk.as_path())) {
        write_shortcut(&launcher, &exe, &lnk)?;
        created.push(Created::File { path: lnk });
    } else {
        tracing::warn!(path = %lnk.display(), "a shortcut not made by Stockroom is in the way; leaving it");
    }

    // An HKCU uninstall entry, so the app shows in Settings > Apps.
    let key = uninstall_key(t.app_id);
    let reg = &t.env.registry;
    let set = |key: &str, name: &str, value: Value| {
        reg.set(Hive::CurrentUser, key, name, value).map_err(|e| InstallError::Registry(e.to_string()))
    };
    set(&key, "DisplayName", Value::String(t.name.to_owned()))?;
    set(&key, "DisplayVersion", Value::String(t.version.to_string()))?;
    set(&key, "Publisher", Value::String(PUBLISHER.to_owned()))?;
    set(&key, "DisplayIcon", Value::String(exe.display().to_string()))?;
    set(&key, "InstallLocation", Value::String(t.app_dir.display().to_string()))?;
    set(
        &key,
        "UninstallString",
        Value::String(format!("\"{}\" remove {}", t.suite_exe.display(), t.app_id)),
    )?;
    set(&key, "EstimatedSize", Value::Dword(size_kib(&t.version_dir)))?;
    set(&key, "NoModify", Value::Dword(1))?;
    set(&key, "NoRepair", Value::Dword(1))?;
    created.push(Created::RegistryKey { hive: Hive::CurrentUser, key });

    // File associations, only when asked: a ProgID of our own, and an "Open with" entry per
    // extension that never takes over the user's default.
    if !t.file_types.is_empty() {
        let pid = prog_id(t.app_id);
        let pkey = format!(r"{CLASSES}\{pid}");
        set(&pkey, "", Value::String(t.name.to_owned()))?;
        set(&format!(r"{pkey}\DefaultIcon"), "", Value::String(format!("\"{}\",0", exe.display())))?;
        set(
            &format!(r"{pkey}\shell\open\command"),
            "",
            Value::String(format!("\"{}\" \"%1\"", launcher.display())),
        )?;
        created.push(Created::RegistryKey { hive: Hive::CurrentUser, key: pkey });
        for ext in t.file_types {
            let Some(ext) = file_extension(ext) else {
                tracing::warn!(ext, "not a file extension; skipped");
                continue;
            };
            let key = format!(r"{CLASSES}\.{ext}\OpenWithProgids");
            set(&key, &pid, Value::String(String::new()))?;
            created.push(Created::RegistryValue { hive: Hive::CurrentUser, key, name: pid.clone() });
        }
    }
    Ok(created)
}

/// `pdf` from `pdf` or `.pdf`; `None` for anything that isn't a plain extension.
fn file_extension(ext: &str) -> Option<String> {
    let e = ext.strip_prefix('.').unwrap_or(ext).to_ascii_lowercase();
    let ok = !e.is_empty()
        && e.len() <= 32
        && e.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    ok.then_some(e)
}

/// A display name usable as a file name on Windows.
fn file_name_safe(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.');
    if trimmed.is_empty() { "App".to_owned() } else { trimmed.to_owned() }
}

/// Size of a folder in KiB, for Settings > Apps (saturating).
fn size_kib(dir: &Path) -> u32 {
    let bytes: u64 =
        files_under(dir).iter().filter_map(|r| std::fs::metadata(dir.join(r)).ok()).map(|m| m.len()).sum();
    u32::try_from(bytes.div_ceil(1024)).unwrap_or(u32::MAX)
}

/// Writes a `.lnk` with the pure-Rust writer: `target` is started, `icon` provides the icon.
#[cfg(windows)]
pub(crate) fn write_shortcut(target: &Path, icon: &Path, lnk: &Path) -> Result<(), InstallError> {
    let dir = lnk.parent().ok_or_else(|| InstallError::State("shortcut without folder".into()))?;
    std::fs::create_dir_all(dir).map_err(|e| InstallError::io(dir, e))?;
    // mslnk unwraps on a target without a folder or file name, or one that isn't valid
    // Unicode; check first, and keep the panic guard for anything else.
    let unsupported = || InstallError::Shortcut(format!("{} can't be put in a shortcut", target.display()));
    let (Some(_), Some(_), Some(icon)) =
        (target.parent().and_then(Path::to_str), target.file_name().and_then(|n| n.to_str()), icon.to_str())
    else {
        return Err(unsupported());
    };
    let tmp = lnk.with_extension("lnk.suite-new");
    let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut shortcut = mslnk::ShellLink::new(target).map_err(|e| e.to_string())?;
        shortcut.set_icon_location(Some(icon.to_owned()));
        shortcut.create_lnk(&tmp).map_err(|e| e.to_string())
    }));
    match made {
        Ok(Ok(())) => std::fs::rename(&tmp, lnk).map_err(|e| InstallError::io(lnk, e)),
        Ok(Err(e)) => Err(InstallError::Shortcut(e)),
        Err(_) => Err(unsupported()),
    }
}

/// Off Windows (tests and `--root` simulating Windows) the shortcut is a text stand-in
/// naming its target and icon; `mslnk` only builds on Windows.
#[cfg(not(windows))]
pub(crate) fn write_shortcut(target: &Path, icon: &Path, lnk: &Path) -> Result<(), InstallError> {
    let text = format!("shortcut -> {}\nicon -> {}\n", target.display(), icon.display());
    crate::state::write_atomic(lnk, text.as_bytes())
}

fn macos(t: &Target<'_>) -> Result<Vec<Created>, InstallError> {
    let Some(bundle) = &t.exe_rel else { return Ok(Vec::new()) };
    let src = t.version_dir.join(bundle);
    let apps = &t.mac_dest;
    std::fs::create_dir_all(apps).map_err(|e| InstallError::io(apps, e))?;
    if !suite_platform::macos::writable(apps) {
        return Err(InstallError::Unsupported(format!(
            "{} can't be written; installing for everyone needs an administrator account",
            apps.display()
        )));
    }
    let dest = apps.join(bundle);
    // A copy the user put there (dragged from a dmg, Homebrew, the App Store) stays; the app
    // then opens from its version folder.
    if dest.exists() && !t.prior.iter().any(|c| c.path() == Some(dest.as_path())) {
        tracing::warn!(path = %dest.display(), "an app not copied by Stockroom is in the way; leaving it");
        return Ok(Vec::new());
    }
    let staging = apps.join(format!(".{bundle}.suite-new"));
    let old = apps.join(format!(".{bundle}.suite-old"));
    let _ = std::fs::remove_dir_all(&staging);
    // `--noqtn`: no quarantine attribute; Stockroom verified the download itself and
    // Gatekeeper still checks the signature.
    let out = t
        .env
        .runner
        .run("ditto", &args([Path::new("--noqtn"), &src, &staging]))
        .map_err(|e| InstallError::Unsupported(format!("ditto: {e}")))?;
    if !out.success {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(InstallError::Unsupported(format!("ditto failed: {}", out.stderr.trim())));
    }
    // Swap by renames; the old copy is deleted unless the app is running from it.
    if dest.exists() {
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(&dest, &old).map_err(|e| InstallError::io(&dest, e))?;
    }
    if staging.exists() {
        std::fs::rename(&staging, &dest).map_err(|e| InstallError::io(&dest, e))?;
    }
    if old.exists() && t.env.processes.running_under(&old).is_empty() {
        let _ = std::fs::remove_dir_all(&old);
    }
    Ok(vec![Created::Dir { path: dest }])
}

/// Deletes what an earlier integration created and a newer one no longer does.
pub(crate) fn remove_stale(env: &Env, old: &[Created], new: &[Created]) {
    let stale: Vec<Created> = old.iter().filter(|c| !new.contains(c)).cloned().collect();
    remove_created(env, &stale);
}

/// Deletes exactly what Stockroom created. Symlinks only while they still point into the
/// Stockroom's apps folder; desktop files only while they carry Stockroom's marker.
pub(crate) fn remove_created(env: &Env, created: &[Created]) {
    let apps_root = &env.paths.apps;
    for c in created {
        let res = match c {
            Created::Symlink { path } => match std::fs::read_link(path) {
                Ok(t) if t.starts_with(apps_root) => std::fs::remove_file(path),
                _ => Ok(()),
            },
            Created::File { path } => {
                let is_desktop = path.extension().is_some_and(|e| e == "desktop");
                if is_desktop && !ours_or_absent(path) { Ok(()) } else { remove_file_if_exists(path) }
            }
            Created::Dir { path } => match std::fs::remove_dir_all(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
            Created::RegistryKey { hive, key } => env.registry.delete_tree(*hive, key),
            Created::RegistryValue { hive, key, name } => env
                .registry
                .delete_value(*hive, key, name)
                .and_then(|()| delete_if_empty(env.registry.as_ref(), *hive, key, CLASSES)),
        };
        if let Err(e) = res {
            tracing::warn!(?c, error = %e, "could not remove");
        }
    }
}

fn remove_file_if_exists(p: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_rewrite() {
        let text = "[Desktop Entry]\nName=Specular\nExec=specular %f\nTryExec=specular\n\n\
                    [Desktop Action new-window]\nName=New Window\nExec=specular --new-window\n";
        let exe = Path::new("/home/u/.local/share/stockroom/apps/specular/current/specular");
        let out = rewrite_desktop_text(text, exe, "specular", "specular");
        assert!(out.contains(&format!("Exec=\"{}\" %f\n", exe.display())));
        assert!(out.contains(&format!("Exec=\"{}\" --new-window\n", exe.display())));
        assert!(out.contains(&format!("TryExec={}\n", exe.display())));
        // The marker lands in the main group, before the first action group.
        let marker = out.find("X-Stockroom-App=specular").unwrap();
        assert!(marker < out.find("[Desktop Action").unwrap());
        // Rewriting again is stable.
        assert_eq!(rewrite_desktop_text(&out, exe, "specular", "specular"), out);
        // Another program's Exec is left alone.
        let other = rewrite_desktop_text("[Desktop Entry]\nExec=env FOO=1 specular\n", exe, "specular", "s");
        assert!(other.contains("Exec=env FOO=1 specular\n"));
    }
}
