//! The app's only UI: the settings window, the first-run guide and the tray icon.

// The generated theme and sprites hold all the design's tokens, and the views
// do not use each.
#![allow(dead_code)]

mod account;
mod controls;
pub mod face;
mod guide;
pub(crate) mod pages;
pub mod picture;
#[cfg(test)]
mod snapshots;
pub mod sprites;
pub mod theme;
pub mod view;

use iced::widget::image;

use crate::i18n::tr;

/// The bundled fonts (fonts/ui), registered with iced at start.
pub const FONTS: [&[u8]; 5] = [
    include_bytes!("../../../fonts/ui/MPLUSRounded1c-Regular.ttf"),
    include_bytes!("../../../fonts/ui/MPLUSRounded1c-Bold.ttf"),
    include_bytes!("../../../fonts/ui/MPLUSRounded1c-ExtraBold.ttf"),
    include_bytes!("../../../fonts/ui/DotGothic16-Regular.ttf"),
    include_bytes!("../../../fonts/ui/FusionPixel12-Regular.ttf"),
];

/// Text that names no font: M PLUS Rounded 1c, regular.
pub const DEFAULT_FONT: iced::Font = theme::typography::BODY.font;

pub struct Sprite {
    pub width: u32,
    pub height: u32,
    pub palette: &'static [u32],
    pub pixels: &'static [u8],
}

impl Sprite {
    /// RGBA bytes at `scale` whole times the size, nearest-neighbour.
    pub fn rgba(&self, scale: u32) -> Vec<u8> {
        let (width, height) = (self.width * scale, self.height * scale);
        let mut bytes = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let index = self.pixels[((y / scale) * self.width + x / scale) as usize];
                bytes.extend_from_slice(&match index.checked_sub(1) {
                    None => [0; 4],
                    Some(n) => {
                        let rgb = self.palette[usize::from(n)];
                        [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 0xFF]
                    }
                });
            }
        }
        bytes
    }

    /// An image of the sprite at `scale` whole times its size; show it at that size, so nothing resamples it.
    pub fn handle(&self, scale: u32) -> image::Handle {
        image::Handle::from_rgba(self.width * scale, self.height * scale, self.rgba(scale))
    }
}

/// The tray icon: the robot's face in colour online, with a `sun` dot beside it when an agent needs you, gray
/// without a link. ARGB32 rows, as StatusNotifierItem pixmaps are.
pub fn tray_icon(picture: &picture::BoxPicture, scale: u32) -> (u32, u32, Vec<u8>) {
    const DOT: [&str; 7] = [
        "..iii..", ".isssi.", "isssssi", "isssssi", "isssssi", ".isssi.", "..iii..",
    ];
    // The icon is this many design pixels high.
    const HEIGHT: u32 = 18;
    let needs_you = picture.needs_you();
    let (face_width, height, rgba) =
        face::rgba(HEIGHT * scale, picture.mood == picture::Mood::NoLink);
    let width = if needs_you {
        face_width + (4 + 7) * scale
    } else {
        face_width
    };
    let mut out = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let pixel = if x < face_width {
                let at = ((y * face_width + x) * 4) as usize;
                [rgba[at + 3], rgba[at], rgba[at + 1], rgba[at + 2]]
            } else {
                let (dx, dy) = ((x - face_width) / scale, y / scale);
                let row = dy
                    .checked_sub((HEIGHT - 7) / 2)
                    .and_then(|row| DOT.get(row as usize));
                match row.and_then(|row| {
                    dx.checked_sub(4)
                        .and_then(|col| row.as_bytes().get(col as usize))
                }) {
                    Some(b'i') => [0xFF, 0x1C, 0x17, 0x30],
                    Some(b's') => [0xFF, 0xFF, 0xD2, 0x3F],
                    _ => [0; 4],
                }
            };
            out[((y * width + x) * 4) as usize..][..4].copy_from_slice(&pixel);
        }
    }
    (width, height, out)
}

/// The settings window's sections, in the sidebar's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Character,
    Agents,
    Conversation,
    Device,
    General,
    Advanced,
}

impl Section {
    pub const ALL: [Section; 6] = [
        Section::Character,
        Section::Agents,
        Section::Conversation,
        Section::Device,
        Section::General,
        Section::Advanced,
    ];

