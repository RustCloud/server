//! The media engine: what plugins with `use = ["ffmpeg"]` may ask the
//! server to do with photos, videos and music — metadata, thumbnails,
//! streams — and the pixels of pictures for `use.onnx`.
//!
//! The work is done by our own static ffmpeg and ffprobe
//! (`docker/build-ffmpeg.sh`), each run in a process of its own behind
//! [`rustcloud_sandbox`]: it gets the file as an open, read-only descriptor and
//! gives its result through a pipe, and can't open a file, reach the network
//! or start a program. A plugin never passes it a word: it asks for an intent
//! ([`PictureOptions`]) and the engine writes the command line. Before any
//! picture is made the file is probed and checked against the `[media]`
//! limits; what comes back is checked too.
//!
//! Photo metadata (date, place, camera) comes from EXIF and QuickTime tags,
//! read here with `nom-exif`, which is memory-safe Rust. RAW photos are
//! shown by the JPEG the camera put inside ([`raw`]).

mod info;
mod picture;
mod raw;
mod run;
mod samples;
mod settings;
mod stream;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use rustcloud_core::{Error, ErrorKind, Live, Result};
use rustcloud_sandbox::Layers;
use serde::Serialize;
use tokio::sync::Semaphore;

pub use info::{AudioStream, Location, MediaInfo, SubtitleStream, VideoStream};
pub use picture::{Crop, Fit, ImageFormat, PictureOptions, PixelFit, PixelOptions};
pub use run::Launcher;
pub use samples::SampleOptions;
pub use settings::MediaSettings;
pub use stream::{Piece, StreamMode, PIECE};

/// The largest piece of a stream: six seconds of a 4K video are ~40 MiB.
const PIECE_LIMIT: u64 = 256 * 1024 * 1024;

/// The longest a request waits for a free process before it hears `busy`.
const WAIT_FOR_SLOT: Duration = Duration::from_secs(10);
/// The longest a batch of jobs waits: it has time, and a job that hears
/// `busy` is only tried again later.
const BACKGROUND_WAIT: Duration = Duration::from_secs(300);

/// The pixels of a picture ([`Media::pixels`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixels {
    /// The picture's size as shown, before any crop.
    pub width: u32,
    pub height: u32,
    /// Three bytes per pixel, row by row, the size asked for.
    pub rgb: Vec<u8>,
}

/// What the engine can do on this server, for plugins and the admin page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub available: bool,
    /// The ffmpeg version, if it runs.
    pub ffmpeg: Option<String>,
    /// Pictures it reads: `jpeg`, `heic`…
    pub image_formats: Vec<String>,
    pub video_codecs: Vec<String>,
    pub audio_codecs: Vec<String>,
}

/// How the engine stands, for the admin page.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub available: bool,
    pub ffmpeg: Option<String>,
    /// What confines the tools here.
    pub layers: Option<Layers>,
    /// Why it isn't available, or what is weaker than it should be.
    pub problem: Option<String>,
}

/// The tools, found in the tools folder.
#[derive(Debug, Clone)]
struct Tools {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

pub struct Media {
    tools: Option<Tools>,
    launcher: Launcher,
    settings: Arc<Live<MediaSettings>>,
    /// Processes that may run at once.
    slots: Semaphore,
    /// Of those, what batches of jobs may take: all but one, which is kept
    /// for requests, so indexing a library never makes a page wait.
    background_slots: Semaphore,
    status: Status,
}

/// Who asks: a request someone waits for, or a batch of jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Request,
    Background,
}

impl Media {
    /// Finds the tools in `tools_dir` and checks that they run confined.
    /// A server without them works on; the engine answers `unsupported`.
    pub async fn start(launcher: Launcher, tools_dir: Option<&Path>, settings: Arc<Live<MediaSettings>>) -> Self {
        let workers = settings.read(MediaSettings::workers);
        let mut media = Self {
            tools: None,
            launcher,
            settings,
            slots: Semaphore::new(workers),
            background_slots: Semaphore::new(workers.saturating_sub(1).max(1)),
            status: Status::default(),
        };
        media.status = media.check(tools_dir).await;
        match &media.status.problem {
            Some(problem) if !media.status.available => tracing::warn!("media engine unavailable: {problem}"),
            Some(problem) => tracing::warn!("media engine: {problem}"),
            None => tracing::info!(
                "media engine: ffmpeg {}, {workers} at a time",
                media.status.ffmpeg.as_deref().unwrap_or("?")
            ),
        }
        media
    }

