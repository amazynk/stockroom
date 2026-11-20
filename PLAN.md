# Stockroom — Plan

> Installs, updates and removes the family of apps (Specular, Dozed, Pagebara and whatever joins
> later) on Windows, macOS and Linux, in one small, fast, private app. Rust on nightly, Slint UI.
> Hard rule: zero `unsafe` in our own code. "Stockroom" is a working name, kept in one place
> (`crates/suite-identity`), like Pagebara's identity crate.

---

## 1. Goals and measurable targets

| Goal | Target |
|---|---|
| Install an app | One click; no admin rights or UAC prompt by default; under 10 s on a fast link for a 50 MB app |
| Trust | Every byte installed is covered by the catalog, served over HTTPS from amazynk.com: SHA-256 per asset. The catalog itself is not signed (decided 2026-11-20, see §3) |
| Updates | Checked at most daily and in the background, without a resident process. Applied atomically, with the previous version kept for one-click rollback |
| Never break an app | An update in progress never leaves a half-written app. A running app keeps running and picks up the update on its next start |
| Respect the user's installs | A copy installed some other way (distro package, Flatpak, MSI, App Store, Homebrew) is detected, shown, and never touched |
| Footprint | Cold start under 150 ms; idle 0% CPU; installer download under 15 MB |
| Privacy | No account, no telemetry, no IDs. The catalog fetch is a plain GET, the same for everyone |

## 2. Shape

- **One binary, `stockroom`,** with three faces:
  - the app window (catalog, installed apps, updates, settings)
  - a CLI: `stockroom list | install <app> | update [<app>|--all] | remove <app> | rollback <app> | status | doctor`
  - `stockroom update --background`: the scheduled run. It checks, downloads and applies updates where allowed, and notifies when an app needs closing.
- **Bootstrap installers** are small, per-OS packages that install `stockroom` itself (per-user), then open it on the catalog. The family's apps can also ship a "Get Stockroom" link.
- **Sibling apps can delegate installs to it.** Running `stockroom install specular --ui` shows a compact install-and-progress window, e.g. another app's "Get Specular". When Stockroom isn't present, apps keep their own fallbacks.

## 3. The catalog

- One JSON document, `catalog.json`, served over HTTPS from amazynk.com.
  - Not signed, by the owner's decision on 2026-11-20: Stockroom takes what amazynk.com serves. The first design had a detached minisign (Ed25519) signature with the public key compiled in, a pinned rotation key and a 30-day expiry; it was built and then removed, so publishing needs no secret key and no monthly re-signing. Downloads are still checked against the SHA-256 in the list, and every list must keep the format's rules (path-safe IDs and paths, HTTPS addresses, a checksum per download).
  - The URL is in `suite-identity`: `https://amazynk.com/stockroom/catalog.json`.
