//! Saved network profiles ("Office", "Lab switch", "Home"): IP settings to
//! apply in one click. Kept in a JSON file that the desktop app and the
//! command line share.

use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::adapters::Adapter;
use crate::config::{self, IpSettings};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    /// The adapter it is usually applied to (its name); any if empty.
    #[serde(default)]
    pub adapter: String,
    pub settings: IpSettings,
    #[serde(default)]
    pub note: String,
    /// Proxy, default printer and network drives.
    #[serde(default)]
    pub extras: crate::extras::Extras,
}

impl Profile {
    pub fn apply(&self, a: &Adapter) -> Result<()> {
        config::apply(a, &self.settings)?;
        if !self.extras.is_empty() {
            self.extras.apply(a).context("the IP settings were applied, but not everything else")?;
        }
        Ok(())
    }
}

/// The folder for this program's files.
pub fn config_dir() -> PathBuf {
    let home =
        || std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default();
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(home).join("Network Manager")
    } else if cfg!(target_os = "macos") {
        home().join("Library/Application Support/Network Manager")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("netmgr")
    }
}

fn file() -> PathBuf {
    config_dir().join("profiles.json")
}

pub fn load() -> Result<Vec<Profile>> {
    let path = file();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)?;
    serde_json::from_str(&text).with_context(|| format!("{} could not be read", path.display()))
}

pub fn save(profiles: &[Profile]) -> Result<()> {
    let path = file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Written next to it first, so that a crash never leaves half a file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(profiles)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Adds `p`, or replaces the profile with the same name.
pub fn upsert(p: Profile) -> Result<()> {
    ensure!(!p.name.trim().is_empty(), "give the profile a name");
    p.settings.validate()?;
    let mut all = load()?;
    match all.iter_mut().find(|x| x.name.eq_ignore_ascii_case(&p.name)) {
        Some(x) => *x = p,
        None => all.push(p),
    }
    save(&all)
}

pub fn remove(name: &str) -> Result<bool> {
    let mut all = load()?;
    let before = all.len();
    all.retain(|p| !p.name.eq_ignore_ascii_case(name));
    save(&all)?;
    Ok(all.len() != before)
}

pub fn find(name: &str) -> Result<Profile> {
    let all = load()?;
    all.iter().find(|p| p.name.eq_ignore_ascii_case(name)).cloned().with_context(|| {
        let names: Vec<_> = all.iter().map(|p| p.name.as_str()).collect();
        if names.is_empty() {
            format!("no profile called \"{name}\" (there are no profiles yet)")
        } else {
            format!("no profile called \"{name}\" (profiles: {})", names.join(", "))
        }
    })
}
