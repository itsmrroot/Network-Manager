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

use crate::servers::{Slots, safe_path};

/// The longest request line or header accepted.
const MAX_LINE: u64 = 16 * 1024;

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
    let slots = Slots::default();
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((s, from)) => {
                let Some(slot) = slots.take() else { continue };
                let (opts, log, stop) = (opts.clone(), log.clone(), stop.clone());
                std::thread::spawn(move || {
                    let _slot = slot;
                    let _ = s.set_nonblocking(false);
                    let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
                    let _ = connection(s, from.ip(), &opts, &log, &stop);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
            // Out of file handles and the like: wait, and keep serving.
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
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
        411 => "Length Required",
        416 => "Range Not Satisfiable",
        500 => "Internal Server Error",
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

/// Percent-encodes `s` for a link, keeping `/` when `slash`.
fn url_encode(s: &str, slash: bool) -> String {
    s.bytes()
        .map(|c| {
            if c.is_ascii_alphanumeric() || b"-_.~".contains(&c) || (slash && c == b'/') {
                (c as char).to_string()
            } else {
                format!("%{c:02X}")
            }
        })
        .collect()
}

/// One line of the request, refusing endless ones.
fn read_line(reader: &mut BufReader<TcpStream>, line: &mut String) -> Result<usize> {
    let n = reader.take(MAX_LINE).read_line(line)?;
    if n as u64 == MAX_LINE && !line.ends_with('\n') {
        bail!("request line too long");
    }
    Ok(n)
}

/// A body sent in chunks (`Transfer-Encoding: chunked`, as `curl -T -`).
fn read_chunked(reader: &mut BufReader<TcpStream>, out: &mut impl Write) -> std::io::Result<u64> {
    let bad = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_string());
    let mut total = 0u64;
    loop {
        let mut line = String::new();
        read_line(reader, &mut line).map_err(|e| bad(&e.to_string()))?;
        let size = line.trim().split(';').next().unwrap_or("");
        let size = u64::from_str_radix(size, 16).map_err(|_| bad("damaged chunk"))?;
        if size == 0 {
            // Trailers, up to the empty line.
            for _ in 0..100 {
                let mut t = String::new();
                if read_line(reader, &mut t).map_err(|e| bad(&e.to_string()))? == 0 || t.trim().is_empty() {
                    break;
                }
            }
            return Ok(total);
        }
        let n = std::io::copy(&mut reader.take(size), out)?;
        if n != size {
            return Err(bad("the upload was interrupted"));
        }
        total += n;
        let mut crlf = String::new();
        read_line(reader, &mut crlf).map_err(|e| bad(&e.to_string()))?;
    }
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
    let link_base = url_encode(&base, true);
    let mut rows = String::new();
    if base != "/" {
        rows += "<tr><td><a href=\"../\">../</a></td><td></td></tr>\n";
    }
    for (name, is_dir, len) in entries {
        let shown = if is_dir { format!("{name}/") } else { name.clone() };
        let href = url_encode(&name, false);
        let size = if is_dir { String::new() } else { crate::adapters::format_bytes(len) };
        rows += &format!(
            "<tr><td><a href=\"{link_base}{href}{}\">{}</a></td><td>{size}</td></tr>\n",
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
    read_line(&mut reader, &mut line)?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("/").to_string());
    let mut length: Option<u64> = None;
    let mut chunked = false;
    let mut bad_length = false;
    let mut range: Option<(u64, Option<u64>)> = None;
    for _ in 0..100 {
        let mut h = String::new();
        if read_line(&mut reader, &mut h)? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            if k == "content-length" {
                length = v.parse().ok();
                bad_length = length.is_none();
            } else if k == "transfer-encoding" {
                chunked = v.to_ascii_lowercase().contains("chunked");
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
                Some((a, b)) if a < total && b.is_none_or(|b| b >= a) => {
                    (a, b.unwrap_or(total - 1).min(total - 1), 206)
                }
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
            if bad_length || (length.is_none() && !chunked) {
                respond(&mut s, 411, &[], b"Send the length of the file\n")?;
                done(411, 0);
                return Ok(());
            }
            // Into a hidden file first: an interrupted upload leaves nothing
            // behind, and never half a file under the real name.
            let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            static PART: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let tmp = file.with_file_name(format!(
                ".{name}.{}-{}.part",
                std::process::id(),
                PART.fetch_add(1, Ordering::Relaxed)
            ));
            let saved = (|| -> std::io::Result<u64> {
                if let Some(dir) = file.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let mut out = std::fs::File::create(&tmp)?;
                let n = match length {
                    _ if chunked => read_chunked(&mut reader, &mut out)?,
                    Some(len) => std::io::copy(&mut (&mut reader).take(len), &mut out)?,
                    None => 0,
                };
                if length.is_some_and(|len| n != len) {
                    return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the upload was interrupted"));
                }
                out.sync_all()?;
                std::fs::rename(&tmp, &file)?;
                Ok(n)
            })();
            match saved {
                Ok(n) => {
                    respond(&mut s, 201, &[], b"Saved\n")?;
                    done(201, n);
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    respond(&mut s, 500, &[], format!("Not saved: {e}\n").as_bytes())?;
                    done(500, 0);
                }
            }
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
        // The server may hang up first (a request it refuses).
        let _ = s.write_all(req.as_bytes());
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
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
        // Sent in chunks, as `curl -T -` does.
        let r = get(
            port,
            "PUT /chunked.cfg HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\nd\r\nhostname sw1\n\r\n0\r\n\r\n",
        );
        assert!(r.contains("201"), "{r}");
        assert_eq!(std::fs::read_to_string(dir.join("chunked.cfg")).unwrap(), "hostname sw1\n");
        // An upload cut short leaves nothing behind.
        {
            let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
            c.write_all(b"PUT /short.txt HTTP/1.1\r\nContent-Length: 100\r\n\r\nhello").unwrap();
        }
        std::thread::sleep(Duration::from_millis(300));
        assert!(!dir.join("short.txt").exists());
        assert!(
            std::fs::read_dir(&dir).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".part"))
        );
        // A range that ends before it starts.
        let r = get(port, "GET /images/c9300%20firmware.bin HTTP/1.1\r\nRange: bytes=8-2\r\n\r\n");
        assert!(r.contains("416"), "{r}");
        // An endless header is cut off instead of filling the memory.
        let r = get(port, &format!("GET / HTTP/1.1\r\nX: {}", "a".repeat(100_000)));
        assert!(!r.contains("200"), "{r}");
        // A folder name cannot break out of the link in the listing.
        std::fs::create_dir_all(dir.join("a\" onmouseover=\"x")).unwrap();
        let r = get(port, "GET /a%22%20onmouseover=%22x/ HTTP/1.1\r\n\r\n");
        assert!(r.contains("200 OK") && !r.contains("href=\"/a\""), "{r}");
        // A link inside the folder that leads out of it.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", dir.join("etc")).unwrap();
            assert!(get(port, "GET /etc/hosts HTTP/1.1\r\n\r\n").contains("403"));
        }
        assert!(get(port, "GET /images/ HTTP/1.1\r\n\r\n").contains("200 OK"));
        stop.store(true, Ordering::Relaxed);
        t.join().unwrap().unwrap();
        assert!(events.lock().unwrap().iter().any(|e| e.status == 206 && e.bytes == 6));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
