//! `suite-publish`: build and sign the catalog.

use std::{io::Write, path::PathBuf, process::ExitCode};

use clap::Parser;
use suite_fetch::Fetcher;
use suite_publish::{Options, build, read_apps, write_catalog};

/// Builds catalog.json from GitHub releases and catalog/apps/*.toml, and checks every asset
/// against its .sha256.
#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    /// Folder with the apps' metadata.
    #[arg(long, default_value = "catalog/apps")]
    apps: PathBuf,
    /// Output folder for catalog.json and the icons.
    #[arg(long, short)]
    out: PathBuf,
    /// GitHub API base URL.
    #[arg(long, default_value = "https://api.github.com")]
    github_api: String,
    /// Only these apps (by id); others are left out of the catalog.
    #[arg(long = "only")]
    only: Vec<String>,
    /// Newest releases to include per app.
    #[arg(long, default_value_t = 3)]
    releases: usize,
    /// Catalog serial (default: now, in seconds).
    #[arg(long)]
    serial: Option<u64>,
    /// The address the icons in OUT/icons will be served from [default: icons/ next to the
    /// catalog's address].
    #[arg(long, value_name = "URL")]
    icon_base: Option<String>,
    /// The catalog.json published last time: release files it already lists with the same
    /// address, size and .sha256 are not downloaded again. A missing file is fine.
    #[arg(long, value_name = "FILE")]
    reuse: Option<PathBuf>,
    /// Also write the catalog here, as the list the next build of Stockroom carries inside
    /// for when it can't reach the server (catalog/builtin.json).
    #[arg(long, value_name = "FILE")]
    builtin: Option<PathBuf>,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("STOCKROOM_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    match run(&Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = &args.out;
    let mut apps = read_apps(&args.apps)?;
    if !args.only.is_empty() {
        apps.retain(|a| args.only.contains(&a.id));
    }
    let reuse = match &args.reuse {
        Some(file) if file.is_file() => Some(suite_catalog::Catalog::from_json(&std::fs::read(file)?)?),
        _ => None,
    };
    let opts = Options {
        icon_base: args.icon_base.clone().unwrap_or_else(suite_publish::default_icon_base),
        reuse,
        api: args.github_api.clone(),
        releases: args.releases,
        serial: args.serial,
    };
    let catalog = build(&apps, &Fetcher::default(), &opts, &mut |msg| tracing::info!("{msg}"))?;
    write_catalog(&catalog, out_dir)?;
    let icons = suite_publish::write_icons(&apps, out_dir)?;
    if let Some(file) = &args.builtin {
        std::fs::write(file, catalog.to_json_pretty()?)?;
    }
    let releases: usize = catalog.apps.iter().map(|a| a.releases.len()).sum();
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "wrote {} (serial {}, {} apps, {releases} releases)",
        out_dir.join("catalog.json").display(),
        catalog.version,
        catalog.apps.len()
    )?;
    if icons > 0 {
        writeln!(out, "wrote {icons} icons to {}", out_dir.join("icons").display())?;
    }
    if let Some(file) = &args.builtin {
        writeln!(out, "wrote the built-in list {}", file.display())?;
    }
    Ok(())
}
