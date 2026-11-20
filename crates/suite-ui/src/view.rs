//! View models: what each screen shows, built from `suite-core`'s rows as plain data so the
//! decisions are unit-tested without a window. `bind` copies them into the Slint structs.

use std::collections::HashMap;

use suite_catalog::{AppStatus, Channel, HostFacts, NotInstallable, Requirements, Store, Unmet};
use suite_core::{AppRow, ChangeKind};
use suite_install::{Change, InstallKind, Policy};

use crate::{
    format,
    i18n::{tr, trn},
    words,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Tone {
    #[default]
    Normal,
    Muted,
    Ok,
    Warn,
    Bad,
    Accent,
}

/// What a card's or page's main button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Act {
    #[default]
    None,
    Install,
    Open,
    Update,
    Store(Store),
    /// A job for the app is queued or running.
    Busy,
}

/// An app as the window knows it: the core's row, plus the newest beta for this computer.
#[derive(Debug, Clone)]
pub(crate) struct Row {
    pub core: AppRow,
    pub beta: Option<semver::Version>,
}

impl Row {
    pub(crate) fn id(&self) -> &str {
        &self.core.app.id
    }

    pub(crate) fn name(&self) -> &str {
        &self.core.app.name
    }

    /// Installed by Stockroom (not by a store or other means).
    pub(crate) fn managed(&self) -> bool {
        self.core.installed.is_some()
    }

    /// What the main button does, given whether a job for the app is under way.
    pub(crate) fn act(&self, busy: bool) -> Act {
        let row = &self.core;
        if busy {
            return Act::Busy;
        }
        if row.app.status == AppStatus::Unreleased {
            return Act::None;
        }
        if row.installed.is_some() {
            return if row.update_available().is_some() { Act::Update } else { Act::Open };
        }
        if row.others.iter().any(|f| suite_core::can_open_found(&row.app, f)) {
            return Act::Open;
        }
        if !row.others.is_empty() {
            return Act::None;
        }
        match (&row.available, row.via_store) {
            (Ok(_), Some(store)) => Act::Store(store),
            (Ok(_), None) => Act::Install,
            (Err(_), _) => Act::None,
        }
    }
}

/// The main button's words.
pub(crate) fn act_text(act: Act) -> String {
    match act {
        Act::Install => tr!("Install"),
        Act::Open => tr!("Open"),
        Act::Update => tr!("Update"),
        Act::Store(_) => tr!("Get it"),
        Act::None | Act::Busy => String::new(),
    }
}

/// The first letter of a name, for its tile.
pub(crate) fn initial(name: &str) -> String {
    name.chars().find(|c| c.is_alphanumeric()).map_or_default(|c| c.to_uppercase().to_string())
}