    pub fn title(self) -> String {
        match self {
            Section::Character => tr("Character", &[]),
            Section::Agents => tr("Agents", &[]),
            Section::Conversation => tr("Conversation", &[]),
            Section::Device => tr("Device", &[]),
            Section::General => tr("General", &[]),
            Section::Advanced => tr("Advanced", &[]),
        }
    }

    pub fn subtitle(self) -> String {
        match self {
            Section::Character => tr("Who lives in the box", &[]),
            Section::Agents => tr("Who the box watches", &[]),
            Section::Conversation => tr("Hold K0 on the box and ask", &[]),
            Section::Device => tr("The box, its firmware and its network", &[]),
            Section::General => tr("How VibeBuddy behaves on this computer", &[]),
            Section::Advanced => tr("For when something's off", &[]),
        }
    }

    /// The tab this section stands for: same state, same messages.
    pub fn tab(self) -> crate::Tab {
        match self {
            Section::Character => crate::Tab::Sound,
            Section::Agents => crate::Tab::Agents,
            Section::Conversation => crate::Tab::Conversation,
            Section::Device => crate::Tab::Device,
            Section::General => crate::Tab::General,
            Section::Advanced => crate::Tab::Advanced,
        }
    }

    pub fn of(tab: crate::Tab) -> Self {
        Section::ALL
            .into_iter()
            .find(|section| section.tab() == tab)
            .unwrap_or(Section::Character)
    }

    pub fn tile(self) -> &'static str {
        match self {
            Section::Character => "icon-nav-char",
            Section::Agents => "icon-nav-agents",
            Section::Conversation => "icon-nav-chat",
            Section::Device => "icon-nav-box",
            Section::General => "icon-nav-gen",
            Section::Advanced => "icon-nav-adv",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaling_repeats_each_pixel_and_keeps_transparency() {
        let icon = &sprites::ICON_CC;
        let rgba = icon.rgba(3);
        let width = icon.width * 3;
        assert_eq!(rgba.len(), (width * icon.height * 3 * 4) as usize);
        for y in 0..icon.height * 3 {
            for x in 0..width {
                let index = icon.pixels[((y / 3) * icon.width + x / 3) as usize];
                let expected = match index.checked_sub(1) {
                    None => [0; 4],
                    Some(n) => {
                        let rgb = icon.palette[usize::from(n)];
                        [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 0xFF]
                    }
                };
                assert_eq!(rgba[((y * width + x) * 4) as usize..][..4], expected);
            }
        }
        assert!(icon.pixels.contains(&0) && icon.pixels.iter().any(|&index| index > 0));
    }

    #[test]
    fn every_sprite_is_whole_and_indexes_its_palette() {
        for (name, sprite) in sprites::ALL {
            assert_eq!(
                sprite.pixels.len(),
                (sprite.width * sprite.height) as usize,
                "{name}"
            );
            assert!(
                sprite
                    .pixels
                    .iter()
                    .all(|&index| usize::from(index) <= sprite.palette.len()),
                "{name}"
            );
        }
    }

    #[test]
    fn the_tray_icon_adds_a_sun_dot_only_when_an_agent_needs_you() {
        use crate::status::{Activity, Status};
        let mut status = Status::default();
        status.device.connected = true;
        let (face_width, height, pixels) =
            tray_icon(&picture::BoxPicture::derive(Some(&status)), 2);
        assert_eq!(height, 36);
        assert_eq!(pixels.len(), (face_width * height * 4) as usize);
        status.activities = vec![Activity {
            agent: "codex".into(),
            title: "CX:Ask".into(),
            state: "needs_input".into(),
            ..Activity::default()
        }];
        let (width, _, pixels) = tray_icon(&picture::BoxPicture::derive(Some(&status)), 2);
        assert_eq!(width, face_width + 22);
        // The dot's centre, in ARGB: sun.
        let at = ((17 * width + face_width + 2 * 7) * 4) as usize;
        assert_eq!(&pixels[at..at + 4], [0xFF, 0xFF, 0xD2, 0x3F]);
    }

    #[test]
    fn the_theme_matches_the_tokens() {
        assert_eq!(theme::INK, iced::Color::from_rgb8(0x1C, 0x17, 0x30));
        assert_eq!(theme::BOX_GROUND, theme::INK);
        assert_eq!(theme::typography::TITLE.size, 28.0);
        assert_eq!(theme::STICKER_SM, 4.0);
    }
}
