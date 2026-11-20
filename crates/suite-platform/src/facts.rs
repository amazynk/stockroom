//! Detecting [`HostFacts`]: CPU level, glibc, macOS and Windows versions, Flatpak.

use std::path::Path;

use suite_catalog::{Arch, DottedVersion, HostFacts, Os};

use crate::{
    Env,
    registry::Hive,
    runner::{Runner, args},
};

/// Whether the CPU supports x86-64-v3 (AVX2 and friends).
pub fn cpu_x86_64_v3() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("avx")
            && std::arch::is_x86_feature_detected!("avx2")
            && std::arch::is_x86_feature_detected!("bmi1")
            && std::arch::is_x86_feature_detected!("bmi2")
            && std::arch::is_x86_feature_detected!("fma")
            && std::arch::is_x86_feature_detected!("f16c")
            && std::arch::is_x86_feature_detected!("lzcnt")
            && std::arch::is_x86_feature_detected!("movbe")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// Where distributions keep glibc, relative to `/`.
const LIBC_CANDIDATES: &[&str] = &[
    "lib/x86_64-linux-gnu/libc.so.6",
    "usr/lib/x86_64-linux-gnu/libc.so.6",
    "lib/aarch64-linux-gnu/libc.so.6",
    "usr/lib/aarch64-linux-gnu/libc.so.6",
    "lib64/libc.so.6",
    "usr/lib64/libc.so.6",
    "usr/lib/libc.so.6",
    "lib/libc.so.6",
];

/// glibc version, read from `libc.so.6` under `root` without running anything: its banner
/// (`... release version 2.39.`), else the newest `GLIBC_2.x` symbol version it defines.
pub fn glibc_version(root: &Path) -> Option<DottedVersion> {
    LIBC_CANDIDATES.iter().find_map(|rel| {
        let bytes = std::fs::read(root.join(rel)).ok()?;
        glibc_from_bytes(&bytes)
    })
}

/// Parses the glibc version out of `libc.so.6` contents.
pub fn glibc_from_bytes(bytes: &[u8]) -> Option<DottedVersion> {
    const BANNER: &[u8] = b"release version ";
    if let Some(pos) = find(bytes, b"GNU C Library")
        && let Some(v) = find(&bytes[pos..], BANNER).and_then(|off| {
            let start = pos + off + BANNER.len();
            leading_version(&bytes[start..bytes.len().min(start + 16)])
        })
    {
        return Some(v);
    }
    // Fallback: the highest GLIBC_x.y version definition.
    let mut best: Option<DottedVersion> = None;
    let mut rest = bytes;
    while let Some(off) = find(rest, b"GLIBC_") {
        let tail = &rest[off + 6..];
        if let Some(v) = leading_version(&tail[..tail.len().min(12)])
            && best.as_ref().is_none_or(|b| v > *b)
        {
            best = Some(v);
        }
        rest = tail;
    }
    best
}

fn leading_version(b: &[u8]) -> Option<DottedVersion> {
    let end = b.iter().position(|c| !(c.is_ascii_digit() || *c == b'.')).unwrap_or(b.len());
    let s = std::str::from_utf8(&b[..end]).ok()?.trim_end_matches('.');
    if s.contains('.') { s.parse().ok() } else { None }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Fallback for unusual layouts: `ldd --version` prints `ldd (GNU libc) 2.39` first.
pub fn glibc_from_ldd(runner: &dyn Runner) -> Option<DottedVersion> {
    let out = runner.run("ldd", &args(["--version"])).ok()?;
    let first = out.stdout.lines().next()?;
    if !first.contains("GLIBC") && !first.contains("GNU libc") {
        return None;
    }
    first.split_whitespace().last()?.parse().ok()
}

/// macOS version from `SystemVersion.plist` under `root`.
pub fn macos_version(root: &Path) -> Option<DottedVersion> {
    let path = root.join("System/Library/CoreServices/SystemVersion.plist");
    let value = plist::Value::from_file(path).ok()?;
    value.as_dictionary()?.get("ProductVersion")?.as_string()?.parse().ok()
}

/// Windows build number from the registry.
pub fn windows_build(env: &Env) -> Option<u32> {
    env.registry
        .get_string(
            Hive::LocalMachine,
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
            "CurrentBuildNumber",
        )?
        .trim()
        .parse()
        .ok()
}

/// Detects the facts of the machine `env` describes.
///
/// glibc and the macOS version are read from the real `/` even under `--root`: they are
/// read-only facts, and an install into a test root still runs on this machine.
pub fn detect(env: &Env) -> HostFacts {
    let os = env.os;
    let real_root = Path::new("/");
    HostFacts {
        os,
        arch: Arch::current(),
        x86_64_v3: cpu_x86_64_v3(),
        glibc: (os == Os::Linux)
            .then(|| glibc_version(real_root).or_else(|| glibc_from_ldd(env.runner.as_ref())))
            .flatten(),
        macos: (os == Os::Macos).then(|| macos_version(real_root)).flatten(),
        windows_build: (os == Os::Windows).then(|| windows_build(env)).flatten(),
        flatpak: os == Os::Linux && env.paths.which("flatpak").is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{FakeRunner, Output};

    #[test]
    fn glibc_banner_and_symbol_fallback() {
        let banner =
            b"\x7fELF....GNU C Library (Ubuntu GLIBC 2.39-0ubuntu8) stable release version 2.39.\n\0";
        assert_eq!(glibc_from_bytes(banner), Some(DottedVersion::new([2, 39])));
        let symbols = b"\0GLIBC_2.2.5\0GLIBC_2.34\0GLIBC_2.4\0GLIBC_PRIVATE\0";
        assert_eq!(glibc_from_bytes(symbols), Some(DottedVersion::new([2, 34])));
        assert_eq!(glibc_from_bytes(b"musl libc"), None);
    }

    #[test]
    fn glibc_from_file_under_root() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib64");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("libc.so.6"), b"GNU C Library (GNU libc) stable release version 2.35.")
            .unwrap();
        assert_eq!(glibc_version(dir.path()), Some(DottedVersion::new([2, 35])));
        assert_eq!(glibc_version(&dir.path().join("nothing")), None);
    }

    #[test]
    fn glibc_from_ldd_output() {
        let r = FakeRunner::new();
        r.on(&["ldd", "--version"], Output::ok("ldd (Ubuntu GLIBC 2.44-1ubuntu2.1) 2.44\nCopyright"));
        assert_eq!(glibc_from_ldd(&r), Some(DottedVersion::new([2, 44])));
        r.on(&["ldd", "--version"], Output::ok("musl libc (x86_64)\nVersion 1.2.4"));
        assert_eq!(glibc_from_ldd(&r), None);
    }

    #[test]
    fn macos_version_from_plist() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("System/Library/CoreServices");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            p.join("SystemVersion.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
               <key>ProductName</key><string>macOS</string>
               <key>ProductVersion</key><string>14.6.1</string></dict></plist>"#,
        )
        .unwrap();
        assert_eq!(macos_version(dir.path()), Some(DottedVersion::new([14, 6, 1])));
    }

    #[test]
    fn windows_build_from_registry() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::for_tests(dir.path(), Os::Windows);
        env.registry
            .set(
                Hive::LocalMachine,
                r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
                "CurrentBuildNumber",
                crate::registry::Value::String("22631".into()),
            )
            .unwrap();
        assert_eq!(detect(&env).windows_build, Some(22631));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn this_machine_has_a_glibc() {
        // Read-only: the build machine runs glibc (the toolchain is gnu).
        assert!(glibc_version(Path::new("/")).is_some());
    }
}