    async fn check(&mut self, tools_dir: Option<&Path>) -> Status {
        let unavailable = |problem: String| Status { problem: Some(problem), ..Status::default() };
        if !self.settings.read(|s| s.enabled) {
            return unavailable("turned off in [media]".to_owned());
        }
        let Some(dir) = tools_dir else {
            return unavailable("no media_tools_dir: the server has no ffmpeg".to_owned());
        };
        let tools = Tools { ffmpeg: dir.join("ffmpeg"), ffprobe: dir.join("ffprobe") };
        if !tools.ffmpeg.is_file() || !tools.ffprobe.is_file() {
            return unavailable(format!("no ffmpeg and ffprobe in {}", dir.display()));
        }
        let layers = match self.launcher.layers().await {
            Ok(layers) => layers,
            Err(err) => return unavailable(format!("the sandbox doesn't work here: {err}")),
        };
        let version = match self.launcher.version(&tools.ffmpeg, &self.limits()).await {
            Ok(version) => version,
            Err(err) => return unavailable(format!("ffmpeg doesn't run in the sandbox (is it static?): {err}")),
        };
        self.tools = Some(tools);
        let problem = (!layers.landlock).then(|| {
            "Landlock is not enabled in this kernel; seccomp alone keeps the tools away from files".to_owned()
        });
        Status { available: true, ffmpeg: Some(version), layers: Some(layers), problem }
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// What plugins may ask for here.
    pub fn capabilities(&self) -> Capabilities {
        let list = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect();
        match self.status.available {
            true => Capabilities {
                available: true,
                ffmpeg: self.status.ffmpeg.clone(),
                image_formats: list(&[
                    "jpeg", "png", "webp", "gif", "bmp", "tiff", "heic", "avif", "dng", "cr2", "cr3", "nef", "arw",
                    "raf", "rw2", "orf", "pef",
                ]),
                video_codecs: list(&["h264", "hevc", "vp8", "vp9", "av1", "mpeg4", "mpeg2"]),
                audio_codecs: list(&["aac", "mp3", "opus", "vorbis", "flac", "alac", "pcm"]),
            },
            false => Capabilities {
                available: false,
                ffmpeg: None,
                image_formats: Vec::new(),
                video_codecs: Vec::new(),
                audio_codecs: Vec::new(),
            },
        }
    }

    /// What a file is: container, streams, size, when and where it was shot.
    pub async fn metadata(&self, input: std::fs::File, priority: Priority) -> Result<MediaInfo> {
        let tools = self.tools()?;
        let tags = info::read_tags(&input).await;
        if let Some(preview) = raw_preview(&input).await? {
            return Ok(info::raw(&preview, tags));
        }
        rewind(&input)?;
        let _slot = self.slot(priority).await?;
        let input = Stdio::from(input);
        let output = self.launcher.run(&tools.ffprobe, info::probe_args(), input, &self.limits(), 1024 * 1024).await?;
        info::parse(&output, tags)
    }

    /// A picture of a file within `options`' box: a photo of any format, a
    /// frame of a video, an album cover, of at most `limit` bytes. The file
    /// is probed first and refused if it is beyond the `[media]` limits.
    pub async fn thumbnail(
        &self,
        input: std::fs::File,
        options: &PictureOptions,
        limit: u64,
        priority: Priority,
    ) -> Result<Vec<u8>> {
        options.check()?;
        let make_args =
            |info: &MediaInfo, _shown, max_pixels, turn| Ok(picture::thumbnail_args(info, options, max_pixels, turn));
        let (picture, _shown) = self.picture(input, make_args, limit, priority).await?;
        picture::check(&picture, options.format)?;
        Ok(picture)
    }

    /// The RGB pixels of a file for a neural network, exactly the size of
    /// `options`, and the size of the picture as shown (before the crop):
    /// a photo of any format, a frame of a video, an album cover.
    pub async fn pixels(&self, input: std::fs::File, options: &PixelOptions, priority: Priority) -> Result<Pixels> {
        let make_args = |info: &MediaInfo, shown, max_pixels, turn| {
            options.check(shown)?;
            Ok(picture::pixels_args(info, options, max_pixels, turn))
        };
        let (rgb, (width, height)) = self.picture(input, make_args, options.size(), priority).await?;
        if rgb.len() as u64 != options.size() {
            return Err(failed("no picture came out"));
        }
        Ok(Pixels { width, height, rgb })
    }

    /// The sound of a file — music, a recording, a video's first audio
    /// track — mono at `options.rate`, as little-endian 32-bit floats: at
    /// most `limit` bytes, so a long file is asked for in parts.
    pub async fn samples(
        &self,
        input: std::fs::File,
        options: &SampleOptions,
        limit: u64,
        priority: Priority,
    ) -> Result<Vec<u8>> {
        options.check()?;
        let tools = self.tools()?;
        let info = self.metadata(input.try_clone().map_err(Error::internal)?, priority).await?;
        if info.audio.is_empty() {
            return Err(unsupported("the file has no sound"));
        }
        // Refused before the work, if the length is known.
        let length_ms = options.duration_ms.or(info.duration_ms.map(|ms| ms.saturating_sub(options.start_ms)));
        if length_ms.is_some_and(|ms| u64::from(options.rate) * 4 * ms / 1000 > limit) {
            return Err(failed("the sound is longer than a call takes at once: ask for a part (duration)"));
        }
        rewind(&input)?;
        let args = samples::samples_args(options);
        let _slot = self.slot(priority).await?;
        self.launcher.run(&tools.ffmpeg, args, Stdio::from(input), &self.limits(), limit).await
    }

    /// Runs ffmpeg for a picture of a file, with the command `make_args`
    /// writes from the probed file, its size as shown, the pixel limit and
    /// the EXIF orientation to apply; at most `limit` bytes. Gives what
    /// ffmpeg printed and the size as shown.
    async fn picture(
        &self,
        input: std::fs::File,
        make_args: impl FnOnce(&MediaInfo, (u32, u32), u64, u16) -> Result<Vec<std::ffi::OsString>>,
        limit: u64,
        priority: Priority,
    ) -> Result<(Vec<u8>, (u32, u32))> {
        let tools = self.tools()?;
        let max_pixels = u64::from(self.settings.read(|s| s.max_megapixels)) * 1_000_000;
        if let Some(preview) = raw_preview(&input).await? {
            return self.raw_picture(tools, input, preview, make_args, max_pixels, limit, priority).await;
        }
        let probe_input = input.try_clone().map_err(Error::internal)?;
        let info = self.metadata(probe_input, priority).await?;
        let (width, height) = (info.width.unwrap_or(0), info.height.unwrap_or(0));
        if width == 0 || height == 0 {
            return Err(unsupported("the file has no picture"));
        }
        if u64::from(width) * u64::from(height) > max_pixels {
            return Err(failed(format!("{width}×{height} is larger than the server allows")));
        }
        rewind(&input)?;
        let args = make_args(&info, (width, height), max_pixels, 1)?;
        let _slot = self.slot(priority).await?;
        let output = self.launcher.run(&tools.ffmpeg, args, Stdio::from(input), &self.limits(), limit).await?;
        Ok((output, (width, height)))
    }

    /// The pieces a video is streamed in ([`stream`]): on its keyframes when
    /// repacked, every six seconds when re-encoded.
    pub async fn stream_pieces(
        &self,
        input: std::fs::File,
        mode: StreamMode,
        priority: Priority,
    ) -> Result<Vec<Piece>> {
        let tools = self.tools()?;
        let info = self.metadata(input.try_clone().map_err(Error::internal)?, priority).await?;
        if info.still || info.video.is_empty() {
            return Err(unsupported("the file is no video"));
        }
        let duration = info.duration_ms.ok_or_else(|| unsupported("the video's length is unknown"))?;
        let keyframes = match mode {
            StreamMode::H264 => Vec::new(),
            StreamMode::Copy => {
                rewind(&input)?;
                let _slot = self.slot(priority).await?;
                let (args, limits) = (stream::packets_args(), self.limits());
                let list = self.launcher.run(&tools.ffprobe, args, Stdio::from(input), &limits, stream::PACKETS_LIMIT);
                stream::keyframes(&list.await?)
            }
        };
        let first = keyframes.first().map_or(0, |keyframe| keyframe.pts_ms);
        Ok(stream::pieces(mode, &keyframes, first, duration))
    }

    /// One piece of a video's stream as fragmented MP4: its init segment
    /// (the same for every piece of the video and mode) and its media.
    pub async fn stream_piece(
        &self,
        input: std::fs::File,
        mode: StreamMode,
        piece: Piece,
        priority: Priority,
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let tools = self.tools()?;
        let max_pixels = u64::from(self.settings.read(|s| s.max_megapixels)) * 1_000_000;
        let args = stream::piece_args(mode, piece, max_pixels);
        let _slot = self.slot(priority).await?;
        let mp4 = self.launcher.run(&tools.ffmpeg, args, Stdio::from(input), &self.limits(), PIECE_LIMIT).await?;
        stream::split_init(mp4)
    }

    /// A picture of a RAW photo, from the JPEG the camera put inside: ffmpeg
    /// gets it through a pipe and turns it as the camera was held.
    #[allow(clippy::too_many_arguments)]
    async fn raw_picture(
        &self,
        tools: &Tools,
        input: std::fs::File,
        preview: raw::Preview,
        make_args: impl FnOnce(&MediaInfo, (u32, u32), u64, u16) -> Result<Vec<std::ffi::OsString>>,
        max_pixels: u64,
        limit: u64,
        priority: Priority,
    ) -> Result<(Vec<u8>, (u32, u32))> {
        if u64::from(preview.width) * u64::from(preview.height) > max_pixels {
            return Err(failed(format!("{}×{} is larger than the server allows", preview.width, preview.height)));
        }
        let turn = preview.turn();
        // Turned by a quarter, it is shown the other way round.
        let shown = match turn {
            5..=8 => (preview.height, preview.width),
            _ => (preview.width, preview.height),
        };
        let info = MediaInfo { still: true, ..MediaInfo::default() };
        let args = make_args(&info, shown, max_pixels, turn)?;
        let jpeg = tokio::task::spawn_blocking(move || preview.read(&input))
            .await
            .map_err(Error::internal)?
            .ok_or_else(|| failed("the preview can't be read"))?;
        let (reader, mut writer) = std::io::pipe().map_err(Error::internal)?;
        // A tool that stops reading early just leaves the rest unwritten.
        tokio::task::spawn_blocking(move || std::io::Write::write_all(&mut writer, &jpeg));
        let _slot = self.slot(priority).await?;
        let output = self.launcher.run(&tools.ffmpeg, args, Stdio::from(reader), &self.limits(), limit).await?;
        Ok((output, shown))
    }

    fn tools(&self) -> Result<&Tools> {
        self.tools
            .as_ref()
            .ok_or_else(|| unsupported(self.status.problem.as_deref().unwrap_or("the media engine is not available")))
    }

    fn limits(&self) -> run::Limits {
        self.settings.read(|s| run::Limits {
            memory: u64::from(s.memory_mib) * 1024 * 1024,
            time: Duration::from_secs(s.time_seconds.into()),
        })
    }

    /// A free process: a batch of jobs takes one of its own share first.
    async fn slot(&self, priority: Priority) -> Result<Vec<tokio::sync::SemaphorePermit<'_>>> {
        let acquire = async {
            let mut permits = Vec::new();
            if priority == Priority::Background {
                permits.push(self.background_slots.acquire().await?);
            }
            permits.push(self.slots.acquire().await?);
            Ok::<_, tokio::sync::AcquireError>(permits)
        };
        let wait = match priority {
            Priority::Request => WAIT_FOR_SLOT,
            Priority::Background => BACKGROUND_WAIT,
        };
        match tokio::time::timeout(wait, acquire).await {
            Ok(Ok(permits)) => Ok(permits),
            _ => Err(Error::new(ErrorKind::TooManyRequests, "MEDIA_BUSY", "The media engine is busy; try later")),
        }
    }
}

