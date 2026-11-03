//! The robot's face (characters/robot/face.png): the face the UI shows for the box, in colour or gray.

use std::sync::LazyLock;

use iced::widget::image;
use iced::{Color, Element, Length, Padding, widget::container};

struct Face {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    gray: Vec<u8>,
}

static FACE: LazyLock<Face> = LazyLock::new(|| {
    let face = ::image::load_from_memory(include_bytes!("../../../characters/robot/face.png"))
        .expect("the bundled robot face is a PNG")
        .into_rgba8();
    let (width, height) = face.dimensions();
    let rgba = face.into_raw();
    let gray = grayed(&rgba, 0.75);
    Face {
        width,
        height,
        rgba,
        gray,
    }
});

/// Each pixel's luminance (Rec. Weights) times `brightness`; alpha stays.
fn grayed(rgba: &[u8], brightness: f32) -> Vec<u8> {
    rgba.chunks(4)
        .flat_map(|pixel| {
            let level = ((0.2126 * f32::from(pixel[0])
                + 0.7152 * f32::from(pixel[1])
                + 0.0722 * f32::from(pixel[2]))
                * brightness)
                .round()
                .clamp(0.0, 255.0) as u8;
            [level, level, level, pixel[3]]
        })
        .collect()
}

/// The face at its own size.
pub fn handle(gray: bool) -> image::Handle {
    let face = &*FACE;
    let bytes = if gray { &face.gray } else { &face.rgba };
    image::Handle::from_rgba(face.width, face.height, bytes.clone())
}

/// The face `height` points high, with its own aspect.
pub fn face<'a, M: 'a>(height: f32, gray: bool) -> Element<'a, M> {
    image(handle(gray))
        .width(Length::Fixed(height * FACE.width as f32 / FACE.height as f32))
        .height(Length::Fixed(height))
        .into()
}

/// The face on a small stage, as the pages put it beside a note.
pub fn badge<'a, M: 'a>(gray: bool, fill: Color) -> Element<'a, M> {
    container(face(36.0, gray))
        .padding(Padding::from([5, 6]))
        .style(move |_| container::Style {
            background: Some(fill.into()),
            border: iced::Border {
                radius: 12.0.into(),
                ..iced::Border::default()
            },
            ..container::Style::default()
        })
        .into()
}

/// RGBA rows of the face resized to `height` pixels, with its own aspect.
pub fn rgba(height: u32, gray: bool) -> (u32, u32, Vec<u8>) {
    let face = &*FACE;
    let width = (height * face.width).div_ceil(face.height);
    let bytes = if gray { &face.gray } else { &face.rgba };
    let source = ::image::RgbaImage::from_raw(face.width, face.height, bytes.clone())
        .expect("the face's bytes match its size");
    let resized =
        ::image::imageops::resize(&source, width, height, ::image::imageops::FilterType::Triangle);
    (width, height, resized.into_raw())
}
