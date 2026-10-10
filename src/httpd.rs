//! A small HTTP file server: switches and routers download firmware from
//! it much faster than over TFTP (`copy http://…`), and can upload their
//! configuration with PUT when uploads are allowed.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::servers::safe_path;

#[derive(Debug, Clone)]
pub struct Options {
    pub root: PathBuf,
    pub port: u16,
    /// The address to listen on: one adapter's, or 0.0.0.0 for every network.
    pub listen: Ipv4Addr,
    /// Accept PUT (and POST) uploads into the folder.
    pub allow_upload: bool,
    pub overwrite: bool,
}

/// What a client did, for the app's log: who, what, file, bytes, seconds.
#[derive(Debug, Clone)]
pub struct Event {
    pub from: IpAddr,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub bytes: u64,
    pub seconds: f64,
}

pub type Log = Arc<dyn Fn(Event) + Send + Sync>;

/// Serves `opts.root` until `stop` is set.
pub fn serve(opts: Options, stop: Arc<AtomicBool>, log: Log) -> Result<()> {
    std::fs::create_dir_all(&opts.root).with_context(|| format!("could not create {}", opts.root.display()))?;
    let listener = TcpListener::bind((opts.listen, opts.port)).map_err(|e| match e.kind() {
        std::io::ErrorKind::AddrInUse => {
            anyhow::anyhow!("TCP port {} is in use by another program: choose another, such as 8080", opts.port)
        }
        std::io::ErrorKind::PermissionDenied => {
            anyhow::anyhow!("port {} needs administrator rights on this system: use 8080", opts.port)
        }
        _ => anyhow::anyhow!("could not open TCP port {}: {e}", opts.port),
    })?;
    listener.set_nonblocking(true)?;
    let opts = Arc::new(opts);
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((s, from)) => {
                let (opts, log, stop) = (opts.clone(), log.clone(), stop.clone());
                std::thread::spawn(move || {
                    let _ = s.set_nonblocking(false);
                    let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
                    let _ = connection(s, from.ip(), &opts, &log, &stop);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => bail!("the server stopped: {e}"),
        }
    }
    Ok(())
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        206 => "Partial Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        416 => "Range Not Satisfiable",
        _ => "Error",
    }
}

fn respond(s: &mut TcpStream, code: u16, headers: &[(&str, String)], body: &[u8]) -> Result<()> {
    let mut head = format!("HTTP/1.1 {code} {}\r\nServer: Network Manager\r\nConnection: close\r\n", status_text(code));
    for (k, v) in headers {
        head += &format!("{k}: {v}\r\n");
    }
    if !headers.iter().any(|(k, _)| *k == "Content-Length") {
        head += &format!("Content-Length: {}\r\n", body.len());
    }
    head += "\r\n";
    s.write_all(head.as_bytes())?;
    s.write_all(body)?;
    Ok(())
}

/// `%20` and `+` in a path.
fn decode(p: &str) -> String {
    let b = p.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = p.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A page listing a folder.
pub fn listing(dir: &Path, url_path: &str) -> Result<String> {
    let mut entries: Vec<(String, bool, u64)> = std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let meta = e.metadata().ok()?;
            Some((name, meta.is_dir(), meta.len()))
        })
        .collect();
    entries.sort_by_key(|e| (!e.1, e.0.to_lowercase()));
    let base = if url_path.ends_with('/') { url_path.to_string() } else { format!("{url_path}/") };
    let mut rows = String::new();
    if base != "/" {
        rows += "<tr><td><a href=\"../\">../</a></td><td></td></tr>\n";
    }
    for (name, is_dir, len) in entries {
        let shown = if is_dir { format!("{name}/") } else { name.clone() };
        let href: String = name
            .bytes()
            .map(|c| {
                if c.is_ascii_alphanumeric() || b"-_.~".contains(&c) {
                    (c as char).to_string()
                } else {
                    format!("%{c:02X}")
                }
            })
            .collect();
        let size = if is_dir { String::new() } else { crate::adapters::format_bytes(len) };
        rows += &format!(
            "<tr><td><a href=\"{base}{href}{}\">{}</a></td><td>{size}</td></tr>\n",
            if is_dir { "/" } else { "" },
            html_escape(&shown)
        );
    }
    Ok(format!(
        "<!doctype html><meta charset=utf-8><title>{t}</title><style>body{{font:15px system-ui;margin:2em}}td{{padding:.2em 1.5em .2em 0}}</style><h1>{t}</h1><table>{rows}</table><p><small>Network Manager</small></p>",
        t = html_escape(url_path)
    ))
}