/// The JPEG inside a RAW photo; none for other files.
async fn raw_preview(input: &std::fs::File) -> Result<Option<raw::Preview>> {
    let file = input.try_clone().map_err(Error::internal)?;
    tokio::task::spawn_blocking(move || raw::find(&file)).await.map_err(Error::internal)
}

/// Back to the start of the file, which reading its tags moved.
fn rewind(file: &std::fs::File) -> Result<()> {
    use std::io::Seek;
    let mut file = file;
    file.seek(std::io::SeekFrom::Start(0)).map(drop).map_err(Error::internal)
}

pub(crate) fn unsupported(message: impl Into<String>) -> Error {
    Error::bad_request("MEDIA_UNSUPPORTED", message)
}

pub(crate) fn failed(message: impl Into<String>) -> Error {
    Error::bad_request("MEDIA_FAILED", message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn batches_of_jobs_leave_a_process_for_requests() {
        let settings = MediaSettings { workers: 2, ..MediaSettings::default() };
        let launcher = Launcher { program: "/nonexistent".into(), prefix: Vec::new() };
        let media = Media::start(launcher, None, Arc::new(Live::new(settings))).await;
        let _first = media.slot(Priority::Background).await.unwrap();
        let second = tokio::time::timeout(Duration::from_millis(100), media.slot(Priority::Background)).await;
        assert!(second.is_err(), "a second batch waits");
        let request = tokio::time::timeout(Duration::from_millis(100), media.slot(Priority::Request)).await;
        assert!(request.is_ok_and(|slot| slot.is_ok()), "a request gets the process kept for it");
    }
}
