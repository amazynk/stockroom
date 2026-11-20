//! Plain, translated words for what `suite-core` reports: requirement problems, where other
//! copies came from, stores, install stages and errors. The decisions stay in the core; this
//! only phrases them.

use suite_catalog::{Arch, AssetKind, Os, Store, Unmet};
use suite_core::ErrorKind;
use suite_detect::Source;
use suite_install::Stage;
use suite_platform::SchedulerKind;

use crate::i18n::tr;

pub(crate) fn os_name(os: Os) -> &'static str {
    os.name()
}

fn arch_name(arch: Arch) -> String {
    match arch {
        Arch::X86_64 => "x86-64 (Intel, AMD)".into(),
        Arch::Aarch64 => "ARM".into(),
        Arch::Universal => tr!("any"),
    }
}

/// Why this computer can't run an app, as a sentence.
pub(crate) fn unmet(u: &Unmet) -> String {
    match u {
        Unmet::WrongOs { needs } => tr!("Made for {}", os_name(*needs)),
        Unmet::WrongArch { needs } => tr!("Made for {} processors", arch_name(*needs)),
        Unmet::NoX86_64V3 => tr!("Needs a processor with AVX2 (most from 2013 or later)"),
        Unmet::Glibc { needs, have: Some(h) } => {
            tr!("Needs a newer Linux: glibc {} or later (this one has {})", needs, h)
        }
        Unmet::Glibc { needs, have: None } => tr!("Needs glibc {} or later, which wasn't found", needs),
        Unmet::Macos { needs, have: Some(h) } => tr!("Needs macOS {} or later (this Mac has {})", needs, h),
        Unmet::Macos { needs, have: None } => tr!("Needs macOS {} or later", needs),
        Unmet::WindowsBuild { needs, have: Some(h) } => {
            tr!("Needs Windows build {} or later (this PC has {})", needs, h)
        }
        Unmet::WindowsBuild { needs, have: None } => tr!("Needs Windows build {} or later", needs),
        Unmet::KindUnsupported(kind) => {
            tr!("Comes as a {} package, which Stockroom can't install here yet", kind_name(*kind))
        }
        Unmet::NoFlatpak => tr!("Needs Flatpak, which isn't installed"),
        Unmet::BadRequirement(what) => {
            tr!("Lists a requirement this version of Stockroom doesn't know ({}); update Stockroom", what)
        }
    }
}

fn kind_name(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::TarGz => "tar.gz",
        AssetKind::Zip => "zip",
        AssetKind::Dmg => "dmg",
        AssetKind::AppImage => "AppImage",
        AssetKind::FlatpakBundle => "Flatpak",
        AssetKind::Msi => "MSI",
        AssetKind::Store => "store",
    }
}

/// "Installed by Flatpak", for copies Stockroom didn't make.
pub(crate) fn installed_by(source: &Source) -> String {
    match source {
        Source::Path => tr!("Installed separately (found on your PATH)"),
        Source::SystemPackage => tr!("Installed by your system's package manager"),
        Source::Flatpak { .. } => tr!("Installed by Flatpak"),
        Source::MacApp => tr!("Installed in Applications"),
        Source::MacAppStore => tr!("Installed by the App Store"),
        Source::MicrosoftStore => tr!("Installed by the Microsoft Store"),
        Source::Homebrew => tr!("Installed by Homebrew"),
        Source::WindowsInstaller { .. } => tr!("Installed by its own installer"),
        Source::Winget => tr!("Installed by winget"),
    }
}

pub(crate) fn store_name(store: Store) -> String {
    match store {
        Store::Microsoft => tr!("Microsoft Store"),
        Store::MacAppStore => tr!("App Store"),
        Store::Website => tr!("website"),
    }
}

/// Where an app Stockroom doesn't install itself comes from: "From the App Store".
pub(crate) fn from_store(store: Store) -> String {
    match store {
        Store::Website => tr!("From its website"),
        _ => tr!("From the {}", store_name(store)),
    }
}

/// "Opened Zynk in the App Store", "Opened the Dozed website".
pub(crate) fn opened_in_store(app: &str, store: Store) -> String {
    match store {
        Store::Website => tr!("Opened the {} website", app),
        _ => tr!("Opened {} in the {}", app, store_name(store)),
    }
}

/// What happens after the store or website was opened.
pub(crate) fn store_takes_over(store: Store) -> String {
    match store {
        Store::Website => tr!("Its website has the steps to install it."),
        _ => tr!("The store takes it from here: it installs the app and keeps it up to date."),
    }
}

