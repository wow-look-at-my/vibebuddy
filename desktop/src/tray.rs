//! The tray icon, as a StatusNotifierItem: Omarchy's shell, KDE and waybar all host these. The icon is
//! Kore and the menu repeats the Mac app's menu bar menu; picking an item only sends an event to
//! the app, which does the work.

use std::path::PathBuf;

use futures::channel::mpsc::UnboundedSender;
use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem, ToolTip};

use crate::i18n::tr;
use crate::status::{Icon as FaceIcon, MenuState};

#[derive(Clone, Copy, Debug)]
pub enum TrayEvent {
    OpenSettings,
    /// The daemon isn't answering: have systemd start it again.
    RestartDaemon,
    ReportProblem,
    Quit,
    /// The shell's tray host is gone (not up yet at login, or restarting) or back; ksni registers again by itself.
    HostGone,
    HostBack,
}

pub struct Tray {
    pub menu: MenuState,
    /// The icon is Kore (ADR-0021), drawn from this.
    pub kore: crate::korekore::picture::BoxPicture,
    pub events: UnboundedSender<TrayEvent>,
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "vibebuddy".to_owned()
    }

    fn title(&self) -> String {
        "VibeBuddy".to_owned()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        [1, 2]
            .into_iter()
            .map(|scale| {
                let (width, height, data) = crate::korekore::tray_icon(&self.kore, scale);
                Icon {
                    width: width as i32,
                    height: height as i32,
                    data,
                }
            })
            .collect()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "VibeBuddy".to_owned(),
            description: self.menu.device_line.clone(),
            ..Default::default()
        }
    }

    fn watcher_online(&self) {
        let _ = self.events.unbounded_send(TrayEvent::HostBack);
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        let _ = self.events.unbounded_send(TrayEvent::HostGone);
        true
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.events.unbounded_send(TrayEvent::OpenSettings);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let line = |label: &str| {
            StandardItem {
                label: label.replace('_', "__"),
                enabled: false,
                ..Default::default()
            }
            .into()
        };
        let action = |label: String, event: TrayEvent| {
            StandardItem {
                label,
                activate: Box::new(move |tray: &mut Self| {
                    let _ = tray.events.unbounded_send(event);
                }),
                ..Default::default()
            }
            .into()
        };
        let daemon_down = self.menu.icon == FaceIcon::DaemonDown;
        let mut items = vec![
            if daemon_down {
                action(self.menu.device_line.clone(), TrayEvent::RestartDaemon)
            } else {
                line(&self.menu.device_line)
            },
            line(&self.menu.mode_line),
            line(&self.menu.today_line),
            MenuItem::Separator,
            action(tr("Settings…", &[]), TrayEvent::OpenSettings),
        ];
        items.push(action(
            tr("Report a Problem…", &[]),
            TrayEvent::ReportProblem,
        ));
        // systemd keeps the daemon running, so unlike on the Mac, quitting leaves the box online.
        items.push(action(tr("Quit", &[]), TrayEvent::Quit));
        items
    }
}

/// Omarchy keeps tray icons in a drawer behind a chevron until they're pinned, while the Mac's icon always shows in the
/// menu bar; so pin ours, once. The edit goes through Omarchy's own `omarchy-shell-config`, which writes `shell.json`
/// atomically and reloads the bar. Once means the marker: unpinning or hiding it later is the user's call, and an icon
/// they already hid stays hidden.
pub async fn pin_in_omarchy_bar(marker: PathBuf) {
    if marker.exists() {
        return;
    }
    // Exit 3: not Omarchy, so nothing to pin and nothing to remember.
    const SCRIPT: &str = r#"
source omarchy-shell-config 2>/dev/null || exit 3
commit "$NORMALIZE"' | .bar.layout[] |= map(
    (if . == "omarchy.tray" then {id: .} else . end)
    | if type == "object" and .id == "omarchy.tray"
        and ((.pinned // []) | any(. == "vibebuddy") | not)
        and ((.hidden // []) | any(. == "vibebuddy") | not)
      then .pinned = ((.pinned // []) + ["vibebuddy"]) else . end)'
"#;
    match tokio::process::Command::new("bash")
        .args(["-c", SCRIPT])
        .status()
        .await
    {
        Ok(status) if status.success() => {
            let _ = marker.parent().map(std::fs::create_dir_all);
            let _ = std::fs::write(&marker, "");
        }
        Ok(status) if status.code() == Some(3) => {}
        Ok(status) => {
            eprintln!("vibebuddy-desktop: pinning the tray icon in Omarchy's bar failed ({status})")
        }
        Err(error) => {
            eprintln!("vibebuddy-desktop: cannot run bash to pin the tray icon ({error})")
        }
    }
}
