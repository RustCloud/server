//! What a file is: ffprobe for the container and streams, EXIF and QuickTime
//! tags (`nom-exif`) for when, where and with what it was shot.

use std::ffi::OsString;

use nom_exif::{EntryValue, Exif, ExifTag, GPSInfo, MediaKind, MediaParser, MediaSource, TrackInfoTag};
use rustcloud_core::Result;
use serde::Serialize;
use serde_json::Value;

/// Tags taken from a file at most.
const TAG_LIMIT: usize = 200;
/// Longest tag value kept.
const VALUE_LIMIT: usize = 500;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MediaInfo {
    /// `jpeg`, `heif`, `mp4`, `mov`, `mkv`, `webm`, `mp3`…
    pub container: String,
    pub mime_type: String,
    /// Videos and music; none for a photo.
    pub duration_ms: Option<u64>,
    /// As shown: the rotation of the picture applied.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// A photo rather than a video.
    pub still: bool,
    pub video: Vec<VideoStream>,
    pub audio: Vec<AudioStream>,
    pub subtitles: Vec<SubtitleStream>,
    /// When it was shot, RFC 3339; without an offset when the camera wrote none.
    pub taken_at: Option<String>,
    pub location: Option<Location>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens: Option<String>,
    /// Other tags, flat: `exif.ISO` → `200`, `ExposureTime` → `1/60`.
    pub tags: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct VideoStream {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    /// Degrees the picture is turned when shown.
    pub rotation: i16,
    pub bit_depth: u8,
    pub hdr: bool,
    pub bitrate: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AudioStream {
    pub codec: String,
    pub channels: u8,
    pub sample_rate: u32,
    pub bitrate: u64,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SubtitleStream {
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
    /// Metres above sea level.
    pub altitude: Option<f64>,
}

/// What EXIF or QuickTime tags say.
#[derive(Debug, Default)]
pub(crate) struct Tags {
    taken_at: Option<String>,
    location: Option<Location>,
    make: Option<String>,
    model: Option<String>,
    lens: Option<String>,
    other: Vec<(String, String)>,
}

/// ffprobe's arguments: the file is the open descriptor it got as input.
pub(crate) fn probe_args() -> Vec<OsString> {
    ["-v", "error", "-protocol_whitelist", "fd", "-print_format", "json", "-show_format", "-show_streams", "-i", "fd:"]
        .into_iter()
        .map(OsString::from)
        .collect()
}

/// The tags of a file; none if it has none or they can't be read.
pub(crate) async fn read_tags(file: &std::fs::File) -> Tags {
    let Ok(file) = file.try_clone() else {
        return Tags::default();
    };
    tokio::task::spawn_blocking(move || tags_of(file).unwrap_or_default()).await.unwrap_or_default()
}

fn tags_of(mut file: std::fs::File) -> Option<Tags> {
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(0)).ok()?;
    let source = MediaSource::seekable(file).ok()?;
    let mut parser = MediaParser::new();
    match source.kind() {
        MediaKind::Image => {
            let exif: Exif = parser.parse_exif(source).ok()?.into();
            let text = |tag| exif.get(tag).map(EntryValue::to_string).map(clean);
            let other = exif
                .entries()
                .map(|entry| (format!("exif.{}", entry.tag()), clean(entry.value().to_string())))
                .take(TAG_LIMIT)
                .collect();
            Some(Tags {
                taken_at: exif.get(ExifTag::DateTimeOriginal).and_then(date),
                location: exif.gps_info().and_then(location),
                make: text(ExifTag::Make),
                model: text(ExifTag::Model),
                lens: text(ExifTag::LensModel),
                other,
            })
        }
        MediaKind::Track => {
            let track = parser.parse_track(source).ok()?;
            let text = |tag| track.get(tag).map(EntryValue::to_string).map(clean);
            Some(Tags {
                taken_at: track.get(TrackInfoTag::CreateDate).and_then(date),
                location: track.gps_info().and_then(location),
                make: text(TrackInfoTag::Make),
                model: text(TrackInfoTag::Model),
                lens: None,
                other: Vec::new(),
            })
        }
    }
}

fn date(value: &EntryValue) -> Option<String> {
    match value.as_datetime()? {
        nom_exif::ExifDateTime::Aware(at) => Some(at.to_rfc3339()),
        nom_exif::ExifDateTime::Naive(at) => Some(at.format("%Y-%m-%dT%H:%M:%S").to_string()),
    }
}

fn location(gps: &GPSInfo) -> Option<Location> {
    let (latitude, longitude) = (gps.latitude_decimal()?, gps.longitude_decimal()?);
    let valid = (-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude);
    // 0, 0 is what many cameras write when they don't know.
    (valid && (latitude, longitude) != (0.0, 0.0)).then(|| Location {
        latitude,
        longitude,
        altitude: gps.altitude_meters(),
    })
}

/// A tag value fit to pass on: printable, not too long.
fn clean(value: String) -> String {
    value.chars().filter(|c| !c.is_control()).take(VALUE_LIMIT).collect::<String>().trim().to_owned()
}

/// What a RAW photo is: its preview's size (turned as shown) and its tags;
/// no ffprobe needed.
pub(crate) fn raw(preview: &crate::raw::Preview, tags: Tags) -> MediaInfo {
    let mime_type = match preview.kind {
        "cr2" => "image/x-canon-cr2",
        "cr3" => "image/x-canon-cr3",
        "nef" => "image/x-nikon-nef",
        "arw" => "image/x-sony-arw",
        "dng" => "image/x-adobe-dng",
        "raf" => "image/x-fuji-raf",
        "rw2" => "image/x-panasonic-rw2",
        "orf" => "image/x-olympus-orf",
        "pef" => "image/x-pentax-pef",
        _ => "image/x-raw",
    };
    let (width, height) = preview.shown();
    MediaInfo {
        container: preview.kind.to_owned(),
        mime_type: mime_type.to_owned(),
        still: true,
        width: Some(width),
        height: Some(height),
        taken_at: tags.taken_at.or_else(|| preview.camera.taken_at.clone()),
        location: tags.location,
        camera_make: tags.make.or_else(|| preview.camera.make.clone()),
        camera_model: tags.model.or_else(|| preview.camera.model.clone()),
        lens: tags.lens,
        tags: tags.other,
        ..MediaInfo::default()
    }
}

/// Reads ffprobe's answer and adds what the tags say.
pub(crate) fn parse(output: &[u8], tags: Tags) -> Result<MediaInfo> {
    let json: Value = serde_json::from_slice(output).map_err(|_| crate::failed("not a media file"))?;
    let format = &json["format"];
    let streams = json["streams"].as_array().cloned().unwrap_or_default();
    let names = format["format_name"].as_str().unwrap_or_default();
    let brand = format["tags"]["major_brand"].as_str().unwrap_or_default().trim().to_owned();
    let codecs: Vec<&str> = streams.iter().filter_map(|s| s["codec_name"].as_str()).collect();
    let (container, mime_type, still) = container(names, &brand, &codecs);

    let number = |value: &Value| value.as_str().and_then(|v| v.parse::<f64>().ok()).or_else(|| value.as_f64());
    let text = |value: &Value| value.as_str().map(|v| clean(v.to_owned())).filter(|v| !v.is_empty());
    let mut info = MediaInfo { container, mime_type, still, ..MediaInfo::default() };
    if !still {
        info.duration_ms = number(&format["duration"]).filter(|d| *d > 0.0).map(|d| (d * 1000.0) as u64);
    }

    let mut picture: Option<(u64, u32, u32)> = None;
    for stream in &streams {
        let codec = stream["codec_name"].as_str().unwrap_or("unknown").to_owned();
        match stream["codec_type"].as_str() {
            Some("video") => {
                let (width, height) =
                    (stream["width"].as_u64().unwrap_or(0) as u32, stream["height"].as_u64().unwrap_or(0) as u32);
                let rotation = rotation(stream);
                let (shown_width, shown_height) =
                    if rotation.abs() % 180 == 90 { (height, width) } else { (width, height) };
                // The largest picture is the one shown; a cover counts only without others.
                let cover = stream["disposition"]["attached_pic"].as_u64() == Some(1);
                let weight = u64::from(width) * u64::from(height) + if cover { 0 } else { 1 << 40 };
                if picture.is_none_or(|(best, _, _)| weight > best) {
                    picture = Some((weight, shown_width, shown_height));
                }
                if cover {
                    continue;
                }
                let pixels = stream["pix_fmt"].as_str().unwrap_or_default();
                info.video.push(VideoStream {
                    codec,
                    width,
                    height,
                    fps: frame_rate(stream["avg_frame_rate"].as_str()).unwrap_or(0.0),
                    rotation,
                    bit_depth: if pixels.contains("12") {
                        12
                    } else if pixels.contains("10") {
                        10
                    } else {
                        8
                    },
                    hdr: matches!(stream["color_transfer"].as_str(), Some("smpte2084" | "arib-std-b67")),
                    bitrate: number(&stream["bit_rate"]).unwrap_or(0.0) as u64,
                });
            }
            Some("audio") => info.audio.push(AudioStream {
                codec,
                channels: stream["channels"].as_u64().unwrap_or(0).min(255) as u8,
                sample_rate: number(&stream["sample_rate"]).unwrap_or(0.0) as u32,
                bitrate: number(&stream["bit_rate"]).unwrap_or(0.0) as u64,
                language: text(&stream["tags"]["language"]),
            }),
            Some("subtitle") => info.subtitles.push(SubtitleStream {
                codec,
                language: text(&stream["tags"]["language"]),
                title: text(&stream["tags"]["title"]),
            }),
            _ => {}
        }
    }
    if let Some((_, width, height)) = picture {
        (info.width, info.height) = (Some(width), Some(height));
    }

    let container_tag = |key: &str| text(&format["tags"][key]);
    info.taken_at = tags
        .taken_at
        .or_else(|| container_tag("com.apple.quicktime.creationdate"))
        .or_else(|| container_tag("creation_time"));
    info.location = tags.location;
    info.camera_make = tags.make.or_else(|| container_tag("com.apple.quicktime.make"));
    info.camera_model = tags.model.or_else(|| container_tag("com.apple.quicktime.model"));
    info.lens = tags.lens;
    info.tags = tags.other;
    if let Some(container_tags) = format["tags"].as_object() {
        let room = TAG_LIMIT.saturating_sub(info.tags.len());
        let extra = container_tags.iter().filter_map(|(key, value)| Some((clean(key.clone()), text(value)?)));
        info.tags.extend(extra.take(room));
    }
    Ok(info)
}

/// The container, its MIME type and whether it holds a still picture.
fn container(names: &str, brand: &str, codecs: &[&str]) -> (String, String, bool) {
    let first = names.split(',').next().unwrap_or_default();
    let (name, mime, still) = match first {
        "mov" => match brand {
            "heic" | "heix" | "mif1" | "msf1" | "heim" | "heis" => ("heif", "image/heic", true),
            "avif" | "avis" => ("avif", "image/avif", true),
            "qt" => ("mov", "video/quicktime", false),
            "M4A" => ("m4a", "audio/mp4", false),
            _ => ("mp4", "video/mp4", false),
        },
        "matroska" => {
            let web = codecs.iter().all(|c| matches!(*c, "vp8" | "vp9" | "av1" | "opus" | "vorbis" | "webvtt"));
            if web {
                ("webm", "video/webm", false)
            } else {
                ("mkv", "video/x-matroska", false)
            }
        }
        "jpeg_pipe" => ("jpeg", "image/jpeg", true),
        "png_pipe" => ("png", "image/png", true),
        "webp_pipe" => ("webp", "image/webp", true),
        "gif" => ("gif", "image/gif", true),
        "bmp_pipe" => ("bmp", "image/bmp", true),
        "tiff_pipe" => ("tiff", "image/tiff", true),
        "mp3" => ("mp3", "audio/mpeg", false),
        "flac" => ("flac", "audio/flac", false),
        "ogg" => ("ogg", "audio/ogg", false),
        "wav" => ("wav", "audio/wav", false),
        "aac" => ("aac", "audio/aac", false),
        "avi" => ("avi", "video/x-msvideo", false),
        "mpegts" => ("ts", "video/mp2t", false),
        "flv" => ("flv", "video/x-flv", false),
        other => (other, "application/octet-stream", false),
    };
    (name.to_owned(), mime.to_owned(), still)
}

/// The turn of a picture: the display matrix, or the old `rotate` tag.
fn rotation(stream: &Value) -> i16 {
    let matrix = stream["side_data_list"].as_array().into_iter().flatten().find_map(|data| data["rotation"].as_f64());
    let tag = stream["tags"]["rotate"].as_str().and_then(|r| r.parse::<f64>().ok());
    // ffprobe gives the matrix's angle counter-clockwise.
    let degrees = matrix.map(|r| -r).or(tag).unwrap_or(0.0).round() as i64;
    degrees.rem_euclid(360) as i16
}

fn frame_rate(rate: Option<&str>) -> Option<f32> {
    let (top, bottom) = rate?.split_once('/')?;
    let (top, bottom): (f32, f32) = (top.parse().ok()?, bottom.parse().ok()?);
    (bottom > 0.0).then(|| top / bottom)
}