/// "Get it from the Microsoft Store": the store installs and updates the app.
pub(crate) fn store_note(store: Store) -> String {
    match store {
        Store::Microsoft => {
            tr!("Get it from the Microsoft Store. The store installs it and keeps it up to date.")
        }
        Store::MacAppStore => {
            tr!("Get it from the App Store. The store installs it and keeps it up to date.")
        }
        Store::Website => tr!("Get it from its website, which has the steps to install it."),
    }
}

pub(crate) fn stage(stage: Stage) -> String {
    match stage {
        Stage::Download => tr!("Downloading"),
        Stage::Unpack => tr!("Unpacking"),
        Stage::Switch => tr!("Switching to the new version"),
        Stage::Integrate => tr!("Adding shortcuts"),
        Stage::Cleanup => tr!("Tidying up"),
    }
}

pub(crate) fn scheduler(kind: SchedulerKind) -> String {
    match kind {
        SchedulerKind::SystemdUser => tr!("a systemd user timer"),
        SchedulerKind::XdgAutostart => tr!("an autostart entry"),
        SchedulerKind::LaunchAgent => tr!("a LaunchAgent"),
        SchedulerKind::TaskScheduler => tr!("Task Scheduler"),
    }
}

/// What went wrong, in plain words; the core's own message is shown under it as details.
pub(crate) fn error(kind: ErrorKind, app: &str) -> String {
    match kind {
        ErrorKind::Busy => tr!("Another install or update is running. Try again in a moment."),
        ErrorKind::Running => tr!("{} is open. Close it, then try again.", app),
        ErrorKind::NeedsRefresh => tr!(
            "{} released a new build that the app list doesn't know yet, so nothing was changed. It works again once the list is refreshed.",
            app
        ),
        ErrorKind::Network => {
            tr!("The download server couldn't be reached. Check your connection and try again.")
        }
        ErrorKind::Integrity => {
            tr!("The download didn't match what the app list promised, so nothing was installed.")
        }
        ErrorKind::Catalog => tr!("The app list couldn't be checked, so nothing was changed."),
        ErrorKind::NotInstallable => tr!("{} can't be installed on this computer.", app),
        ErrorKind::NoPrevious => tr!("There's no earlier version of {} to go back to.", app),
        ErrorKind::Disk => tr!("Files couldn't be written. Check that the disk has free space."),
        ErrorKind::Other => tr!("Something went wrong with {}.", app),
    }
}

/// Why the app list couldn't be loaded.
pub(crate) fn catalog_problem(kind: ErrorKind, cached: bool) -> String {
    // The detail line under it says why (no connection, or what the server answered).
    match (kind, cached) {
        (ErrorKind::Network, false) => {
            tr!("The app list couldn't be downloaded. Check your connection, then try again.")
        }
        (_, true) => tr!("The newest app list couldn't be checked. Showing the last known list."),
        (_, false) => tr!("The app list couldn't be checked, so nothing can be installed right now."),
    }
}

#[cfg(test)]
mod tests {
    use suite_catalog::DottedVersion;

    use super::*;

    #[test]
    fn requirement_problems_read_plainly() {
        let have: DottedVersion = "2.31".parse().unwrap();
        assert_eq!(
            unmet(&Unmet::Glibc { needs: "2.35".into(), have: Some(have) }),
            "Needs a newer Linux: glibc 2.35 or later (this one has 2.31)"
        );
        assert_eq!(unmet(&Unmet::NoX86_64V3), "Needs a processor with AVX2 (most from 2013 or later)");
        assert_eq!(unmet(&Unmet::WrongOs { needs: Os::Windows }), "Made for Windows");
    }

    #[test]
    fn errors_say_what_to_do() {
        let pinned = error(ErrorKind::NeedsRefresh, "Zynk");
        assert!(pinned.starts_with("Zynk released a new build"), "{pinned}");
        assert!(pinned.contains("nothing was changed"));
        assert_eq!(error(ErrorKind::Running, "Harbor"), "Harbor is open. Close it, then try again.");
        assert!(catalog_problem(ErrorKind::Network, true).contains("last known list"));
    }

    #[test]
    fn other_sources() {
        assert_eq!(installed_by(&Source::Flatpak { user: true }), "Installed by Flatpak");
        assert_eq!(installed_by(&Source::MicrosoftStore), "Installed by the Microsoft Store");
        assert_eq!(store_name(Store::MacAppStore), "App Store");
    }
}
