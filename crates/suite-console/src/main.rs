//! `stockroom.com`, Stockroom's command line on Windows.
//!
//! `stockroom.exe` is a windowed program there (`windows_subsystem = "windows"`), so starting it
//! from Explorer, the Start menu or the daily scheduled check never shows a console. A
//! windowed program can't print to the terminal it was typed in, though, and the terminal
//! doesn't wait for it. This console program sits next to it as `stockroom.com`: Windows tries
//! `.com` before `.exe`, so `stockroom list` in a terminal runs this, which starts `stockroom.exe`
//! with the terminal's input and output (on Windows 10 these are real handles that any
//! process can use), waits for it and exits with its code, as Visual Studio's `devenv.com`
//! does. Ctrl+C ends this program but not a job `stockroom.exe` has started; Stockroom's
//! installs are atomic, so the job finishes or is repaired on the next run.
//!
//! It runs the `.exe` with its own name (`stockroom.com` → `stockroom.exe`), so a renamed pair keeps
//! working. Standard library only and no `unsafe`; it builds on every OS, but only Windows
//! packages ship it.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let program = match std::env::current_exe() {
        Ok(me) => windowed(&me),
        Err(e) => return fail(&format!("can't find Stockroom: {e}")),
    };
    match Command::new(&program).args(&args).status() {
        Ok(status) => ExitCode::from(exit_byte(status.code())),
        Err(e) => fail(&format!("{}: {e}", program.display())),
    }
}

/// The windowed program beside this one: the same name with `.exe`. Run as the build's own
/// `suite-console.exe`, that would be itself, so it is `stockroom.exe` there.
fn windowed(me: &Path) -> PathBuf {
    let twin = me.with_extension("exe");
    if twin == me { me.with_file_name(format!("stockroom{}", std::env::consts::EXE_SUFFIX)) } else { twin }
}

/// The child's exit code as a process exit code: 0 to 255 as they are, anything else
/// (a Windows status such as 0xC000013A for Ctrl+C, or none) as a failure.
fn exit_byte(code: Option<i32>) -> u8 {
    code.and_then(|c| u8::try_from(c).ok()).unwrap_or(1)
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("stockroom: {msg}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_the_exe_with_its_own_name() {
        let dir = Path::new("C:/Users/u/AppData/Local/Stockroom/bin");
        assert_eq!(windowed(&dir.join("stockroom.com")), dir.join("stockroom.exe"));
        assert_eq!(windowed(&dir.join("Specular-Installer.com")), dir.join("Specular-Installer.exe"));
        // The build's own name: Stockroom next to it.
        let built = dir.join("suite-console.exe");
        if std::env::consts::EXE_SUFFIX == ".exe" {
            assert_eq!(windowed(&built), dir.join("stockroom.exe"));
        }
    }

    #[test]
    fn exit_codes_pass_through() {
        assert_eq!(exit_byte(Some(0)), 0);
        assert_eq!(exit_byte(Some(2)), 2);
        assert_eq!(exit_byte(Some(-1_073_741_510)), 1);
        assert_eq!(exit_byte(None), 1);
    }

    #[test]
    fn passes_arguments_output_and_exit_code() {
        // A shell script stands in for stockroom.exe, so this part runs on Unix only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let exe = dir.path().join("stockroom.exe");
            std::fs::write(&exe, "#!/bin/sh\necho \"got $*\"\nexit 3\n").unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            let me = dir.path().join("stockroom.com");
            let out = Command::new(windowed(&me)).args(["list", "--all"]).output().unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout), "got list --all\n");
            assert_eq!(exit_byte(out.status.code()), 3);
        }
    }
}
