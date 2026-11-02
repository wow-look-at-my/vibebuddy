//! The daemon's status snapshot, read only as far as the app needs it, and the tray menu derived from it.
//! Mirrors `MenuState.swift` so both apps say the same thing.

use serde::{Deserialize, Serialize};

use crate::i18n::tr;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Status {
    pub daemon: DaemonInfo,
    pub device: Device,
    pub today: Today,
    pub hooks: Hooks,
    pub operation: Option<Operation>,
    pub config: Config,
    /// Every Activity, newest first, as the box's task cards show them; empty from older daemons.
    pub activities: Vec<Activity>,
    /// Whether the user's own key for spoken replies is set (ADR-0020); absent from older daemons.
    pub voice_keys: Option<VoiceKeys>,
    /// The Character being made, or the last one made since the daemon started (ADR-0020); absent when there is none,
    /// and from older daemons.
    pub making: Option<crate::making::Making>,
}

/// The daemon says only whether the key is set, and whether ElevenLabs refused it; a key never comes back.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct VoiceKeys {
    pub elevenlabs: VoiceKey,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct VoiceKey {
    pub set: bool,
    pub refused: bool,
}

/// One Activity as the daemon summarises it (daemon/src/activity.rs `ActivitySummary`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Activity {
    /// `codex`, `claude-code`, `github-actions`, or the agent a CLI hook names (`opencode`, `copilot`, `pi`).
    pub agent: String,
    /// The task card's title, with the prefix that tells agents apart (`CC:`, `CX:`…).
    pub title: String,
    pub project: Option<String>,
    /// `working` or `needs_input`.
    pub state: String,
}

/// Writing a voice pack or flashing firmware; the daemon runs one at a time and reports progress here.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Operation {
    pub kind: OperationKind,
    pub state: OperationState,
    /// 0 to 1.
    pub progress: f32,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    VoicePack,
    Firmware,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Running,
    Done,
    Failed,
    /// Firmware written, but the box didn't start it; it needs unplugging and plugging back in.
    Replug,
}