fn connection(mut s: TcpStream, from: IpAddr, opts: &Options, log: &Log, stop: &AtomicBool) -> Result<()> {
    let started = Instant::now();
    let mut reader = BufReader::new(s.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("/").to_string());
    let mut length: Option<u64> = None;
    let mut range: Option<(u64, Option<u64>)> = None;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            if k == "content-length" {
                length = v.parse().ok();
            } else if k == "range"
                && let Some(r) = v.strip_prefix("bytes=")
                && let Some((a, b)) = r.split_once('-')
                && let Ok(a) = a.parse::<u64>()
            {
                range = Some((a, b.parse().ok()));
            }
        }
    }
    let path = decode(target.split(['?', '#']).next().unwrap_or("/"));
    let done = |status: u16, bytes: u64| {
        log(Event {
            from,
            method: method.clone(),
            path: path.clone(),
            status,
            bytes,
            seconds: started.elapsed().as_secs_f64(),
        })
    };
    let rel = path.trim_start_matches('/');
    let file = if rel.is_empty() { Ok(opts.root.clone()) } else { safe_path(&opts.root, rel) };
    let Ok(file) = file else {
        respond(&mut s, 403, &[], b"Forbidden\n")?;
        done(403, 0);
        return Ok(());
    };
    match method.as_str() {
        "GET" | "HEAD" => {
            if file.is_dir() {
                let page = listing(&file, &path)?;
                respond(
                    &mut s,
                    200,
                    &[("Content-Type", "text/html; charset=utf-8".into())],
                    if method == "HEAD" { b"" } else { page.as_bytes() },
                )?;
                done(200, 0);
                return Ok(());
            }
            let Ok(mut f) = std::fs::File::open(&file) else {
                respond(&mut s, 404, &[], b"Not found\n")?;
                done(404, 0);
                return Ok(());
            };
            let total = f.metadata()?.len();
            // Resuming a download: send only the part asked for.
            let (start, end, code) = match range {
                Some((a, b)) if a < total => (a, b.unwrap_or(total - 1).min(total - 1), 206),
                Some(_) => {
                    respond(&mut s, 416, &[("Content-Range", format!("bytes */{total}"))], b"")?;
                    done(416, 0);
                    return Ok(());
                }
                None => (0, total.saturating_sub(1), 200),
            };
            let len = if total == 0 { 0 } else { end - start + 1 };
            let mut headers = vec![
                ("Content-Type", "application/octet-stream".to_string()),
                ("Content-Length", len.to_string()),
                ("Accept-Ranges", "bytes".to_string()),
            ];
            if code == 206 {
                headers.push(("Content-Range", format!("bytes {start}-{end}/{total}")));
            }
            respond(&mut s, code, &headers, b"")?;
            if method == "HEAD" {
                done(code, 0);
                return Ok(());
            }
            use std::io::Seek;
            f.seek(std::io::SeekFrom::Start(start))?;
            let mut left = len;
            let mut buf = vec![0u8; 256 * 1024];
            while left > 0 && !stop.load(Ordering::Relaxed) {
                let want = left.min(buf.len() as u64) as usize;
                let n = f.read(&mut buf[..want])?;
                if n == 0 {
                    break;
                }
                s.write_all(&buf[..n])?;
                left -= n as u64;
            }
            done(code, len - left);
        }
        "PUT" | "POST" => {
            if !opts.allow_upload {
                respond(&mut s, 403, &[], b"Uploads are turned off\n")?;
                done(403, 0);
                return Ok(());
            }
            if rel.is_empty() || file.is_dir() {
                respond(&mut s, 400, &[], b"Give a file name\n")?;
                done(400, 0);
                return Ok(());
            }
            if file.exists() && !opts.overwrite {
                respond(&mut s, 409, &[], b"The file exists\n")?;
                done(409, 0);
                return Ok(());
            }
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut out = std::fs::File::create(&file)?;
            let n = match length {
                Some(len) => std::io::copy(&mut (&mut reader).take(len), &mut out)?,
                None => std::io::copy(&mut reader, &mut out)?,
            };
            respond(&mut s, 201, &[], b"Saved\n")?;
            done(201, n);
        }
        _ => {
            respond(&mut s, 405, &[("Allow", "GET, HEAD, PUT".into())], b"")?;
            done(405, 0);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn get(port: u16, req: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn serves_lists_resumes_and_receives() {
        let dir = std::env::temp_dir().join(format!("netmgr-http-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("images")).unwrap();
        std::fs::write(dir.join("images/c9300 firmware.bin"), b"0123456789").unwrap();
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(Vec::new()));
        let (s2, e2) = (stop.clone(), events.clone());
        let opts =
            Options { root: dir.clone(), port, listen: Ipv4Addr::LOCALHOST, allow_upload: true, overwrite: false };
        let t = std::thread::spawn(move || serve(opts, s2, Arc::new(move |e| e2.lock().unwrap().push(e))));
        std::thread::sleep(Duration::from_millis(300));
        let r = get(port, "GET /images/ HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r.contains("200 OK") && r.contains("c9300%20firmware.bin"), "{r}");
        let r = get(port, "GET /images/c9300%20firmware.bin HTTP/1.1\r\n\r\n");
        assert!(r.ends_with("0123456789"));
        let r = get(port, "GET /images/c9300%20firmware.bin HTTP/1.1\r\nRange: bytes=4-\r\n\r\n");
        assert!(r.contains("206 Partial") && r.contains("bytes 4-9/10") && r.ends_with("456789"), "{r}");
        assert!(get(port, "GET /../etc/passwd HTTP/1.1\r\n\r\n").contains("403"));
        assert!(get(port, "GET /nope.bin HTTP/1.1\r\n\r\n").contains("404"));
        let r = get(port, "PUT /configs/sw1.cfg HTTP/1.1\r\nContent-Length: 13\r\n\r\nhostname sw1\n");
        assert!(r.contains("201"), "{r}");
        assert_eq!(std::fs::read_to_string(dir.join("configs/sw1.cfg")).unwrap(), "hostname sw1\n");
        assert!(get(port, "PUT /configs/sw1.cfg HTTP/1.1\r\nContent-Length: 1\r\n\r\nx").contains("409"));
        stop.store(true, Ordering::Relaxed);
        t.join().unwrap().unwrap();
        assert!(events.lock().unwrap().iter().any(|e| e.status == 206 && e.bytes == 6));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
