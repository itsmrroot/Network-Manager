# What's new

Each version has its own section below, written for the people who use the
app. The release process puts the section of a version on its release page,
and the app shows it in its update window. A version without a section here
cannot be released (the tests and the release check for it).

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