impl Operation {
    pub fn running(&self) -> bool {
        self.state == OperationState::Running
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct DaemonInfo {
    pub build: String,
    /// The git description alone; empty from a daemon older than the field.
    pub revision: String,
}

impl DaemonInfo {
    /// What the settings window shows: the revision, or the whole build line from an older daemon.
    pub fn short_build(&self) -> &str {
        if self.revision.is_empty() {
            &self.build
        } else {
            &self.revision
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Device {
    pub connected: bool,
    pub port: Option<String>,
    pub bridge: bool,
    pub mode: Option<String>,
    pub firmware_build: Option<String>,
    /// Firmware older than ADR-0010 and older daemons report none.
    pub firmware_version: Option<String>,
    pub voice: Option<String>,
    pub volume: Option<u8>,
    /// Which set of screens the box draws, `robot` or `korekore` (ADR-0021); None from firmware that predates the choice.
    pub screens: Option<String>,
    /// Linked over the local network; `port` is then the box's address.
    pub network: bool,
    /// Another computer has the box (ADR-0011).
    pub busy: Option<Busy>,
    /// Connected but running other firmware, as the daemon judged it (docs/architecture.md, decision 17).
    pub foreign_firmware: bool,
    /// Our firmware built for other hardware (the breadboard devkit).
    pub unsupported_board: Option<String>,
    /// An environment variable narrowing the search to one port or device; older daemons report none.
    pub pin: Option<Pin>,
    /// Devices that could be the box when there are several and none is the remembered one.
    pub candidates: Vec<Candidate>,
    /// The box's key (ADR-0012); none for firmware that can't pair, or from an older daemon.
    pub box_key: Option<String>,
    /// The firmware takes updates over any link (ADR-0016): no cable, no download mode.
    pub transfer: bool,
    /// The last image sent over the link didn't confirm itself, and the box went back (`FIRMWARE ROLLED BACK`).
    pub rolled_back: bool,
    /// This computer's key, to tell it apart in `paired_computers`.
    pub computer_key: Option<String>,
    pub paired_computers: Vec<PairedComputer>,
    /// The Wi-Fi network the box is set to join, and the address it got there.
    pub wifi_network: Option<String>,
    pub wifi_address: Option<String>,
    /// Why the box couldn't join the network, until it does.
    pub wifi_error: Option<String>,
    /// What the box heard on its last scan, strongest first.
    pub wifi_seen: Vec<SeenNetwork>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct SeenNetwork {
    pub ssid: String,
    pub rssi: i32,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Busy {
    /// By name; empty when the box didn't say.
    pub owner: String,
    /// `usb`: on that computer's cable, can't be taken. `linked`: can be taken over.
    pub reason: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct PairedComputer {
    pub key: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Pin {
    pub variable: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Candidate {
    pub port: String,
    /// What the choice is remembered by; without one the device can't be chosen.
    pub usb_serial: Option<String>,
}

impl Device {
    /// The firmware as shown in the interface: version first, then the build ID.
    pub fn firmware_label(&self) -> Option<String> {
        match (&self.firmware_version, &self.firmware_build) {
            (Some(version), Some(build)) => Some(format!("{version} · {build}")),
            (version, build) => version.clone().or_else(|| build.clone()),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Today {
    pub done: u32,
    pub asks: u32,
    pub busy_seconds: u64,
}

/// When each agent's last hook event arrived, as the daemon wrote it (RFC 3339).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Hooks {
    pub codex: Option<String>,
    pub claude: Option<String>,
    pub opencode: Option<String>,
    pub copilot: Option<String>,
    pub pi: Option<String>,
}

/// The daemon owns this; the app only reads it and writes it back whole through `PUT /v1/config`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Config {
    pub voice: Option<String>,
    pub notify_link: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            voice: None,
            notify_link: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Online,
    Offline,
    DaemonDown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MenuState {
    pub icon: Icon,
    pub device_line: String,
    pub mode_line: String,
    pub today_line: String,
}

impl MenuState {
    /// `None` means the daemon isn't answering.
    pub fn derive(status: Option<&Status>) -> Self {
        let Some(status) = status else {
            return Self {
                icon: Icon::DaemonDown,
                // The tray offers to start it again: systemd may have given up after it failed to start.
                device_line: tr("daemon not responding · click to restart", &[]),
                mode_line: "—".to_owned(),
                today_line: "—".to_owned(),
            };
        };
        let device = &status.device;
        let device_line = if device.connected {
            let build = device
                .firmware_build
                .as_deref()
                .and_then(|build| build.split(' ').next())
                .unwrap_or("?");
            tr("Box online · firmware %@", &[&build])
        } else {
            tr("Box not found", &[])
        };
        let mode = match device.mode.as_deref() {
            Some("duty") => tr("On duty", &[]),
            Some("pomodoro") => tr("Pomodoro", &[]),
            Some("leisure") => tr("Leisure", &[]),
            _ => "—".to_owned(),
        };
        let today = &status.today;
        Self {
            icon: if device.connected {
                Icon::Online
            } else {
                Icon::Offline
            },
            device_line,
            mode_line: tr("Mode: %@", &[&mode]),
            today_line: tr(
                "Today: done %lld · asks %lld · busy %@",
                &[&today.done, &today.asks, &duration(today.busy_seconds)],
            ),
        }
    }
}

pub fn duration(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    if hours > 0 {
        tr("%lld h %lld min", &[&hours, &minutes])
    } else {
        tr("%lld min", &[&minutes])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE: &str = r#"{
        "daemon": {"build": "v0.2.2 2026-10-01 13:23", "app_version": null},
        "device": {"connected": true, "port": "/dev/ttyACM0", "bridge": false, "mode": "duty",
                   "firmware_build": "v0.2.1-38-ga0bffc7 2026-09-30 16:29", "voice": "xiaohe2", "volume": 40},
        "today": {"done": 8, "asks": 3, "busy_seconds": 2217},
        "hooks": {"codex": "2026-10-01T17:06:59+08:00", "claude": null},
        "operation": null,
        "config": {"voice": null, "notify_link": true}
    }"#;

    #[test]
    fn the_box_screens_parse() {
        let device: Device = serde_json::from_str(
            r#"{"connected": true, "port": null, "bridge": false, "screens": "korekore"}"#,
        )
        .expect("device");
        assert_eq!(device.screens.as_deref(), Some("korekore"));
    }

    #[test]
    fn a_live_snapshot_parses() {
        let status: Status = serde_json::from_str(LIVE).expect("status");
        assert!(status.device.connected);
        assert_eq!(status.device.volume, Some(40));
        assert_eq!(
            status.device.screens, None,
            "firmware that predates the choice says nothing"
        );
        assert_eq!(status.today.busy_seconds, 2217);
        assert!(status.config.notify_link);
    }

    #[test]
    fn the_settings_window_shows_the_revision_alone() {
        let status: Status = serde_json::from_str(LIVE).expect("status");
        assert_eq!(
            status.daemon.short_build(),
            "v0.2.2 2026-10-01 13:23",
            "an older daemon has no revision"
        );
        let newer: DaemonInfo = serde_json::from_str(
            r#"{"build": "0.4.0 v0.4.0-19-gcb7f43c 2026-10-09 17:28", "revision": "v0.4.0-19-gcb7f43c"}"#,
        )
        .expect("daemon");
        assert_eq!(newer.short_build(), "v0.4.0-19-gcb7f43c");
    }

    #[test]
    fn the_firmware_label_leads_with_the_version() {
        let mut status: Status = serde_json::from_str(LIVE).expect("status");
        assert_eq!(
            status.device.firmware_label().as_deref(),
            Some("v0.2.1-38-ga0bffc7 2026-09-30 16:29")
        );
        status.device.firmware_version = Some("0.2.2".to_owned());
        assert_eq!(
            status.device.firmware_label().as_deref(),
            Some("0.2.2 · v0.2.1-38-ga0bffc7 2026-09-30 16:29")
        );
    }

    #[test]
    fn the_menu_says_what_the_mac_app_says() {
        let status: Status = serde_json::from_str(LIVE).expect("status");
        let menu = MenuState::derive(Some(&status));
        assert_eq!(menu.icon, Icon::Online);
        // Tests run without a Chinese locale, so the English key is the text.
        assert_eq!(menu.device_line, "Box online · firmware v0.2.1-38-ga0bffc7");
        assert_eq!(menu.mode_line, "Mode: On duty");
        assert_eq!(menu.today_line, "Today: done 8 · asks 3 · busy 36 min");
    }

    #[test]
    fn no_daemon_and_no_box_read_differently() {
        assert_eq!(MenuState::derive(None).icon, Icon::DaemonDown);
        let offline = Status::default();
        assert_eq!(MenuState::derive(Some(&offline)).icon, Icon::Offline);
        assert_eq!(
            MenuState::derive(Some(&offline)).device_line,
            "Box not found"
        );
    }

    #[test]
    fn a_flash_waiting_for_a_replug_parses() {
        // An unknown state would fail the whole status and cut the app off from the daemon.
        let operation: Operation = serde_json::from_str(
            r#"{"kind": "firmware", "state": "replug", "progress": 1.0, "message": "firmware flashed but not started, replug the box"}"#,
        )
        .expect("operation");
        assert!(!operation.running());
    }

    #[test]
    fn own_keys_read_as_absent_from_an_older_daemon() {
        let status: Status = serde_json::from_str(LIVE).expect("status");
        assert_eq!(status.voice_keys, None);
        let keys: VoiceKeys =
            serde_json::from_str(r#"{"elevenlabs": {"set": true, "refused": true}}"#).expect("keys");
        assert_eq!(
            keys,
            VoiceKeys {
                elevenlabs: VoiceKey {
                    set: true,
                    refused: true
                }
            }
        );
    }

    #[test]
    fn the_character_being_made_reads_and_is_absent_from_an_older_daemon() {
        let status: Status = serde_json::from_str(LIVE).expect("status");
        assert_eq!(status.making, None);
        let status: Status = serde_json::from_str(r#"{"making":{"id":"owl-1a2b3c4d","name":"Owl","state":"making","done":3,"total":12,"reason":null,"error":null}}"#).expect("status");
        let making = status.making.expect("making");
        assert!(
            making.running() && making.id == "owl-1a2b3c4d" && making.progress() == 0.25,
            "{making:?}"
        );
    }

    #[test]
    fn durations_switch_to_hours() {
        assert_eq!(duration(59), "0 min");
        assert_eq!(duration(3600 + 120), "1 h 2 min");
    }
}
