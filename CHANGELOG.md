# What's new

Each version has its own section below, written for the people who use the
app. The release process puts the section of a version on its release page,
and the app shows it in its update window. A version without a section here
cannot be released (the tests and the release check for it).

## 0.9.27

### New
- **Connection history (Monitor):** every 5, 15, 30 or 60 minutes while the
  app is open, the app pings your router and the internet, times a name lookup
  and reads the Wi-Fi signal. A graph shows the last 24 hours, 7 or 30 days,
  with the typical and worst delay, how often the internet was reachable and a
  list of outages. It is off until you choose how often, and the results stay
  on your computer.
- **Slowdown alerts:** a notification when a watched host, or the internet in
  the connection history, suddenly answers much slower than usual, or the
  internet is gone while your router still answers. A switch "Slow
  connections" in Settings turns it off.
- **Network report as a web page or PDF:** besides Markdown, the report can be
  saved as an HTML page or a PDF; the name you give the file decides.
- **Export and import settings:** Settings can be saved to a file and loaded
  on another computer. Passwords and what belongs to one computer (its
  adapters, the devices it has seen) are left out.
- **Legal and support (About):** a list of the open-source software in the
  app with its licences, and a button that copies the version, your system and
  the end of the problem log for a bug report, with IP and MAC addresses and
  your user folder hidden first.
- **Search (Ctrl/Cmd+K)** also finds the connection history, exporting
  settings and the licences.

### Changed
- **Window size:** the app opens at the same size on Windows, macOS and Linux
  (as large as fits your screen with its title bar and the taskbar or Dock).
  A window you resized yourself keeps its size.

### Fixed
- **Updates:** an installer is only downloaded from the project's own
  releases page, whatever the update check answers.

## 0.9.26

### Fixed
- **macOS: Paste, Copy and Cut in a console window of its own:** the Edit menu
  and the Paste button did nothing once you pulled a console tab out into its
  own window. They now reach that window correctly.
- **Routes:** the card showing where your traffic goes now lists every
  network tied for the lowest metric, instead of picking just one. This
  matters most on macOS, which reports no metric at all.
- **Window size (macOS):** on a small screen, the app no longer sizes itself
  under the Dock.
- **Updates:** a downloaded update is kept for three days instead of one
  hour, so it is still there to install if you choose "Update later" and
  come back the next day.
- **Error dialogs:** pressing Enter now closes them, matching the highlighted
  OK button. It used to take a click.

## 0.9.25

### Fixed
- **Update check:** when GitHub allows no more checks from your network for the
  hour, "Check for updates" says so and tells you when to try again (for
  example "Try again after 13:17"). It used to say "could not reach GitHub:
  http status: 403", which sounded like a broken connection.

## 0.9.24

### Fixed
- **Installer:** the licence page of the Windows setup shows the licence as
  plain text. It used to show the Markdown marks (`#`, `<…>`, `[…](#…)` and
  `***`) between the words.

## 0.9.23

### New
- **Updates ask before they restart the app:** when an update is downloaded,
  you choose "Restart now" or "Update later". Nothing is installed, and the app
  does not close, until you restart. After "Update later" the button at the top
  reads "Restart to update", so you can finish whenever it suits you. This
  works the same on Windows, macOS and Linux.

## 0.9.22

### New
- **Menu bar (Windows and Linux):** File, Edit, View, Go, Network, Console and
  Help at the top, as in VS Code, with the keyboard shortcuts beside the
  entries (Ctrl+1 to 9 for the pages, Ctrl+K, F1, F11 for full screen). When
  the window is narrow, the last menus move under "···". The settings icon at
  the top is gone: Settings is in the File menu, under Ctrl+, and in the
  sidebar.

### Fixed
- **Opening the window:** it no longer shows up at the wrong size and then
  jumps. It opens in the place and state you left it in, maximized too (a
  maximized window used to come back smaller and partly off the screen), and is
  only centred the first time.
- **VPN on Windows:** Tailscale, WireGuard, OpenVPN and other VPN programs are
  recognised, so the VPN page, the top bar, the Overview and the Adapters page
  know a VPN is on. The route card shows where traffic really goes when a VPN
  also offers your own network, and names the adapter right after the start.
- **Tailscale:** devices have the names `tailscale status` shows.

## 0.9.21

### Fixed
- **Terminals:** the app no longer closes when a screen reader (Narrator,
  NVDA) is running while an SSH or Telnet session is open; the last lines and
  the prompt are no longer lost when the terminal gets shorter (a second
  session opens, a window is brought back), so typing no longer writes over
  earlier output.
- **VPN:** "Where your traffic goes" lists your local network again on
  Windows in German and other languages that write the gateway in two words.
- **Sidebar:** all pages fit on a 768-pixel screen, so Help and About no
  longer hide below the list; screen readers say the name of every page.
- **Settings:** on a narrow window the buttons no longer cover their
  descriptions; in Arabic the rows read from right to left.
- **Windows:** the Start menu and desktop shortcuts keep the chosen icon for
  every user of the computer; the installer an update downloaded is removed
  afterwards.
- **Small things:** Enter starts the MTU measurement, Esc closes the session
  window, the network report writes the Wi-Fi channel as "44 (5 GHz)", and
  the HTTP server log counts the size of folder listings.

## 0.9.20

### Fixed
- **SNMPv3:** an answer that is not signed or encrypted as the user is, is
  refused; an unsigned trap is only taken from a user set up without a
  signature; the trap key cache has a limit.
- **VPN:** the device scan and the connection check use your local network
  under a VPN; the routes card picks the route with the lowest metric and
  follows Refresh; the leak check tests the DNS servers of every adapter and
  no longer counts an unknown provider as safe; Tailscale, system VPN tools
  and DNS and SNMP lookups have time limits, so they cannot hang the page.
- **Terminals:** Undo, Redo and Select All go to the window that has the
  keyboard; terminal windows stay where they are; a failed Reconnect keeps
  the tab and its output; the main window can no longer stay hidden after a
  Dock menu item; Home, End, Page Up, Page Down and Insert work in the serial
  console.
- **Backups:** devices with look-alike names get their own folders, and two
  copies in the same second no longer overwrite each other. A push does not
  confirm questions of commands that erase or restart a device.
- **Privacy and limits:** the session, profile and backup files are readable
  only by you (macOS, Linux); uploads to the TFTP and HTTP servers stop at
  4 GB; the DHCP pool is at most 4096 addresses; a firewall rule for SSH is
  only removed when this app added it.
- Disconnect is no longer translated as "disable" in Arabic and Chinese.

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
