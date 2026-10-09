//! Network Manager — Powered by Bashar Salmo.
//!
//! Manages the computer's network adapters (IP settings, DNS, MAC address),
//! shows the Wi-Fi network and saved Wi-Fi passwords, finds the devices on
//! the local network, and offers the everyday tools of network engineers.
//! Shared by the desktop app (`netmgr-gui`) and the command line (`netmgr`).

pub mod adapters;
pub mod backup;
pub mod capture;
pub mod cmd;
pub mod config;
pub mod console;
pub mod dhcp;
pub mod discovery;
pub mod dns;
pub mod extras;
pub mod icmp;
pub mod internet;
pub mod mac;
pub mod mdns;
pub mod monitor;
pub mod ntp;
pub mod plan;
pub mod profiles;
pub mod remote;
pub mod report;
pub mod scan;
pub mod servers;
pub mod snmp;
pub mod subnet;
pub mod system;
pub mod tools;
pub mod traffic;
pub mod web;
pub mod wifi;

pub const POWERED_BY: &str = "Powered by Bashar Salmo";
pub const REPO: &str = "https://github.com/itsmrroot/Network-Manager";
