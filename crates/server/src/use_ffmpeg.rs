//! `use-ffmpeg` for plugins: the media engine ([`rustcloud_media`]) behind
//! the WIT import, and its state on the admin page.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use rustcloud_auth::AdminUser;
use rustcloud_core::Result;
use rustcloud_media::{Fit, ImageFormat, Media, MediaInfo, PictureOptions, Priority, Status};
use rustcloud_wasm_host::{ffmpeg, UseFfmpeg};
use serde::Serialize;

use crate::AppState;

/// The engine as the WASM host sees it.
pub struct Ffmpeg(pub Arc<Media>);

#[async_trait]
impl UseFfmpeg for Ffmpeg {
    fn capabilities(&self) -> ffmpeg::Capabilities {
        let found = self.0.capabilities();
        ffmpeg::Capabilities {
            available: found.available,
            ffmpeg: found.ffmpeg,
            image_formats: found.image_formats,
            video_codecs: found.video_codecs,
            audio_codecs: found.audio_codecs,
        }
    }

    async fn metadata(&self, file: std::fs::File, background: bool) -> Result<ffmpeg::Metadata> {
        self.0.metadata(file, priority(background)).await.map(metadata)
    }

    async fn thumbnail(
        &self,
        file: std::fs::File,
        options: ffmpeg::PictureOptions,
        limit: u64,
        background: bool,
    ) -> Result<Vec<u8>> {
        let options = PictureOptions {
            max_width: options.max_width,
            max_height: options.max_height,
            fit: match options.fit {
                ffmpeg::Fit::Inside => Fit::Inside,
                ffmpeg::Fit::Cover => Fit::Cover,
            },
            format: match options.format {
                ffmpeg::ImageFormat::Jpeg => ImageFormat::Jpeg,
                ffmpeg::ImageFormat::Webp => ImageFormat::Webp,
                ffmpeg::ImageFormat::Png => ImageFormat::Png,
            },
            quality: options.quality,
            at_ms: options.at_ms,
        };
        self.0.thumbnail(file, &options, limit, priority(background)).await
    }
}

fn priority(background: bool) -> Priority {
    if background {
        Priority::Background
    } else {
        Priority::Request
    }
}

fn metadata(info: MediaInfo) -> ffmpeg::Metadata {
    ffmpeg::Metadata {
        container: info.container,
        mime_type: info.mime_type,
        still: info.still,
        duration_ms: info.duration_ms,
        width: info.width,
        height: info.height,
        video: info
            .video
            .into_iter()
            .map(|v| ffmpeg::VideoStream {
                codec: v.codec,
                width: v.width,
                height: v.height,
                fps: v.fps,
                rotation: v.rotation as u16,
                bit_depth: v.bit_depth,
                hdr: v.hdr,
                bitrate: v.bitrate,
            })
            .collect(),
        audio: info
            .audio
            .into_iter()
            .map(|a| ffmpeg::AudioStream {
                codec: a.codec,
                channels: a.channels,
                sample_rate: a.sample_rate,
                bitrate: a.bitrate,
                language: a.language,
            })
            .collect(),
        subtitles: info
            .subtitles
            .into_iter()
            .map(|s| ffmpeg::SubtitleStream { codec: s.codec, language: s.language, title: s.title })
            .collect(),
        taken_at: info.taken_at,
        location: info.location.map(|l| ffmpeg::Location {
            latitude: l.latitude,
            longitude: l.longitude,
            altitude: l.altitude,
        }),
        camera_make: info.camera_make,
        camera_model: info.camera_model,
        lens: info.lens,
        tags: info.tags,
    }
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/admin/use/ffmpeg", get(status))
}

/// The state of `use.ffmpeg` on this server.
#[derive(Serialize)]
struct FfmpegStatus {
    #[serde(flatten)]
    status: Status,
    capabilities: rustcloud_media::Capabilities,
}

/// `GET /api/v1/admin/use/ffmpeg`: whether ffmpeg is there, its version and
/// what confines it.
async fn status(State(media): State<Arc<Media>>, _admin: AdminUser) -> Json<FfmpegStatus> {
    Json(FfmpegStatus { status: media.status().clone(), capabilities: media.capabilities() })
}