/// A stable tile colour per app.
pub(crate) fn tint(id: &str) -> i32 {
    let h = id.bytes().fold(7_u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    i32::try_from(h % 8).unwrap_or_default()
}

/// One line about where the app stands, and its tone.
pub(crate) fn status(row: &Row, busy: Option<&str>) -> (String, Tone) {
    let r = &row.core;
    if let Some(stage) = busy {
        return (stage.to_owned(), Tone::Accent);
    }
    if r.app.status == AppStatus::Unreleased {
        return (tr!("Coming soon"), Tone::Muted);
    }
    if let Some(i) = &r.installed {
        return match r.update_available() {
            Some(v) => (
                tr!("Installed {} · {} is ready", format::version(&i.current), format::version(v)),
                Tone::Accent,
            ),
            None => (tr!("Installed · {}", format::version(&i.current)), Tone::Ok),
        };
    }
    if let Some(f) = r.others.first() {
        return (words::installed_by(&f.source), Tone::Ok);
    }
    match (&r.available, r.via_store) {
        (Ok(_), Some(store)) => (words::from_store(store), Tone::Muted),
        (Ok(v), None) => {
            let size = row.core.best.as_ref().map(|(_, a)| a.size).filter(|s| *s > 0);
            match size {
                Some(s) => (tr!("Version {} · {}", format::version(v), format::bytes(s)), Tone::Muted),
                None => (tr!("Version {}", format::version(v)), Tone::Muted),
            }
        }
        (Err(NotInstallable::Unsupported(reasons)), _) => {
            // An app for another system isn't something wrong with this computer.
            let tone = match reasons.first() {
                Some(Unmet::WrongOs { .. }) => Tone::Muted,
                _ => Tone::Warn,
            };
            (reasons.first().map_or_default(words::unmet), tone)
        }
        (Err(NotInstallable::NoRelease { channel }), _) => (no_release(*channel), Tone::Muted),
        (Err(NotInstallable::ComingSoon), _) => (tr!("Coming soon"), Tone::Muted),
    }
}

fn no_release(channel: Channel) -> String {
    match channel {
        Channel::Stable => tr!("No stable version yet"),
        Channel::Beta => tr!("No beta version yet"),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CardView {
    pub id: String,
    /// "AMZ-01"; the maker for an app from another maker; empty before it has a number.
    pub part: String,
    pub name: String,
    pub pitch: String,
    pub vendor: String,
    pub status: String,
    pub tone: Tone,
    pub act: Act,
    pub act_text: String,
    /// Below 0: no job.
    pub progress: f32,
}

/// A job under way for an app: its progress (below 0 if unknown) and stage.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Busy {
    pub fraction: f32,
    pub stage: String,
}

pub(crate) fn card(row: &Row, busy: Option<&Busy>) -> CardView {
    let act = row.act(busy.is_some());
    let (status, tone) = status(row, busy.map(|b| b.stage.as_str()));
    CardView {
        id: row.id().to_owned(),
        part: row
            .core
            .app
            .part
            .clone()
            .or_else(|| row.core.app.vendor.as_ref().map(|v| v.to_uppercase()))
            .unwrap_or_default(),
        name: row.name().to_owned(),
        pitch: row.core.app.pitch.clone(),
        vendor: row.core.app.vendor.as_ref().map_or_default(|v| tr!("by {}", v)),
        status,
        tone,
        act,
        act_text: act_text(act),
        progress: busy.map_or(-1.0, |b| b.fraction.max(0.0)),
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Home {
    /// The family's apps, in catalog order.
    pub family: Vec<CardView>,
    /// Apps from other makers (Zynk), after them.
    pub others: Vec<CardView>,
    /// (id, name, "1.1.0 → 1.2.0")
    pub updates: Vec<(String, String, String)>,
    pub updates_title: String,
    /// What "Install all" installs.
    pub install_all: Vec<String>,
    pub install_all_text: String,
    /// "IN STOCK: 02": how many of the family's apps are out.
    pub stock: String,
}

pub(crate) fn home(rows: &[Row], busy: &HashMap<String, Busy>) -> Home {
    let mut h = Home::default();
    for row in rows {
        let b = busy.get(row.id());
        let card = card(row, b);
        if row.core.app.vendor.is_some() {
            h.others.push(card);
        } else {
            h.family.push(card);
        }
        if let (Some(i), Some(v)) = (&row.core.installed, row.core.update_available())
            && b.is_none()
        {
            h.updates.push((row.id().to_owned(), row.name().to_owned(), format!("{} → {v}", i.current)));
        }
        if row.core.app.vendor.is_none() && b.is_none() && row.act(false) == Act::Install {
            h.install_all.push(row.id().to_owned());
        }
    }
    let out = rows.iter().filter(|r| r.core.app.vendor.is_none() && !r.core.app.releases.is_empty()).count();
    if out > 0 {
        h.stock = tr!("IN STOCK: {}", format!("{out:02}"));
    }
    let n = h.updates.len();
    h.updates_title = trn!("{n} update is ready", "{n} updates are ready", n);
    // One app isn't "all"; its card has its own button.
    if h.install_all.len() >= 2 {
        h.install_all_text = tr!("Install all ({})", h.install_all.len());
    } else {
        h.install_all.clear();
    }
    h
}

/// The other apps to offer while `current` installs: at most `limit` of those that aren't
/// on this computer yet, the ones it can have now before the ones still to come. Apps that
/// can't run here are left out.
pub(crate) fn more(rows: &[Row], current: &str, limit: usize) -> Vec<CardView> {
    let offered =
        |row: &Row| row.id() != current && row.core.installed.is_none() && row.core.others.is_empty();
    let ready = |row: &Row| matches!(row.act(false), Act::Install | Act::Store(_));
    let soon = |row: &Row| row.core.app.status == AppStatus::Unreleased;
    let pick = |wanted: &dyn Fn(&Row) -> bool| -> Vec<&Row> {
        rows.iter().filter(|r| offered(r) && wanted(r)).collect()
    };
    pick(&ready).into_iter().chain(pick(&soon)).take(limit).map(|row| card(row, None)).collect()
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct OtherView {
    pub text: String,
    pub note: String,
    pub can_open: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct PageView {
    pub card: Option<CardView>,
    pub description: String,
    pub facts: Vec<(String, String)>,
    /// (text, fulfilled)
    pub reqs: Vec<(String, bool)>,
    pub problem: String,
    pub notes_url: String,
    pub homepage: String,
    pub managed: bool,
    pub channel: Channel,
    pub can_beta: bool,
    pub policy: Policy,
    pub rollback: String,
    pub has_data: bool,
    pub store_note: String,
    pub others: Vec<OtherView>,
    /// (label, link)
    pub phones: Vec<(String, String)>,
    pub stage: String,
}

/// The app page. `channel` is the one an install would use (the installed one, or the
/// choice made on the page).
pub(crate) fn page(row: &Row, busy: Option<&Busy>, facts: &HostFacts, channel: Channel) -> PageView {
    let r = &row.core;
    let app = &r.app;
    let mut v = PageView {
        card: Some(card(row, busy)),
        description: app.description.clone(),
        homepage: app.homepage.clone(),
        managed: row.managed(),
        channel: r.installed.as_ref().map_or(channel, |i| i.channel),
        policy: r.installed.as_ref().map_or_default(|i| i.policy),
        has_data: !app.data_paths.for_os(facts.os).is_empty(),
        stage: busy.map_or_default(|b| b.stage.clone()),
        ..PageView::default()
    };
    v.can_beta = r.via_store.is_none()
        && app.status == AppStatus::Available
        && (row.beta.as_ref().is_some_and(|b| r.available.as_ref().map_or(true, |s| b > s))
            || v.channel == Channel::Beta);

    // Facts.
    if let Some(i) = &r.installed {
        v.facts.push((tr!("Installed"), format::version(&i.current)));
        if let Some(new) = r.update_available() {
            v.facts.push((tr!("Newest"), format::version(new)));
        }
        v.facts.push((tr!("Since"), format::date(i.installed)));
    } else if let Ok(ver) = &r.available {
        let shown = if v.channel == Channel::Beta { row.beta.as_ref().unwrap_or(ver) } else { ver };
        v.facts.push((tr!("Version"), format::version(shown)));
    }
    if let Some((release, asset)) = &r.best {
        if asset.size > 0 {
            v.facts.push((tr!("Download"), format::bytes(asset.size)));
        }
        if let Some(d) = release.date {
            v.facts.push((tr!("Released"), format::date(d)));
        }
        v.notes_url = release.notes_url.clone().unwrap_or_default();
    }
    if !app.license.is_empty() {
        v.facts.push((tr!("Licence"), app.license.clone()));
    }
    if let Some(part) = &app.part {
        v.facts.insert(0, (tr!("Part"), part.clone()));
    }
    if let Some(vendor) = &app.vendor {
        v.facts.push((tr!("Made by"), vendor.clone()));
    }

    // Requirements, and what's wrong when the computer can't run it.
    match &r.available {
        Ok(_) => v.reqs = requirement_lines(r.best.as_ref().map(|(_, a)| &a.requires), facts),
        Err(NotInstallable::Unsupported(reasons)) => {
            v.reqs = reasons.iter().map(|u| (words::unmet(u), false)).collect();
            v.problem = tr!("{} can't run on this computer.", app.name);
        }
        Err(NotInstallable::NoRelease { channel }) if app.status == AppStatus::Available => {
            v.problem = no_release(*channel);
        }
        Err(_) => {}
    }

    if let (Some(store), None) = (r.via_store, &r.installed) {
        v.store_note = words::store_note(store);
    }
    if let Some(i) = &r.installed
        && i.kind != InstallKind::Flatpak
        && let Some(prev) = &i.previous
    {
        v.rollback = tr!("Go back to {}", prev);
    }
    v.others = r
        .others
        .iter()
        .map(|f| OtherView {
            text: match &f.version {
                Some(ver) => format!("{} · {ver}", words::installed_by(&f.source)),
                None => words::installed_by(&f.source),
            },
            note: tr!("Its updates come from there; Stockroom leaves this copy alone."),
            can_open: suite_core::can_open_found(app, f),
        })
        .collect();
    if let Some(link) = &app.mobile.android {
        v.phones.push(("Android".into(), link.clone()));
    }
    if let Some(link) = &app.mobile.ios {
        v.phones.push(("iPhone".into(), link.clone()));
    }
    v
}

/// What the chosen download needs, each checked against this computer.
fn requirement_lines(req: Option<&Requirements>, facts: &HostFacts) -> Vec<(String, bool)> {
    let mut out = vec![(tr!("Made for {}, which this computer runs", words::os_name(facts.os)), true)];
    let Some(req) = req else { return out };
    let unmet = suite_catalog::select::unmet_requirements(req, facts);
    if req.x86_64_v3 {
        out.push((tr!("A processor with AVX2"), !unmet.contains(&suite_catalog::Unmet::NoX86_64V3)));
    }
    if let Some(g) = &req.glibc {
        let ok = !unmet.iter().any(|u| matches!(u, suite_catalog::Unmet::Glibc { .. }));
        out.push((tr!("glibc {} or later", g), ok));
    }
    if let Some(m) = &req.macos {
        let ok = !unmet.iter().any(|u| matches!(u, suite_catalog::Unmet::Macos { .. }));
        out.push((tr!("macOS {} or later", m), ok));
    }
    if let Some(b) = req.windows_build {
        let ok = !unmet.iter().any(|u| matches!(u, suite_catalog::Unmet::WindowsBuild { .. }));
        out.push((tr!("Windows build {} or later", b), ok));
    }
    out
}

/// One line of the log of past changes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LogView {
    pub when: String,
    pub text: String,
    pub tone: Tone,
}

/// The history, newest first.
pub(crate) fn log(history: &[Change], now: time::OffsetDateTime) -> Vec<LogView> {
    history
        .iter()
        .rev()
        .map(|c| {
            let n = &c.name;
            let (text, tone) = match &c.what {
                ChangeKind::Installed { version } => (tr!("Installed {} {}", n, version), Tone::Normal),
                ChangeKind::Updated { from, to } => {
                    (tr!("Updated {} from {} to {}", n, from, to), Tone::Normal)
                }
                ChangeKind::RolledBack { from, to } => {
                    (tr!("Took {} back from {} to {}", n, from, to), Tone::Normal)
                }
                ChangeKind::Removed { version, purged: false } => {
                    (tr!("Removed {} {}", n, version), Tone::Normal)
                }
                ChangeKind::Removed { version, purged: true } => {
                    (tr!("Removed {} {} with its settings and data", n, version), Tone::Normal)
                }
                ChangeKind::OpenedStore { store } => (words::opened_in_store(n, *store), Tone::Normal),
            };
            LogView { when: format::when(c.at, now), text, tone }
        })
        .collect()
}

/// Policy as the page's choice index, and back.
pub(crate) fn policy_index(p: Policy) -> i32 {
    match p {
        Policy::Auto => 0,
        Policy::Ask => 1,
        Policy::Never => 2,
    }
}

pub(crate) fn policy_from(i: i32) -> Policy {
    match i {
        1 => Policy::Ask,
        2 => Policy::Never,
        _ => Policy::Auto,
    }
}

/// The OS facts a page needs when there is no host to ask (tests, demos).
#[cfg(test)]
pub(crate) fn linux() -> HostFacts {
    HostFacts::example(suite_catalog::Os::Linux)
}

#[cfg(test)]
mod tests {
    use suite_catalog::{Arch, Asset, AssetKind, Layout, Mobile, Os, Release, StoreLink};
    use suite_detect::{Found, Source};
    use suite_install::{AppState, InstallKind};
    use time::OffsetDateTime;

    use super::*;

    fn v(s: &str) -> semver::Version {
        s.parse().unwrap()
    }

    fn asset(size: u64) -> Asset {
        Asset {
            os: Os::Linux,
            arch: Arch::X86_64,
            kind: AssetKind::TarGz,
            url: "https://example.com/a.tar.gz".into(),
            size,
            sha256: None,
            requires: Requirements { x86_64_v3: true, glibc: Some("2.35".into()), ..Requirements::default() },
            layout: Layout::default(),
            store: None,
            pinned: false,
            note: None,
        }
    }

    fn release(ver: &str) -> Release {
        Release {
            version: v(ver),
            channel: Channel::Stable,
            date: Some(OffsetDateTime::UNIX_EPOCH),
            notes_url: Some("https://example.com/notes".into()),
            assets: vec![asset(42_000_000)],
        }
    }

    fn row(id: &str) -> Row {
        let mut app = suite_testkit::app_entry(id);
        app.name = format!("{}{}", id[..1].to_uppercase(), &id[1..]);
        Row {
            core: AppRow {
                app,
                installed: None,
                available: Ok(v("1.2.0")),
                via_store: None,
                others: Vec::new(),
                best: Some((release("1.2.0"), asset(42_000_000))),
            },
            beta: None,
        }
    }

    fn installed(r: &mut Row, current: &str, previous: Option<&str>) {
        let now = OffsetDateTime::UNIX_EPOCH;
        r.core.installed = Some(AppState {
            name: r.core.app.name.clone(),
            kind: InstallKind::Archive,
            channel: Channel::Stable,
            policy: Policy::Ask,
            current: v(current),
            previous: previous.map(v),
            versions: Vec::new(),
            executable: Some("x".into()),
            flatpak_id: None,
            desktop_id: None,
            bin_name: None,
            pitch: String::new(),
            layouts: std::collections::BTreeMap::new(),
            installed: now,
            updated: now,
            created: Vec::new(),
            associate: false,
        });
    }

    fn zynk() -> Row {
        let mut z = row("zynk");
        z.core.app.vendor = Some("Zynk".into());
        z.core.app.mobile = Mobile {
            android: Some("https://play.google.com/store/apps/details?id=it.zynk".into()),
            ios: Some("https://apps.apple.com/app/zynk/id1".into()),
        };
        z
    }

    #[test]
    fn cards_say_what_each_app_needs() {
        let mut spec = row("specular");
        installed(&mut spec, "1.1.0", Some("1.0.0"));
        let c = card(&spec, None);
        assert_eq!((c.act, c.act_text.as_str()), (Act::Update, "Update"));
        assert_eq!(c.status, "Installed 1.1.0 · 1.2.0 is ready");

        let mut harbor = row("harbor");
        installed(&mut harbor, "1.2.0", None);
        assert_eq!(card(&harbor, None).act, Act::Open);
        assert_eq!(card(&harbor, None).status, "Installed · 1.2.0");

        let bindery = row("bindery");
        assert_eq!(card(&bindery, None).act, Act::Install);
        assert_eq!(card(&bindery, None).status, "Version 1.2.0 · 42 MB");

        let mut soon = row("foundry");
        soon.core.app.status = AppStatus::Unreleased;
        soon.core.available = Err(NotInstallable::ComingSoon);
        assert_eq!((card(&soon, None).act, card(&soon, None).status.as_str()), (Act::None, "Coming soon"));

        let mut old = row("almanac");
        old.core.available = Err(NotInstallable::Unsupported(vec![Unmet::NoX86_64V3]));
        old.core.best = None;
        let c = card(&old, None);
        assert_eq!((c.act, c.tone), (Act::None, Tone::Warn));
        assert!(c.status.contains("AVX2"), "{}", c.status);

        let busy = Busy { fraction: 0.4, stage: "Downloading".into() };
        let c = card(&bindery, Some(&busy));
        assert_eq!((c.act, c.progress, c.status.as_str()), (Act::Busy, 0.4, "Downloading"));
    }

    #[test]
    fn the_install_window_offers_what_is_not_here_yet() {
        let mut have = row("harbor");
        installed(&mut have, "1.2.0", None);
        let mut soon = row("foundry");
        soon.core.app.status = AppStatus::Unreleased;
        soon.core.available = Err(NotInstallable::ComingSoon);
        let mut cant = row("almanac");
        cant.core.available = Err(NotInstallable::Unsupported(vec![Unmet::NoX86_64V3]));
        cant.core.best = None;
        let mut elsewhere = row("dozed");
        elsewhere.core.others = vec![Found { source: Source::Path, version: None, location: None }];
        let rows = [soon, row("specular"), have, cant, row("bindery"), elsewhere, zynk()];
        let ids = |current: &str, limit: usize| -> Vec<String> {
            more(&rows, current, limit).into_iter().map(|c| c.id).collect()
        };
        // Not the app being installed, nothing already here, nothing that can't run here;
        // what can be had now comes before what is still to come.
        assert_eq!(ids("specular", 5), ["bindery", "zynk", "foundry"]);
        assert_eq!(ids("bindery", 2), ["specular", "zynk"]);
        assert_eq!(ids("nope", 0), [] as [&str; 0]);
    }

    #[test]
    fn other_installs_open_but_never_update() {
        let mut z = zynk();
        z.core.others = vec![Found {
            source: Source::Flatpak { user: true },
            version: Some("3.1".into()),
            location: None,
        }];
        z.core.app.ids.flatpak = Some("it.zynk.Zynk".into());
        let c = card(&z, None);
        assert_eq!(
            (c.act, c.status.as_str(), c.vendor.as_str()),
            (Act::Open, "Installed by Flatpak", "by Zynk")
        );
        let p = page(&z, None, &linux(), Channel::Stable);
        assert!(!p.managed, "no channel, policy or update for someone else's install");
        assert_eq!(p.others[0].text, "Installed by Flatpak · 3.1");
        assert!(p.others[0].can_open);
        // Something found but not startable (an installer entry without a program).
        z.core.others =
            vec![Found { source: Source::WindowsInstaller { machine: true }, version: None, location: None }];
        assert_eq!(card(&z, None).act, Act::None);
    }

    #[test]
    fn store_apps_point_at_their_store() {
        let mut z = zynk();
        z.core.via_store = Some(Store::Microsoft);
        let mut a = asset(0);
        a.kind = AssetKind::Store;
        a.store = Some(StoreLink {
            store: Store::Microsoft,
            product_id: "9PP".into(),
            url: "https://apps.microsoft.com/x".into(),
            link: "ms-windows-store://pdp/?productid=9PP".into(),
        });
        z.core.best = Some((release("3.0.0"), a));
        let c = card(&z, None);
        assert_eq!((c.act, c.act_text.as_str()), (Act::Store(Store::Microsoft), "Get it"));
        assert_eq!(c.status, "From the Microsoft Store");
        let p = page(&z, None, &HostFacts::example(Os::Windows), Channel::Stable);
        assert!(p.store_note.starts_with("Get it from the Microsoft Store"), "{}", p.store_note);
        assert!(!p.can_beta);
        assert_eq!(p.phones.len(), 2);
        assert!(!p.facts.iter().any(|f| f.0 == "Download"), "no size for a store listing");
    }

    #[test]
    fn home_groups_and_counts() {
        let mut spec = row("specular");
        installed(&mut spec, "1.1.0", None);
        let mut soon = row("foundry");
        soon.core.app.status = AppStatus::Unreleased;
        soon.core.available = Err(NotInstallable::ComingSoon);
        let rows = vec![zynk(), spec, row("harbor"), row("bindery"), soon];
        let h = home(&rows, &HashMap::new());
        assert_eq!(
            h.family.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["specular", "harbor", "bindery", "foundry"]
        );
        assert_eq!(h.others.len(), 1, "Zynk goes after the family");
        assert_eq!(h.updates, [("specular".into(), "Specular".into(), "1.1.0 → 1.2.0".into())]);
        assert_eq!(h.updates_title, "1 update is ready");
        // The family's installable apps, not Zynk, nothing installed or coming soon.
        assert_eq!(h.install_all, ["harbor", "bindery"]);
        assert_eq!(h.install_all_text, "Install all (2)");
        // While one installs, it's neither in "Install all" nor in the updates.
        let busy = HashMap::from([("harbor".to_owned(), Busy { fraction: -1.0, stage: "Waiting".into() })]);
        let h = home(&rows, &busy);
        assert!(h.install_all.is_empty(), "one app left isn't \"all\"");
    }

    #[test]
    fn page_shows_requirements_and_management() {
        let mut spec = row("specular");
        installed(&mut spec, "1.1.0", Some("1.0.0"));
        let p = page(&spec, None, &linux(), Channel::Stable);
        assert!(p.managed);
        assert_eq!(p.policy, Policy::Ask);
        assert_eq!(p.rollback, "Go back to 1.0.0");
        assert!(p.has_data);
        assert_eq!(p.notes_url, "https://example.com/notes");
        assert!(p.reqs.iter().all(|r| r.1), "{:?}", p.reqs);
        assert!(p.reqs.iter().any(|r| r.0 == "glibc 2.35 or later"));
        assert!(p.facts.contains(&("Newest".into(), "1.2.0".into())));
        assert!(p.facts.contains(&("Download".into(), "42 MB".into())));

        let mut old = row("almanac");
        old.core.available = Err(NotInstallable::Unsupported(vec![
            Unmet::NoX86_64V3,
            Unmet::Glibc { needs: "2.39".into(), have: Some("2.31".parse().unwrap()) },
        ]));
        old.core.best = None;
        let p = page(&old, None, &linux(), Channel::Stable);
        assert_eq!(p.problem, "Almanac can't run on this computer.");
        assert_eq!(p.reqs.len(), 2);
        assert!(p.reqs.iter().all(|r| !r.1));

        // A beta newer than stable offers the channel; choosing it shows the beta version.
        let mut b = row("bindery");
        b.beta = Some(v("1.3.0-beta.1"));
        let p = page(&b, None, &linux(), Channel::Beta);
        assert!(p.can_beta);
        assert!(p.facts.contains(&("Version".into(), "1.3.0-beta.1".into())));
        assert!(!page(&row("harbor"), None, &linux(), Channel::Stable).can_beta);
    }

    #[test]
    fn log_is_newest_first_and_plain() {
        let at = OffsetDateTime::now_utc();
        let change = |what| Change { at, app: "specular".into(), name: "Specular".into(), what };
        let h = vec![
            change(ChangeKind::Installed { version: v("1.0.0") }),
            change(ChangeKind::Updated { from: v("1.0.0"), to: v("1.1.0") }),
            change(ChangeKind::Removed { version: v("1.1.0"), purged: true }),
        ];
        let rows = log(&h, at);
        assert_eq!(rows[0].text, "Removed Specular 1.1.0 with its settings and data");
        assert_eq!(rows[1].text, "Updated Specular from 1.0.0 to 1.1.0");
        assert!(rows[2].when.starts_with("Today"));
    }

    #[test]
    fn tiles_and_policies() {
        assert_eq!(initial("specular"), "S");
        assert_eq!(initial("  zynk"), "Z");
        assert_eq!(tint("harbor"), tint("harbor"));
        assert!((0..8).contains(&tint("bindery")));
        for p in [Policy::Auto, Policy::Ask, Policy::Never] {
            assert_eq!(policy_from(policy_index(p)), p);
        }
    }
}
