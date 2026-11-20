//! One window per user: the first `stockroom` holds a lock in its data folder and listens on a
//! local socket (a Unix socket in that folder, or a named pipe on Windows); a second one
//! hands its request over ("show", or "install <app>") and exits.

use std::{
    fs::{File, TryLockError},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, ListenerOptions, Name, Stream, prelude::*,
};
use suite_catalog::Channel;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Request {
    /// Bring the app window to the front.
    Show,
    /// `stockroom install <app> --ui`: the compact install window.
    Install { app: String, channel: Option<Channel> },
}

impl Request {
    pub(crate) fn encode(&self) -> String {
        match self {
            Self::Show => "show\n".into(),
            Self::Install { app, channel: None } => format!("install {app}\n"),
            Self::Install { app, channel: Some(c) } => format!("install {app} {c}\n"),
        }
    }

    pub(crate) fn decode(line: &str) -> Option<Self> {
        let mut words = line.split_whitespace();
        match (words.next()?, words.next(), words.next(), words.next()) {
            ("show", None, None, None) => Some(Self::Show),
            ("install", Some(app), channel, None) => {
                suite_catalog::model::validate_app_id(app).ok()?;
                let channel = match channel {
                    None => None,
                    Some(c) => Some(c.parse().ok()?),
                };
                Some(Self::Install { app: app.to_owned(), channel })
            }
            _ => None,
        }
    }
}

/// This process is the one: requests from later starts arrive at the handler.
#[derive(Debug)]
pub(crate) struct Primary {
    _lock: File,
}

#[derive(Debug)]
pub(crate) enum Start {
    Primary(Primary),
    /// Another window took the request.
    Handed,
}

/// Becomes the single instance for `data`, or hands `request` to the one already running.
/// `on` runs on a listener thread for every request that arrives later.
pub(crate) fn start(
    data: &Path,
    request: &Request,
    on: impl Fn(Request) + Send + 'static,
) -> io::Result<Start> {
    std::fs::create_dir_all(data)?;
    let lock = File::options().create(true).truncate(false).write(true).open(data.join("ui.lock"))?;
    match lock.try_lock() {
        Ok(()) => {
            listen(data, on)?;
            Ok(Start::Primary(Primary { _lock: lock }))
        }
        Err(TryLockError::WouldBlock) => {
            hand_over(data, request)?;
            Ok(Start::Handed)
        }
        Err(TryLockError::Error(e)) => Err(e),
    }
}

fn listen(data: &Path, on: impl Fn(Request) + Send + 'static) -> io::Result<()> {
    // The lock is ours, so a socket file left by a crash can go.
    let listener = ListenerOptions::new().name(name(data)?).try_overwrite(true).create_sync()?;
    std::thread::Builder::new().name("suite-instance".into()).spawn(move || {
        for conn in listener.incoming().filter_map(Result::ok) {
            let mut line = String::new();
            // One short line per connection; anything else is ignored.
            if BufReader::new(conn).take(256).read_line(&mut line).is_ok()
                && let Some(request) = Request::decode(&line)
            {
                on(request);
            }
        }
    })?;
    Ok(())
}

fn hand_over(data: &Path, request: &Request) -> io::Result<()> {
    // The first instance may still be starting its listener.
    let mut last = io::Error::other("no answer");
    for _ in 0..20 {
        match Stream::connect(name(data)?) {
            Ok(mut s) => return s.write_all(request.encode().as_bytes()),
            Err(e) => last = e,
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(last)
}

/// A socket in the (private) data folder; a named pipe named after it on Windows.
fn name(data: &Path) -> io::Result<Name<'static>> {
    if cfg!(windows) {
        let hash = data
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
        format!("{}-ui-{hash:016x}", suite_identity::SLUG).to_ns_name::<GenericNamespaced>()
    } else {
        socket_path(data).to_fs_name::<GenericFilePath>()
    }
}

/// Unix socket paths are limited to about 100 bytes; a deep `--root` falls back to the
/// runtime folder.
fn socket_path(data: &Path) -> PathBuf {
    let path = data.join("ui.sock");
    if path.as_os_str().len() < 100 {
        return path;
    }
    let hash =
        data.as_os_str().as_encoded_bytes().iter().fold(0_u32, |h, &b| h.wrapping_mul(31) ^ u32::from(b));
    let base = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    base.join(format!("{}-ui-{hash:08x}.sock", suite_identity::SLUG))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        for r in [
            Request::Show,
            Request::Install { app: "specular".into(), channel: None },
            Request::Install { app: "harbor".into(), channel: Some(Channel::Beta) },
        ] {
            assert_eq!(Request::decode(&r.encode()), Some(r));
        }
        assert_eq!(Request::decode("install ../x"), None);
        assert_eq!(Request::decode("install a nightly"), None);
        assert_eq!(Request::decode("rm -rf /"), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_second_start_hands_over() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let first = start(dir.path(), &Request::Show, move |r| drop(tx.send(r))).unwrap();
        assert!(matches!(first, Start::Primary(_)));
        let want = Request::Install { app: "bindery".into(), channel: None };
        let second = start(dir.path(), &want, |_| unreachable!()).unwrap();
        assert!(matches!(second, Start::Handed));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), want);
        // Once the first is gone, the next start is the primary again.
        drop(first);
        assert!(matches!(start(dir.path(), &Request::Show, |_| {}).unwrap(), Start::Primary(_)));
    }

    #[test]
    fn long_roots_still_get_a_socket() {
        let deep = PathBuf::from("/").join("x".repeat(120));
        assert!(socket_path(&deep).as_os_str().len() < 100);
        assert_eq!(socket_path(Path::new("/d")), Path::new("/d/ui.sock"));
    }
}
