//! Pictures of files: what a plugin may ask for, the ffmpeg command that
//! makes it, and the check of what came out.

use std::ffi::OsString;

use rustcloud_core::{Error, Result};
use serde::Serialize;

use crate::MediaInfo;

/// The largest box a picture may be asked for.
const MAX_SIDE: u32 = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ImageFormat {
    Jpeg,
    Webp,
    Png,
}

/// How a picture fills its box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Fit {
    /// All of it, within the box.
    Inside,
    /// The box filled, the rest cropped: square thumbnails.
    Cover,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PictureOptions {
    pub max_width: u32,
    pub max_height: u32,
    pub fit: Fit,
    pub format: ImageFormat,
    /// 1–100.
    pub quality: u8,
    /// A video's moment; none: a representative frame, not the black start.
    pub at_ms: Option<u64>,
}

impl PictureOptions {
    pub(crate) fn check(&self) -> Result<()> {
        let side = 1..=MAX_SIDE;
        if !side.contains(&self.max_width) || !side.contains(&self.max_height) {
            return Err(invalid(format!("the box must be from 1×1 to {MAX_SIDE}×{MAX_SIDE}")));
        }
        if !(1..=100).contains(&self.quality) {
            return Err(invalid("quality must be from 1 to 100".to_owned()));
        }
        Ok(())
    }
}

fn invalid(message: String) -> Error {
    Error::bad_request("INVALID_OPTIONS", message)
}

/// The pixels of a file for a neural network: exactly `width`×`height`,
/// RGB, from all of the picture or a part of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PixelOptions {
    pub width: u32,
    pub height: u32,
    pub fit: PixelFit,
    /// A part of the picture as shown (turned as the camera was held), in
    /// its pixels; none: all of it.
    pub crop: Option<Crop>,
}

/// How a picture fills the exact size of [`PixelOptions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PixelFit {
    /// Stretched to it.
    Stretch,
    /// All of it, at the top left, the rest black: a model's "letterbox".
    Inside,
    /// It filled, the rest cropped.
    Cover,
}

/// A part of a picture, in its pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The largest side of [`PixelOptions`]: models take a few hundred pixels.
const MAX_PIXELS_SIDE: u32 = 4096;

impl PixelOptions {
    /// Checks the options against the picture's size as shown.
    pub(crate) fn check(&self, shown: (u32, u32)) -> Result<()> {
        let side = 1..=MAX_PIXELS_SIDE;
        if !side.contains(&self.width) || !side.contains(&self.height) {
            return Err(invalid(format!("the size must be from 1×1 to {MAX_PIXELS_SIDE}×{MAX_PIXELS_SIDE}")));
        }
        if let Some(crop) = self.crop {
            let inside = |start: u32, length: u32, whole: u32| length > 0 && start.checked_add(length) <= Some(whole);
            if !inside(crop.x, crop.width, shown.0) || !inside(crop.y, crop.height, shown.1) {
                return Err(invalid(format!("the crop is not within the {}×{} picture", shown.0, shown.1)));
            }
        }
        Ok(())
    }

    /// Bytes of the pixels: three per pixel.
    pub(crate) fn size(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * 3
    }
}

/// The start of an ffmpeg command for a picture of the probed file: its
/// input, at a video's asked moment or a tenth of the way in (past fades and
/// titles).
fn input_args(info: &MediaInfo, at_ms: Option<u64>, max_pixels: u64) -> Vec<String> {
    let mut args: Vec<String> =
        ["-v", "error", "-nostdin", "-protocol_whitelist", "fd", "-threads", "2"].map(str::to_owned).into();
    args.extend(["-max_pixels".into(), max_pixels.to_string()]);
    let start = match (info.still, at_ms, info.duration_ms) {
        (false, Some(at), _) => Some(at),
        (false, None, Some(duration)) if duration > 3000 => Some((duration / 10).min(30_000)),
        _ => None,
    };
    if let Some(at) = start {
        args.extend(["-ss".into(), format!("{}.{:03}", at / 1000, at % 1000)]);
    }
    args.extend(["-i".into(), "fd:".into()]);
    args
}

