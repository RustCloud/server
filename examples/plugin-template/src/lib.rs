//! A plugin that tells what a photo or video is — date, place, camera — and
//! shows its thumbnail, through the server's ffmpeg (`use.ffmpeg`).
//!
//! `GET /app/photo-info/?<file id>`: the metadata as JSON-like text;
//! `GET /app/photo-info/thumbnail?<file id>`: a 320×320 WebP.

wit_bindgen::generate!({ path: "wit", world: "app" });

use rustcloud::app::use_ffmpeg::{self, Fit, ImageFormat, PictureOptions, Source};

struct PhotoInfo;

impl Guest for PhotoInfo {
    fn handle(_user: Option<User>, request: Request) -> Response {
        let file = Source::File(request.query.clone());
        match request.path.as_str() {
            "/" => match use_ffmpeg::get_metadata(&file) {
                Ok(info) => text(
                    200,
                    format!(
                        "{} {}×{}, taken {}, at {:?}, with {} {}",
                        info.container,
                        info.width.unwrap_or(0),
                        info.height.unwrap_or(0),
                        info.taken_at.unwrap_or_default(),
                        info.location.map(|l| (l.latitude, l.longitude)),
                        info.camera_make.unwrap_or_default(),
                        info.camera_model.unwrap_or_default(),
                    ),
                ),
                Err(err) => text(500, format!("{err:?}")),
            },
            "/thumbnail" => {
                let options = PictureOptions {
                    max_width: 320,
                    max_height: 320,
                    fit: Fit::Cover,
                    format: ImageFormat::Webp,
                    quality: 80,
                    at_ms: None,
                };
                match use_ffmpeg::thumbnail(&file, options) {
                    Ok(picture) => Response {
                        status: 200,
                        headers: vec![("content-type".to_owned(), "image/webp".to_owned())],
                        body: picture,
                    },
                    Err(err) => text(500, format!("{err:?}")),
                }
            }
            _ => text(404, "not found"),
        }
    }
}

fn text(status: u16, body: impl Into<String>) -> Response {
    Response {
        status,
        headers: vec![("content-type".to_owned(), "text/plain; charset=utf-8".to_owned())],
        body: body.into().into_bytes(),
    }
}

export!(PhotoInfo);
