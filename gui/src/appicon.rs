//! The chosen app icon outside the app, also when it is closed: the app
//! itself on macOS (Dock, Finder, Launchpad), its shortcuts on Windows
//! (Start menu, desktop, task bar), the user's icon theme on Linux. The
//! blue icon is the app's own: choosing it removes the change.

use crate::theme::AppIcon;

/// Makes `icon` the app's icon from now on. Failures are ignored: the app
/// keeps working, only the icon outside it stays as it was.
pub fn keep(icon: AppIcon) {
    #[cfg(target_os = "macos")]
    mac(icon);
    #[cfg(windows)]
    std::thread::spawn(move || windows(icon));
    #[cfg(target_os = "linux")]
    linux(icon);
}

/// macOS: a custom icon on the .app, as Finder's "Get Info" sets it.
#[cfg(target_os = "macos")]
fn mac(icon: AppIcon) {
    use objc2::AnyThread;
    use objc2_app_kit::{NSImage, NSWorkspace, NSWorkspaceIconCreationOptions};
    use objc2_foundation::{NSData, NSString};
    // …/Network Manager.app/Contents/MacOS/netmgr-gui
    let Some(app) = std::env::current_exe().ok().and_then(|e| e.ancestors().nth(3).map(|p| p.to_path_buf())) else {
        return;
    };
    if app.extension().is_none_or(|e| e != "app") {
        return;
    }
    let path = NSString::from_str(&app.display().to_string());
    let ws = NSWorkspace::sharedWorkspace();
    let image = (icon != AppIcon::Blue)
        .then(|| NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(icon.png())))
        .flatten();
    // No image: back to the app's own icon.
    ws.setIcon_forFile_options(image.as_deref(), &path, NSWorkspaceIconCreationOptions::empty());
}

/// Windows: the app's shortcuts point at an icon file of the chosen icon.
#[cfg(windows)]
fn windows(icon: AppIcon) {
    let Ok(exe) = std::env::current_exe() else { return };
    let location = if icon == AppIcon::Blue {
        format!("{},0", exe.display())
    } else {
        let Some(dir) = std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join("Network Manager"))
        else {
            return;
        };
        let file = dir.join(format!("icon-{}.ico", icon as u8));
        let Some(png) = icon.image(256).and_then(|i| {
            let mut out = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(i).write_to(&mut out, image::ImageFormat::Png).ok()?;
            Some(out.into_inner())
        }) else {
            return;
        };
        if std::fs::create_dir_all(&dir).is_err() || std::fs::write(&file, ico(&png)).is_err() {
            return;
        }
        format!("{},0", file.display())
    };
    let script = format!(
        "$exe = {exe}; $icon = {icon}; $shell = New-Object -ComObject WScript.Shell
$places = @(
  [Environment]::GetFolderPath('CommonPrograms'), [Environment]::GetFolderPath('Programs'),
  [Environment]::GetFolderPath('CommonDesktopDirectory'), [Environment]::GetFolderPath('DesktopDirectory'),
  (Join-Path $env:APPDATA 'Microsoft\\Internet Explorer\\Quick Launch\\User Pinned\\TaskBar'))
foreach ($p in $places) {{
  if (-not $p -or -not (Test-Path $p)) {{ continue }}
  Get-ChildItem -Path $p -Filter *.lnk -Recurse -Depth 2 -ErrorAction SilentlyContinue | ForEach-Object {{
    $s = $shell.CreateShortcut($_.FullName)
    if ($s.TargetPath -ieq $exe) {{ $s.IconLocation = $icon; $s.Save() }}
  }}
}}
ie4uinit.exe -show",
        exe = netmgr::cmd::ps_quote(&exe.display().to_string()),
        icon = netmgr::cmd::ps_quote(&location),
    );
    let _ = netmgr::cmd::powershell(&script);
}

/// An .ico holding one 256-pixel PNG (Windows Vista and later read these).
#[cfg_attr(not(windows), allow(dead_code))]
fn ico(png: &[u8]) -> Vec<u8> {
    let mut out = vec![0, 0, 1, 0, 1, 0];
    // 0 × 0 means 256 × 256; 32 bits per pixel; size; data right after.
    out.extend_from_slice(&[0, 0, 0, 0, 1, 0, 32, 0]);
    out.extend_from_slice(&(png.len() as u32).to_le_bytes());
    out.extend_from_slice(&22u32.to_le_bytes());
    out.extend_from_slice(png);
    out
}

/// Linux: the user's icon theme has its own "netmgr" icon, which the menus
/// and docks prefer to the one installed for everyone.
#[cfg(target_os = "linux")]
fn linux(icon: AppIcon) {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else { return };
    let dir = home.join(".local/share/icons/hicolor/256x256/apps");
    // The .deb names the icon after the program, the .rpm after the package.
    for name in ["netmgr.png", "netmgr-gui.png"] {
        let file = dir.join(name);
        if icon == AppIcon::Blue {
            let _ = std::fs::remove_file(&file);
        } else if let Some(i) = icon.image(256) {
            let _ = std::fs::create_dir_all(&dir);
            let _ = i.save(&file);
        }
    }
    // Menus notice the change sooner.
    let _ = netmgr::cmd::output(
        "gtk-update-icon-cache",
        &["-f", "-t", &home.join(".local/share/icons/hicolor").display().to_string()],
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn ico_header() {
        let i = super::ico(&[1, 2, 3]);
        assert_eq!(&i[..6], &[0, 0, 1, 0, 1, 0]);
        assert_eq!(u32::from_le_bytes(i[14..18].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(i[18..22].try_into().unwrap()), 22);
        assert_eq!(&i[22..], &[1, 2, 3]);
    }
}
