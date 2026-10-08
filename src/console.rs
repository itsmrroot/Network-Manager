//! Console access: serial console cables (switches, routers, firewalls),
//! a small terminal screen for their output, and opening SSH or Telnet in
//! the system's terminal.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct PortInfo {
    pub name: String,
    /// "USB serial (FTDI FT232R)", "Bluetooth", …
    pub description: String,
}

/// The serial ports of this computer; USB console cables first.
pub fn ports() -> Vec<PortInfo> {
    let mut out: Vec<PortInfo> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        // macOS lists each port twice (tty. and cu.): cu. is the one to open.
        .filter(|p| !p.port_name.starts_with("/dev/tty.") || !cfg!(target_os = "macos"))
        .map(|p| {
            let description = match p.port_type {
                serialport::SerialPortType::UsbPort(u) => {
                    let what = [u.manufacturer, u.product].into_iter().flatten().collect::<Vec<_>>().join(" ");
                    if what.is_empty() {
                        format!("USB serial ({:04x}:{:04x})", u.vid, u.pid)
                    } else {
                        format!("USB serial ({what})")
                    }
                }
                serialport::SerialPortType::BluetoothPort => "Bluetooth".into(),
                serialport::SerialPortType::PciPort => "Built-in".into(),
                serialport::SerialPortType::Unknown => "Serial port".into(),
            };
            PortInfo { name: p.port_name, description }
        })
        .collect();
    out.sort_by_key(|p| (!p.description.starts_with("USB"), p.name.clone()));
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Parity {
    None,
    Odd,
    Even,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Flow {
    None,
    Software,
    Hardware,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineSettings {
    pub baud: u32,
    pub data_bits: u8,
    pub parity: Parity,
    pub stop_bits: u8,
    pub flow: Flow,
}

impl Default for LineSettings {
    /// 9600 8N1, no flow control: the console default of nearly every
    /// switch and router.
    fn default() -> Self {
        Self { baud: 9600, data_bits: 8, parity: Parity::None, stop_bits: 1, flow: Flow::None }
    }
}

pub const BAUD_RATES: [u32; 8] = [1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200];

/// An open serial console.
pub struct Session {
    port: Mutex<Box<dyn serialport::SerialPort>>,
    pub received: Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
    pub name: String,
}

impl Session {
    pub fn open(name: &str, s: LineSettings) -> Result<Session> {
        let port = serialport::new(name, s.baud)
            .data_bits(match s.data_bits {
                5 => serialport::DataBits::Five,
                6 => serialport::DataBits::Six,
                7 => serialport::DataBits::Seven,
                _ => serialport::DataBits::Eight,
            })
            .parity(match s.parity {
                Parity::None => serialport::Parity::None,
                Parity::Odd => serialport::Parity::Odd,
                Parity::Even => serialport::Parity::Even,
            })
            .stop_bits(if s.stop_bits == 2 { serialport::StopBits::Two } else { serialport::StopBits::One })
            .flow_control(match s.flow {
                Flow::None => serialport::FlowControl::None,
                Flow::Software => serialport::FlowControl::Software,
                Flow::Hardware => serialport::FlowControl::Hardware,
            })
            .timeout(Duration::from_millis(100))
            .open()
            .map_err(|e| {
                let hint = if cfg!(target_os = "linux") && e.to_string().contains("ermission") {
                    " — add yourself to the \"dialout\" group (sudo usermod -aG dialout $USER), then log out and in"
                } else {
                    ""
                };
                anyhow::anyhow!("{name} could not be opened: {e}{hint}")
            })?;
        let mut reader = port.try_clone().context("the port could not be read")?;
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let s2 = stop.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while !s2.load(Ordering::Relaxed) {
                match reader.read(&mut buf) {
                    Ok(0) => {}
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(_) => break,
                }
            }
        });
        Ok(Session { port: Mutex::new(port), received: rx, stop, name: name.to_string() })
    }

    pub fn send(&self, bytes: &[u8]) -> Result<()> {
        let mut p = self.port.lock().map_err(|_| anyhow::anyhow!("the port is busy"))?;
        p.write_all(bytes)?;
        p.flush()?;
        Ok(())
    }

    /// A serial break (for password recovery on Cisco and others).
    pub fn send_break(&self) -> Result<()> {
        let p = self.port.lock().map_err(|_| anyhow::anyhow!("the port is busy"))?;
        p.set_break()?;
        std::thread::sleep(Duration::from_millis(500));
        p.clear_break()?;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A small terminal screen: keeps lines of text and understands carriage
/// returns, backspaces, tabs and the ANSI sequences consoles use to erase
/// ("--More--" prompts) and colour text.
#[derive(Debug, Default, Clone)]
pub struct Screen {
    pub lines: Vec<String>,
    col: usize,
    escape: Option<String>,
}

/// Lines kept on the screen.
const SCROLLBACK: usize = 5000;

impl Screen {
    fn line(&mut self) -> &mut String {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.lines.last_mut().unwrap_or_else(|| unreachable!())
    }

    fn put(&mut self, c: char) {
        let col = self.col;
        let line = self.line();
        let len = line.chars().count();
        if col < len {
            let mut chars: Vec<char> = line.chars().collect();
            chars[col] = c;
            *line = chars.into_iter().collect();
        } else {
            line.extend(std::iter::repeat_n(' ', col - len));
            line.push(c);
        }
        self.col += 1;
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for c in String::from_utf8_lossy(bytes).chars() {
            if let Some(seq) = self.escape.as_mut() {
                seq.push(c);
                let done = seq.len() > 1 && (seq.starts_with('[') && ('@'..='~').contains(&c) || !seq.starts_with('['));
                if done || seq.len() > 32 {
                    let seq = self.escape.take().unwrap_or_default();
                    self.apply(&seq);
                }
                continue;
            }
            match c {
                '\x1b' => self.escape = Some(String::new()),
                '\r' => self.col = 0,
                '\n' => {
                    self.lines.push(String::new());
                    self.col = 0;
                    if self.lines.len() > SCROLLBACK {
                        self.lines.drain(..self.lines.len() - SCROLLBACK);
                    }
                }
                '\x08' => self.col = self.col.saturating_sub(1),
                '\t' => {
                    let to = (self.col / 8 + 1) * 8;
                    while self.col < to {
                        self.put(' ');
                    }
                }
                '\x07' | '\0' => {}
                c if c.is_control() => {}
                c => self.put(c),
            }
        }
    }

    fn apply(&mut self, seq: &str) {
        let Some(body) = seq.strip_prefix('[') else { return };
        let (args, cmd) = body.split_at(body.len().saturating_sub(1));
        let n = args.trim_start_matches('?').parse::<usize>().unwrap_or(0);
        match cmd {
            // Erase in line: from the cursor (0), to the cursor (1), all (2).
            "K" => {
                let col = self.col;
                let line = self.line();
                let chars: Vec<char> = line.chars().collect();
                *line = match n {
                    1 => {
                        std::iter::repeat_n(' ', col.min(chars.len())).chain(chars.iter().skip(col).copied()).collect()
                    }
                    2 => String::new(),
                    _ => chars.into_iter().take(col).collect(),
                };
            }
            "J" if n == 2 => {
                self.lines.clear();
                self.col = 0;
            }
            "D" => self.col = self.col.saturating_sub(n.max(1)),
            "C" => self.col += n.max(1),
            "G" => self.col = n.saturating_sub(1),
            _ => {}
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

/// Opens the system's terminal with `ssh user@host` (or `telnet host`).
pub fn open_terminal(command: &str) -> Result<()> {
    if command.chars().any(|c| matches!(c, ';' | '&' | '|' | '`' | '$' | '>' | '<' | '\n' | '"' | '\'')) {
        bail!("the address contains characters that are not allowed");
    }
    if cfg!(windows) {
        crate::cmd::command("cmd").args(["/c", "start", "", "cmd", "/k", command]).spawn()?;
    } else if cfg!(target_os = "macos") {
        crate::cmd::command("osascript")
            .args([
                "-e",
                &format!("tell application \"Terminal\" to do script \"{command}\""),
                "-e",
                "tell application \"Terminal\" to activate",
            ])
            .spawn()?;
    } else {
        let parts: Vec<&str> = command.split_whitespace().collect();
        let terminals: [(&str, &[&str]); 5] = [
            ("x-terminal-emulator", &["-e"]),
            ("gnome-terminal", &["--"]),
            ("konsole", &["-e"]),
            ("xfce4-terminal", &["-x"]),
            ("xterm", &["-e"]),
        ];
        let (prog, flag) =
            terminals.iter().find(|(t, _)| crate::cmd::exists(t)).context("no terminal program was found")?;
        crate::cmd::command(prog).args(*flag).args(&parts).spawn()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_handles_console_output() {
        let mut s = Screen::default();
        s.feed(b"Switch>en\r\nPassword: \r\n");
        assert_eq!(s.lines, vec!["Switch>en", "Password: ", ""]);
        // Cisco "--More--", erased with backspaces and spaces.
        let mut s = Screen::default();
        s.feed(b" --More-- \x08\x08\x08\x08\x08\x08\x08\x08\x08\x08          \x08\x08\x08\x08\x08\x08\x08\x08\x08\x08Gi0/1  up");
        assert_eq!(s.lines[0].trim_end(), "Gi0/1  up");
        // ANSI erase-line and colours.
        let mut s = Screen::default();
        s.feed(b"\x1b[1;32mOK\x1b[0m line\r\x1b[KNew");
        assert_eq!(s.lines, vec!["New"]);
        // Tabs.
        let mut s = Screen::default();
        s.feed(b"a\tb");
        assert_eq!(s.lines[0], "a       b");
    }

    #[test]
    fn terminal_commands_are_checked() {
        assert!(open_terminal("ssh admin@10.0.0.1; rm -rf ~").is_err());
    }
}
