//! A small HTTP/1.1 release server on 127.0.0.1 with `Range` support and injectable faults.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

/// A misbehaviour for the next N requests to a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// Send the full headers, then close the connection after this many body bytes.
    TruncateAfter(usize),
    /// Answer with this status and no body.
    Status(u16),
    /// Ignore `Range` and send the whole file with 200.
    IgnoreRange,
}

#[derive(Debug, Default)]
struct State {
    files: HashMap<String, Arc<Vec<u8>>>,
    faults: HashMap<String, Vec<Fault>>,
    /// Per path: the `Range` start of every request (`None`: no range).
    requests: HashMap<String, Vec<Option<u64>>>,
}

#[derive(Debug)]
pub struct Server {
    addr: std::net::SocketAddr,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Server {
    /// Starts on an ephemeral loopback port.
    pub fn start() -> Self {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) => panic!("binding the test server: {e}"),
        };
        let addr = listener.local_addr().unwrap_or_else(|e| panic!("{e}"));
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (st, sp) = (state.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            for conn in listener.incoming() {
                if sp.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                let st = st.clone();
                std::thread::spawn(move || {
                    let _ = handle(conn, &st);
                });
            }
        });
        Self { addr, state, stop, thread: Some(thread) }
    }

    /// `http://127.0.0.1:<port><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn put(&self, path: &str, bytes: impl Into<Vec<u8>>) {
        lock(&self.state).files.insert(path.to_owned(), Arc::new(bytes.into()));
    }

    pub fn remove(&self, path: &str) {
        lock(&self.state).files.remove(path);
    }

    /// Applies `fault` to the next `times` requests for `path`.
    pub fn fault(&self, path: &str, fault: Fault, times: usize) {
        lock(&self.state)
            .faults
            .entry(path.to_owned())
            .or_default()
            .extend(std::iter::repeat_n(fault, times));
    }

    pub fn hits(&self, path: &str) -> usize {
        lock(&self.state).requests.get(path).map_or(0, Vec::len)
    }

    /// The `Range` start of every request for `path`, in order.
    pub fn ranges_requested(&self, path: &str) -> Vec<Option<u64>> {
        lock(&self.state).requests.get(path).cloned().unwrap_or_default()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the accept loop.
        let _ = TcpStream::connect(self.addr);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn handle(conn: TcpStream, state: &Mutex<State>) -> std::io::Result<()> {
    let mut reader = BufReader::new(conn.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();
    let mut range: Option<u64> = None;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.eq_ignore_ascii_case("range")
        {
            range = v
                .trim()
                .strip_prefix("bytes=")
                .and_then(|r| r.split('-').next())
                .and_then(|s| s.parse().ok());
        }
    }
    let path = target.split('?').next().unwrap_or_default().to_owned();

    let (file, fault) = {
        let mut st = lock(state);
        st.requests.entry(path.clone()).or_default().push(range);
        let fault =
            st.faults.get_mut(&path).and_then(|f| if f.is_empty() { None } else { Some(f.remove(0)) });
        let file = st.files.get(&target).or_else(|| st.files.get(&path)).cloned();
        (file, fault)
    };
    let mut out = conn;
    if method != "GET" && method != "HEAD" {
        return respond(&mut out, 405, &[], b"");
    }
    if let Some(Fault::Status(code)) = fault {
        return respond(&mut out, code, &[], b"");
    }
    let Some(file) = file else {
        return respond(&mut out, 404, &[], b"not found");
    };
    let range = if fault == Some(Fault::IgnoreRange) { None } else { range };
    let (status, body, extra): (u16, &[u8], Vec<String>) = match range {
        Some(start) if start >= file.len() as u64 => {
            return respond(&mut out, 416, &[format!("Content-Range: bytes */{}", file.len())], b"");
        }
        Some(start) => {
            let s = usize::try_from(start).unwrap_or(usize::MAX);
            let cr = format!("Content-Range: bytes {start}-{}/{}", file.len() - 1, file.len());
            (206, &file[s..], vec![cr])
        }
        None => (200, &file[..], Vec::new()),
    };
    let body = if method == "HEAD" { &body[..0] } else { body };
    match fault {
        Some(Fault::TruncateAfter(n)) => {
            write_head(&mut out, status, &extra, body.len())?;
            out.write_all(&body[..n.min(body.len())])?;
            out.flush()?;
            out.shutdown(Shutdown::Both)
        }
        _ => respond(&mut out, status, &extra, body),
    }
}

fn write_head(out: &mut TcpStream, status: u16, extra: &[String], len: usize) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        404 => "Not Found",
        416 => "Range Not Satisfiable",
        _ => "Status",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n"
    );
    for h in extra {
        head.push_str(h);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    out.write_all(head.as_bytes())
}

fn respond(out: &mut TcpStream, status: u16, extra: &[String], body: &[u8]) -> std::io::Result<()> {
    write_head(out, status, extra, body.len())?;
    out.write_all(body)?;
    out.flush()
}
