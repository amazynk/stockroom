//! The Windows resources of `stockroom.exe`: the icon (Explorer, the Start menu, the taskbar),
//! the version and names (file properties, Task Manager), and the application manifest.
//!
//! The manifest matters most. Windows treats a program without one whose name contains
//! "setup", "install" or "update" as an installer and starts it as administrator, and the
//! Stockroom is handed out under exactly such names (`Specular-Installer.exe`). `asInvoker`
//! keeps it a per-user program. It also declares Windows 10, per-monitor DPI and long paths.
//!
//! Needs a resource compiler: rc.exe from the Windows SDK, `llvm-rc`, or MinGW's `windres`.
//! Without one, a cross-build for Windows goes on without the resources and says so;
//! `STOCKROOM_WINDOWS_RESOURCES=required` (the release script sets it) makes that an error.
//!
//! It also puts the programs a Windows release carries inside itself in `OUT_DIR`: the
//! launcher shim and `stockroom.com`, from the folder `STOCKROOM_CARRY_DIR` names
//! (`suite-shim.exe` and `suite-console.exe`, built first by `packaging/windows/build.sh`).
//! Without it they are empty and nothing is carried.

use std::{fmt::Write as _, path::PathBuf};

use suite_identity::{APP_ID, NAME, PUBLISHER, SLUG};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../packaging/icons/stockroom.ico");
    println!("cargo:rerun-if-env-changed=STOCKROOM_WINDOWS_RESOURCES");
    println!("cargo:rerun-if-env-changed=STOCKROOM_CARRY_DIR");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default());
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let carry = std::env::var_os("STOCKROOM_CARRY_DIR").filter(|_| windows).map(PathBuf::from);
    for (built, carried) in [("suite-shim.exe", "carried-shim"), ("suite-console.exe", "carried-console")] {
        let bytes = match &carry {
            Some(dir) => {
                let file = dir.join(built);
                println!("cargo:rerun-if-changed={}", file.display());
                std::fs::read(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()))
            }
            None => Vec::new(),
        };
        write_bytes(&out.join(carried), &bytes);
    }
    if !windows {
        return;
    }
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let icon = manifest_dir.join("../../packaging/icons/stockroom.ico");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let mut numbers: Vec<u16> =
        version.split(['.', '-', '+']).take(3).map(|n| n.parse().unwrap_or(0)).collect();
    numbers.resize(4, 0);
    let numeric = numbers.iter().map(u16::to_string).collect::<Vec<_>>().join(",");
    let dotted = numbers.iter().map(u16::to_string).collect::<Vec<_>>().join(".");

    let manifest = out.join(format!("{SLUG}.exe.manifest"));
    write(&manifest, &app_manifest(&dotted));
    let rc = out.join(format!("{SLUG}.rc"));
    let mut text = String::new();
    let _ = writeln!(text, "1 ICON \"{}\"", rc_path(&icon));
    // RT_MANIFEST (24), resource 1: the manifest of an executable.
    let _ = writeln!(text, "1 24 \"{}\"", rc_path(&manifest));
    let _ = write!(
        text,
        "1 VERSIONINFO\nFILEVERSION {numeric}\nPRODUCTVERSION {numeric}\nFILEOS 0x40004\nFILETYPE 0x1\n\
         BEGIN\n  BLOCK \"StringFileInfo\"\n  BEGIN\n    BLOCK \"040904B0\"\n    BEGIN\n\
         \x20     VALUE \"CompanyName\", \"{PUBLISHER}\"\n\
         \x20     VALUE \"FileDescription\", \"{NAME}\"\n\
         \x20     VALUE \"FileVersion\", \"{version}\"\n\
         \x20     VALUE \"InternalName\", \"{SLUG}\"\n\
         \x20     VALUE \"OriginalFilename\", \"{SLUG}.exe\"\n\
         \x20     VALUE \"ProductName\", \"{NAME}\"\n\
         \x20     VALUE \"ProductVersion\", \"{version}\"\n\
         \x20   END\n  END\n  BLOCK \"VarFileInfo\"\n  BEGIN\n    VALUE \"Translation\", 0x409, 1200\n  END\nEND\n"
    );
    write(&rc, &text);

    let result = embed_resource::compile(&rc, embed_resource::NONE);
    let required = std::env::var("STOCKROOM_WINDOWS_RESOURCES").is_ok_and(|v| v == "required")
        || std::env::var("HOST").is_ok_and(|h| h.contains("windows"));
    let checked = if required { result.manifest_required() } else { result.manifest_optional() };
    match checked {
        Ok(()) => {}
        Err(e) if required => panic!("the Windows resources of {SLUG}.exe: {e}"),
        Err(e) => println!("cargo:warning=the Windows resources of {SLUG}.exe were not embedded: {e}"),
    }
}

fn app_manifest(version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
  <assemblyIdentity type="win32" name="{APP_ID}" version="{version}" processorArchitecture="*"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}}"/>
    </application>
  </compatibility>
  <asmv3:application>
    <asmv3:windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </asmv3:windowsSettings>
  </asmv3:application>
</assembly>
"#
    )
}

/// A path as a resource script string: forward slashes, which every resource compiler
/// reads, so a Windows path's backslashes aren't taken as escapes.
fn rc_path(p: &std::path::Path) -> String {
    p.display().to_string().replace('\\', "/")
}

fn write(path: &std::path::Path, text: &str) {
    write_bytes(path, text.as_bytes());
}

fn write_bytes(path: &std::path::Path, bytes: &[u8]) {
    // Unchanged files keep their time, so cargo doesn't rebuild the binary for nothing.
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return;
    }
    if let Err(e) = std::fs::write(path, bytes) {
        panic!("{}: {e}", path.display());
    }
}