- **Contents:**
  - `version` (a serial; a lower one than already seen is refused) and `published` (RFC 3339).
  - Apps: id, name, one-line pitch, description, icon (URL + SHA-256), homepage, licence, category, and `ids` (the app's platform IDs: Flatpak/desktop ID, macOS bundle ID, Windows AppUserModelID and uninstall key) so existing installs can be found.
  - Per release: version, notes URL, date, `min_os`, `requires` (`x86-64-v3`/AVX2, glibc ≥ x, macOS ≥ x, Windows build ≥ x), and assets per platform. Each asset has: url, size, sha256, `kind` (`tar.gz`, `zip`, `dmg`, `appimage`, `flatpak-bundle`, `msi`), and `layout`: the main executable's path inside, the icon and the desktop file.
- **Channels:** `stable` and `beta` per app. The user chooses per app; the default is stable.
- **`suite-catalog`, a publishing tool:**
  - Builds `catalog.json` from GitHub releases. Assets plus their `.sha256` siblings, as Specular already publishes them, are checked by downloading them.
  - Merges in hand-written app metadata (`catalog/apps/*.toml`).
  - Lives in the same workspace.

## 4. Installing (per user, no admin, by default)

| OS | Preferred asset | Where | Integration |
|---|---|---|---|
| Linux | `tar.gz` (else AppImage) | `~/.local/share/<stockroom>/apps/<app>/<version>/`, with `current` a symlink | `~/.local/bin/<app>` symlink, `.desktop` file and icons in `~/.local/share/{applications,icons}`, MIME types from the app's desktop file, `update-desktop-database` if present |
| Linux (Flatpak-only systems, or when the user prefers it) | `flatpak-bundle` | `flatpak install --user` | Flatpak handles it; Stockroom only tracks the version |
| Windows | `zip` (portable) | `%LOCALAPPDATA%\Programs\<stockroom>\<app>\<version>\`, with a `current.txt` pointer and a tiny launcher shim | Start-menu shortcut (`.lnk`), an HKCU uninstall entry (shows in Settings → Apps), file associations under HKCU only on request |
| macOS | `dmg` | `~/Applications/<App>.app` (or `/Applications` when the user chooses it and authorises) | `hdiutil attach -nobrowse -readonly`, `ditto` copy, detach. Quarantine isn't added, since we downloaded it ourselves and verified it; Gatekeeper still checks signatures |

- **The steps:** download (resumable, with Range) into a staging folder, verify size and SHA-256, then extract safely:
  - no `..`, no absolute paths
  - no symlinks pointing outside, no hard links
  - permission bits kept on Unix
  - size and entry-count limits against archive bombs
- **Then switch:** atomically switch `current` (rename/symlink swap) and write the state. Keep the previous version until the next successful update; delete older ones.
- **A running app** (detected by process path) is never overwritten. The new version is staged next to it and becomes current; the shortcut targets the pointer, so the next launch gets it.
- **Removing** deletes the version folders, shortcuts, desktop entries and registry entries Stockroom created, and nothing else. App data and settings stay unless the user ticks "also remove its settings and data" (paths per app, from the catalog).
- **Detecting other installs:** PATH, `/usr/bin`, `flatpak info <id>`, `/Applications/*.app` and `~/Applications` (bundle ID from Info.plist), Windows HKLM/HKCU uninstall keys, Homebrew casks, winget.
  - Shown as "installed by <source>", with version where readable, and Open.
  - Updates for those are left to their source; Stockroom says so instead of offering one.

## 5. Updates

- **Scheduling** uses the OS scheduler, not a resident process:
  - a systemd user timer on Linux (or an XDG autostart entry where there's no systemd)
  - a LaunchAgent on macOS
  - a Task Scheduler task created with `schtasks` on Windows

  Daily, with jitter. Toggled in Settings, default on. Metered connections are skipped (NetworkManager or `GetNetworkConnectivityHint`).
- **Policy per app:** install automatically (default), ask first, or never.
- **Rollback** to the kept previous version is one click, or `stockroom rollback <app>`.
- **Stockroom updates itself** through the same catalog: it's an app in it, and it replaces itself via a staged copy and a relaunch.

## 6. UI (Slint, same look and engineering as the family)

- **Home:** a grid of the family's apps (icon, name, pitch, Install/Open/Update), an "Install all" button, and updates at the top when there are any.
- **App page:** description, screenshots (from the catalog, cached), version, size, requirements (with a plain reason when this computer can't run it), release notes link, channel, update policy, Rollback and Remove.
- **Activity:** downloads and installs with progress, speed and time left (a smoothed speed and a steady time left), plus a log of past changes.
- **Settings:** automatic updates, metered behaviour, install location (per user or all users), language, theme.
- **Quality bar:**
  - light and dark from one Theme global, with a contrast check
  - keyboard and screen readers, with accessibility tests
  - translations bundled (en, de, es, fr, nl to start, the union of the family's)
  - window-only screenshots in docs; never full-screen captures

## 7. Crates

| Crate | Job |
|---|---|
| `suite-identity` | Product name, app ID, catalog URL |
| `suite-catalog` | Catalog types, parsing, the format's safety rules; the publishing builder lives in `suite-publish` |
| `suite-platform` | OS, arch, CPU features (`is_x86_feature_detected!`), glibc version, macOS/Windows versions, metered network, scheduler registration, running-process detection |
| `suite-fetch` | HTTPS download with resume, progress, SHA-256 while writing, proxy from environment/OS, timeouts and retries |
| `suite-unpack` | Safe extraction of `tar.gz`, `zip`; `dmg` via hdiutil; AppImage and Flatpak bundle handling |
| `suite-install` | Install, update, rollback and remove; per-OS integration (desktop files, `.lnk`, HKCU uninstall entries, LaunchAgents); state file (JSON, atomic writes, versioned) |
| `suite-detect` | Finding installs made by other means |
| `suite-core` | Orchestration: jobs, queue, policies, events for the UI and CLI |
| `suite-ui` | Slint app |
| `suite-cli` / `stockroom` (bin) | Arg parsing, the background mode, the `--ui` single-app install window |
| `suite-publish` | Builds `catalog.json` from GitHub releases plus `catalog/apps/*.toml` |
| `suite-testkit` | Fake release server (assets, `.sha256`, catalog), archive builders including malicious ones |

## 8. Testing

- **Unit tests** for catalog parsing and its safety rules, requirements matching, and the version ordering of releases.
- **Integration tests** against `suite-testkit`'s local server: install, update, rollback, remove; a checksum mismatch; truncated downloads that resume; an update while the "app" is running; a crash mid-install (the state stays consistent).
- **Malicious archives:** `../`, absolute paths, symlink escapes, bombs.
- **Fuzzing:** catalog JSON, tar, zip.
- **Tests never:**
  - touch the real home folder, registry, scheduler or Applications folder (everything goes through a root/prefix that tests point at temp dirs)
  - use the network beyond localhost
  - suspend or wake the machine
  - touch the real clipboard or keyring
- **Gates:** `cargo clippy --workspace --all-targets -- -D warnings` (pedantic), `cargo deny check`, `cargo fmt --check`, cross-target `cargo check` for Windows and macOS.

## 9. Milestones

1. **Core and CLI on Linux:** catalog plus verification, fetch, unpack, install, update, rollback and remove, detection, state, and `suite-publish` able to build a catalog for Specular's real releases. Tests.
2. **The UI:** home, app page, activity, settings, themes, translations, accessibility. *Done;
   not yet: catalog screenshots (the catalog has no field for them), installs for all users
   (the setting is there, disabled), SVG icons (PNG only).*
3. **Windows and macOS integration** (shortcuts, registry, LaunchAgents, dmg), cross-checked. Real runs where a machine is available.
   *Done, but not yet run on a real Windows PC or Mac: none was available. Everything was
   exercised on Linux through the fakes (in-memory registry, recording runner whose handlers
   emulate `hdiutil` and `ditto`), and `cargo clippy --all-targets -D warnings` passes for
   `x86_64-pc-windows-gnu`, `x86_64-pc-windows-msvc`, `x86_64-apple-darwin` and
   `aarch64-apple-darwin`; the Windows binaries also link with mingw. Not run: the real
   `WinRegistry`, `mslnk` output opened by Explorer, `schtasks`, `winget`, `hdiutil`, `ditto`,
   `launchctl`, `brew`.*
   - Windows: `current.txt` holds the version and the program; `suite-shim` (std only,
     ~340 KB) is copied beside it as `<app>.exe` and starts what it names, so the Start-menu
     `.lnk` (pure-Rust `mslnk`, MIT, no COM; inputs checked so its unwraps can't fire) keeps
     one target. HKCU uninstall entry with size. File associations only on request
     (`stockroom files <app> on`): a ProgID plus `OpenWithProgids` values under
     `HKCU\Software\Classes`, never the default handler; removed value by value, emptied keys
     tidied, others' entries kept. A shortcut that isn't ours is left alone.
     Open: the shim must ship next to `stockroom.exe` (bootstrap packages, milestone 4; without it
     the shortcut targets the version folder and is rewritten on each switch); no
     AppUserModelID on the shortcut (`mslnk` can't write property stores); Explorer isn't
     told about association changes (`SHChangeNotify` needs `unsafe`), it notices on its own.
   - macOS: dmg via `hdiutil attach -nobrowse -readonly -noautoopen`, `ditto --noqtn`, detach
     (always, also on failure); the bundle's `CFBundleIdentifier` must match the catalog;
     copies to `~/Applications`, or `/Applications` when installing for everyone (offered on
     macOS only; needs an admin account, checked with a probe file, no elevation prompt). A
     user's own copy is never replaced; the app then opens from its version folder.
     Homebrew casks found in both Caskroom prefixes, else via `brew list --cask --versions`.
     The LaunchAgent plist writer was already there (milestone 1) and is tested with `plist`.
   - Size (Linux x86-64, `stockroom` binary, measured 2026-11-16):

     | Build | Bytes | gzip -9 | xz -9 |
     |---|---|---|---|
     | `release` (`lto = "fat"`, 1 CGU, stripped, opt 3) | 30.4 MB | | |
     | **`dist`** (release + `opt-level = "s"`, no debug info) | **23.8 MB** | 11.3 MB | 8.4 MB |
     | `dist`, `opt-level = "z"` | 22.2 MB | 10.6 MB | 8.0 MB |
     | `dist` + `panic = "abort"` | 21.8 MB | 10.4 MB | 7.7 MB |
     | `dist`, software renderer only (`--no-default-features`) | 19.9 MB | 9.6 MB | 7.1 MB |
     | `suite-shim` (`dist`) | 0.34 MB | | |
     | Windows `stockroom.exe` (`dist`, x86_64-pc-windows-gnu) | 18.2 MB | 9.1 MB | 6.8 MB |
     | Windows `stockroom-shim.exe` (`dist`) | 0.33 MB | | |

     `dist` (`cargo build --profile dist`) is the shipping profile. "s" over "z": "z" saves
     another 7%, but its headless first frames measured 4.6–6.0 ms against 3.0–5.0 ms for
     "s" (within noise, but no gain). Unwinding is kept so a panicking job thread doesn't take
     the window with it. On Windows, `stockroom.exe` is a windowed program since 2026-11-20 and
     the command line goes through `stockroom.com` (milestone 4).
     The raw binaries are over the 15 MB goal (the earlier release build without LTO was
     36 MB); the download, compressed in the bootstrap package, is under it on both. The biggest parts
     (cargo-bloat, unoptimised .text): std, zbus (metered network, accessibility), the
     generated UI, naga + wgpu (~3 MB with wgpu-hal), AccessKit, winit, rustls. Dropping GPU
     rendering saves 3.9 MB; it stays on (the family's look and smooth scrolling), and is a
     cargo feature (`gpu`) should a size-first build be wanted.
   - Startup: process start (`stockroom --help`) 1.7 ms median in every build; the window's
     first frame in the headless test harness takes 3–6 ms of CPU at opt 3, "s" and "z"
     alike (noise-level differences). The full first frame on a display (~120 ms, mostly GPU
     device setup in the driver) was not re-measured, as no real window may be opened here;
     the code that runs there is the same apart from opt level.
   - **A real Mac (2026-11-19, Apple Silicon, macOS 27.0.1):** the test suite passes (the
     fixtures assumed an x86-64 Linux host; they now follow the host's processor, and the CLI
     tests pin `--os linux`). `stockroom --root` installed Specular 0.1.2's real 135 MB dmg in
     28 s: `hdiutil attach`, `ditto`, detach, the bundle ID check, the copy in
     `~/Applications`; the ad-hoc signature verifies afterwards, no quarantine flag, nothing
     left mounted; `remove` took all of it away. `ditto` clones on APFS there, so the copy
     in `~/Applications` takes no extra space. The window opens through Metal: first frame
     about 0.5 s after launch in a debug build. Not run for real: `launchctl` (the
     LaunchAgent is written under the root and not loaded), `/Applications` for everyone,
     Homebrew detection, a signed or notarised app.
4. **Scheduling, self-update, bootstrap packages,** and the delegation contract for the family's apps.
   *Partly done (2026-11-19):*
   - *Done:* the app list falls back to the saved copy or the built-in list (README, "The
     app list"); the list format tolerates additions; icons are published with the list and
     carried in the build; Dozed is listed (from its website, Linux); a copy named after an
     app opens that app's install window, which offers the other apps; Stockroom keeps a
     copy of itself for the scheduler and the uninstall entries; `stockroom uninstall`.
   - *Done (2026-11-20):* Stockroom's own icon (`packaging/icons`, drawn by `render.py`) in
     the window, the Windows `.exe`, the Mac app and the Linux menu entry, whose name is the
     window's app ID; `packaging/macos/build-app.sh` (an app bundle, `--for <app>`, ad hoc
     signed or with a Developer ID and notarised); `packaging/windows/build.sh`; `stockroom.exe`
     windowed with its icon, version and an `asInvoker` manifest (so a download named
     "...Installer.exe" isn't started as administrator), `stockroom.com` for the command line,
     and both it and the shim carried inside `stockroom.exe`; a Mac app run from a disk image
     or translocated by Gatekeeper keeps a copy like any download; the apps' uninstall
     entries name the kept copy even when it was made after Stockroom started. Not run on
     a real Windows PC, and the Mac app not with a Developer ID. The list holds the apps
     whose source is public on GitHub (Specular, Dozed and Pagebara). Its page is amazynk.com/stockroom.
     The catalog signature, its keys and its expiry were removed at the owner's request
     (§3); `suite-publish` writes the list straight into the site's `public/stockroom/`.
   - *Not done:* Stockroom updating itself through the catalog; signing for Windows and a
     Developer ID for macOS (the owner's accounts); `stockroom uninstall` run from the kept copy on Windows (a running
     program can't delete its own file there, so the copy would stay; untested).

## 10. Release placeholders (need the owner)

Set on 2026-11-20: the name Stockroom, the app ID `com.amazynk.Stockroom` and the catalog at `amazynk.com/stockroom/catalog.json`. Still open: publishing each family app's releases with `.sha256` files, and code-signing accounts for the bootstrap installers.
