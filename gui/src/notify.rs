//! Desktop notifications, with each system's own tools: no extra library,
//! and they appear like any other notification.

/// Shows `title` and `body` in the background; failures are ignored (a
/// missed notification must never stop the app).
pub fn send(title: &str, body: &str) {
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        let _ = show(&title, &body);
    });
}

fn show(title: &str, body: &str) -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        // AppleScript strings: backslashes and quotes escaped.
        let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
        netmgr::cmd::run("osascript", &["-e", &format!("display notification {} with title {}", q(body), q(title))])?;
    } else if cfg!(windows) {
        // A toast through PowerShell's registered app ID, which Windows shows
        // without an installed shortcut.
        let x = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let script = format!(
            "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null; \
             [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] | Out-Null; \
             $xml = New-Object Windows.Data.Xml.Dom.XmlDocument; \
             $xml.LoadXml({}); \
             $toast = [Windows.UI.Notifications.ToastNotification]::new($xml); \
             [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}}\\WindowsPowerShell\\v1.0\\powershell.exe').Show($toast)",
            netmgr::cmd::ps_quote(&format!(
                "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
                x(title),
                x(body)
            ))
        );
        netmgr::cmd::powershell(&script)?;
    } else {
        netmgr::cmd::run("notify-send", &["--app-name=Network Manager", title, body])?;
    }
    Ok(())
}
