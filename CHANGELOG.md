# What's new

Each version has its own section below, written for the people who use the
app. The release process puts the section of a version on its release page,
and the app shows it in its update window. A version without a section here
cannot be released (the tests and the release check for it).

## 0.9.19

### New
- **A VPN page** in the sidebar. It shows whether a VPN is on, whether it
  carries all your traffic, what websites see (your address, place and
  provider), and a **leak check** for your traffic, IPv6 and DNS lookups.
- **Where your traffic goes, in plain words:** what goes through the VPN, what
  is reached directly, and what "Allow local network access" means.
- **Tailscale:** your devices, the exit node (change it with one click),
  Allow local network access, connect and disconnect, and an SSH button for
  each device.
- **VPN connections of the system** (macOS, Linux, Windows): connect and
  disconnect them.
- **A VPN badge in the top bar**, and the VPN tunnel on the Adapters page. The
  address shown at the top is your local network's, not the tunnel's, and
  scans and IP settings keep working on your local network while a VPN is on.

### Changed
- Cmd+4 now opens the VPN page; Devices and the pages after it moved one
  digit down.

## 0.9.18

### Changed
- **New license: PolyForm Noncommercial 1.0.0.** The app stays free to download
  and use, and its code stays public. Personal and other non-commercial use is
  allowed; selling it or using it in a commercial product needs written
  permission. Versions up to 0.9.17 were released under the MIT License and
  stay under it for those who received them.

## 0.9.17

### Fixed
- The Windows download is back. Version 0.9.16 came out without its Windows
  files; this version has everything 0.9.16 has.

## 0.9.16

### New
- **Hide the list of pages on the left** for more room. Use the button at the
  top left, View → "Hide or show sidebar", or Cmd+\ (Ctrl+\ on Windows and
  Linux). The app remembers your choice.

## 0.9.15

### Fixed
- Cmd+V (and any other menu item) in a terminal window of its own no longer
  brings the main window up.

## 0.9.14

### Fixed
- A session opened from the Dock menu while the app is minimised now gets a
  terminal window with its own title bar and buttons, and Bring back and Paste
  work in it.
- On the Mac, windows of the app are no longer merged into tabs.

## 0.9.13

### New
- **Drag a saved session onto a group** to move it there; drop it on "No group"
  to take it out of its group.
- **Drag a tab out of the row of tabs** to open that session in a window of its
  own. In the window, drag the address at the top out of the window to bring it
  back.
- **The height of the terminal follows the mouse.** Drag the line under the
  terminal; double-click it to fill the page again.

## 0.9.12

### Fixed
- Choosing a saved session in the Dock menu while the app is minimised or
  hidden opens only the terminal window; the main window stays away.

## 0.9.11

### Fixed
- Choosing a session or any other item in the Dock menu while the app is
  minimised or hidden now shows the app and opens the window at once.

## 0.9.10

### New
- **Saved sessions from a menu open in a terminal window of their own.** Pick
  one in the Dock menu or the Console menu; "Bring back" puts it in the app.

## 0.9.9

### New
- **The update window shows what is new.** It lists the notes of the new
  version (the first update that shows them is the one after this one), with a
  link to the full release page.
- **Docker, Podman and virtual machine icons** for saved sessions.
- **The Dock menu has a symbol before each item,** and before each saved
  session, matching the icon you chose for it.

## 0.9.8

### New
- **Colours in the terminals.** Addresses, MAC addresses, interface names and
  words like up, down and error get a colour in SSH, Telnet and Android
  sessions. A Highlight button turns it off.
- **Find in the terminal.** Press Cmd+F (Ctrl+Shift+F on Windows and Linux) to
  search the screen and everything that scrolled by.
- **Session logs.** "Log to file…" writes an SSH or Telnet session as clean
  text, with the time of each line.
- **Send to all sessions.** One command for every open session, after a
  second click to confirm.
- **Terminals in their own windows.** SSH sessions and the serial console can
  open in a window of their own, for another screen.
- **Icons for saved sessions:** router, switch, server, firewall and more.
- **Menus on the Mac.** Every menu item has an icon. A new Console menu opens
  your saved sessions, and the Network menu has "Change IP settings…". The
  Dock icon has a menu too: check the connection, renew the IP address, change
  the IP settings, scan, open a saved session.

### Fixed
- Cmd+V in a terminal window of its own pasted into the main window.
- Opening a session to a host that does not answer no longer freezes the app.
- All 14 session icons can be chosen, and tabs keep the active session when
  another one is closed.

## 0.9.7

### New
- **Select text in the terminals with the mouse.** Drag, double-click a word or
  triple-click a line; it is copied when you let go.
- **Resize the terminal area.** Drag the border between the saved sessions and
  the terminal, and use Maximize to give the terminal the whole page.

### Fixed
- Android: the text filter of the log now works, adb calls can no longer hang
  the app, restarting the phone asks twice, and the shell and the log keep
  being read when you look at another page.
- Terminals: long lines that wrap copy as one line, Chinese words are selected
  whole, Paste and Ctrl+C work in the serial console, and more keys work
  (F5–F12, Shift+Tab, Ctrl+arrows).

## 0.9.6

### Fixed
- The CLI guide folds away in a small window instead of being cut off, and
  closes with Cmd+W.
- The serial port list forgets a port that was unplugged.
- A wrong subnet shows a red warning; the router picture in the CLI guide
  matches its commands.

## 0.9.5

### Fixed
- The CLI guide next to the console keeps the width you drag it to.

## 0.9.4

### New
- **Android (ADB)** in the Console: a shell, the log (logcat) with filters, wireless
  pairing, screenshots, installing apps and moving files.
- **Pictures in the CLI guide:** the console cable, port names, trunks,
  spanning tree, router on a stick, the layers and subnet bits.

### Fixed
- The CLI guide no longer takes all the room from the console in a split screen.

## 0.9.3

### New
- A CLI guide for switches inside the Console (Cisco, Juniper, Aruba, Arista,
  MikroTik and a CCNA study map), next to the console or in its own window.
- Typing `24` in the subnet field turns it into `255.255.255.0`.

### Fixed
- Pressing Enter in the serial console no longer jumps to another page.