/// The filter that turns a picture as an EXIF `orientation` says, for
/// pictures the decoder doesn't turn itself (a RAW photo's preview).
fn turn(orientation: u16) -> &'static str {
    match orientation {
        2 => "hflip,",
        3 => "hflip,vflip,",
        4 => "vflip,",
        5 => "transpose=0,",
        6 => "transpose=1,",
        7 => "transpose=3,",
        8 => "transpose=2,",
        _ => "",
    }
}

/// The ffmpeg command for a picture of the probed file. Only numbers from
/// checked options and fixed words: nothing a plugin wrote.
/// `orientation`: the EXIF one of a picture the decoder doesn't turn itself
/// (a RAW photo's preview); 1 for none.
pub(crate) fn thumbnail_args(
    info: &MediaInfo,
    options: &PictureOptions,
    max_pixels: u64,
    orientation: u16,
) -> Vec<OsString> {
    let mut args = input_args(info, options.at_ms, max_pixels);
    let (width, height, turn) = (options.max_width, options.max_height, turn(orientation));
    let mut filters = match options.fit {
        Fit::Inside => format!("{turn}scale={width}:{height}:force_original_aspect_ratio=decrease"),
        Fit::Cover => {
            format!("{turn}scale={width}:{height}:force_original_aspect_ratio=increase,crop={width}:{height}")
        }
    };
    // Of a video's first frames from there, the most typical one.
    if !info.still && options.at_ms.is_none() {
        filters.push_str(",thumbnail=30");
    }
    args.extend(["-vf".into(), filters, "-frames:v".into(), "1".into()]);
    args.extend(["-an", "-sn", "-dn", "-map_metadata", "-1"].map(str::to_owned));

    let quality = u32::from(options.quality);
    match options.format {
        // mjpeg: 2 is the best, 31 the worst.
        ImageFormat::Jpeg => args.extend([
            "-c:v".into(),
            "mjpeg".into(),
            "-pix_fmt".into(),
            "yuvj420p".into(),
            "-q:v".into(),
            (2 + (100 - quality) * 29 / 99).to_string(),
        ]),
        ImageFormat::Webp => args.extend(["-c:v".into(), "libwebp".into(), "-quality".into(), quality.to_string()]),
        ImageFormat::Png => args.extend(["-c:v".into(), "png".into()]),
    }
    args.extend(["-f".into(), "image2pipe".into(), "pipe:1".into()]);
    args.into_iter().map(OsString::from).collect()
}

/// The ffmpeg command for the raw RGB pixels of the probed file, as
/// [`thumbnail_args`]: a video gives the frame a tenth of the way in.
pub(crate) fn pixels_args(
    info: &MediaInfo,
    options: &PixelOptions,
    max_pixels: u64,
    orientation: u16,
) -> Vec<OsString> {
    let mut args = input_args(info, None, max_pixels);
    let (width, height) = (options.width, options.height);
    let mut filters = turn(orientation).to_owned();
    if let Some(crop) = options.crop {
        filters.push_str(&format!("crop={}:{}:{}:{},", crop.width, crop.height, crop.x, crop.y));
    }
    filters.push_str(&match options.fit {
        PixelFit::Stretch => format!("scale={width}:{height}"),
        PixelFit::Inside => {
            format!("scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:0:0:black")
        }
        PixelFit::Cover => format!("scale={width}:{height}:force_original_aspect_ratio=increase,crop={width}:{height}"),
    });
    // Square pixels whatever the video says, then RGB.
    filters.push_str(",setsar=1,format=rgb24");
    args.extend(["-vf".into(), filters, "-frames:v".into(), "1".into()]);
    args.extend(["-an", "-sn", "-dn", "-map_metadata", "-1"].map(str::to_owned));
    args.extend(["-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"].map(str::to_owned));
    args.into_iter().map(OsString::from).collect()
}

/// Whether the tool gave a picture of the asked format.
pub(crate) fn check(picture: &[u8], format: ImageFormat) -> Result<()> {
    let fits = match format {
        ImageFormat::Jpeg => picture.starts_with(&[0xFF, 0xD8, 0xFF]),
        ImageFormat::Png => picture.starts_with(b"\x89PNG\r\n\x1a\n"),
        ImageFormat::Webp => picture.len() > 12 && &picture[0..4] == b"RIFF" && &picture[8..12] == b"WEBP",
    };
    if fits {
        Ok(())
    } else {
        Err(crate::failed("no picture came out"))
    }
}
