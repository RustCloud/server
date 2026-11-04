//! The engine with the real tools: set RUSTCLOUD_TEST_MEDIA_TOOLS to a folder
//! with the static ffmpeg and ffprobe of docker/build-ffmpeg.sh. Without it
//! only what needs no tools is tested. The samples were made for these tests
//! (ffmpeg testsrc2, heif-enc, exiftool).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustcloud_core::Live;
use rustcloud_media::{
    Crop, Fit, ImageFormat, Launcher, Media, MediaSettings, PictureOptions, PixelFit, PixelOptions, Priority,
    SampleOptions, StreamMode,
};

fn launcher() -> Launcher {
    Launcher { program: PathBuf::from(env!("CARGO_BIN_EXE_media-sandbox")), prefix: Vec::new() }
}

fn sample(name: &str) -> std::fs::File {
    std::fs::File::open(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples").join(name)).unwrap()
}

/// The engine with the tools, or none (and a note) if the tests have none.
async fn media() -> Option<Media> {
    let Some(dir) = std::env::var_os("RUSTCLOUD_TEST_MEDIA_TOOLS") else {
        eprintln!("RUSTCLOUD_TEST_MEDIA_TOOLS is not set: skipped");
        return None;
    };
    let media = Media::start(launcher(), Some(Path::new(&dir)), Arc::new(Live::new(MediaSettings::default()))).await;
    assert!(media.status().available, "{:?}", media.status());
    Some(media)
}

/// What a plugin at ×1 takes at once.
const LIMIT: u64 = 16 << 20;

fn options(width: u32, height: u32, fit: Fit, format: ImageFormat) -> PictureOptions {
    PictureOptions { max_width: width, max_height: height, fit, format, quality: 80, at_ms: None }
}

#[tokio::test]
async fn without_tools_the_engine_says_so() {
    let media = Media::start(launcher(), None, Arc::new(Live::new(MediaSettings::default()))).await;
    assert!(!media.status().available);
    assert!(!media.capabilities().available);
    assert_eq!(media.metadata(sample("photo.jpg"), Priority::Request).await.unwrap_err().code, "MEDIA_UNSUPPORTED");
}

#[tokio::test]
async fn photos_tell_when_where_and_with_what_they_were_taken() {
    let Some(media) = media().await else { return };
    let photo = media.metadata(sample("photo.jpg"), Priority::Request).await.unwrap();
    assert_eq!((photo.container.as_str(), photo.mime_type.as_str(), photo.still), ("jpeg", "image/jpeg", true));
    assert_eq!((photo.width, photo.height, photo.duration_ms), (Some(640), Some(480), None));
    assert_eq!(photo.taken_at.as_deref(), Some("2026-05-17T10:30:00+03:00"));
    let place = photo.location.unwrap();
    assert!((place.latitude - 55.7558).abs() < 1e-3 && (place.longitude - 37.6173).abs() < 1e-3, "{place:?}");
    assert_eq!(place.altitude.map(f64::round), Some(150.0));
    assert_eq!(photo.camera_make.as_deref(), Some("RustCloud"));
    assert_eq!(photo.camera_model.as_deref(), Some("Test Camera"));
    assert_eq!(photo.lens.as_deref(), Some("Test Lens 50mm"));

    let heic = media.metadata(sample("photo.heic"), Priority::Request).await.unwrap();
    assert_eq!((heic.container.as_str(), heic.mime_type.as_str(), heic.still), ("heif", "image/heic", true));
    assert_eq!((heic.width, heic.height), (Some(640), Some(480)));
}

#[tokio::test]
async fn videos_are_read_whole_even_with_their_index_at_the_end() {
    let Some(media) = media().await else { return };
    let video = media.metadata(sample("video.mp4"), Priority::Request).await.unwrap();
    assert_eq!((video.container.as_str(), video.still), ("mp4", false));
    assert!(video.duration_ms.is_some_and(|ms| (3900..=4100).contains(&ms)), "{:?}", video.duration_ms);
    assert_eq!((video.width, video.height), (Some(240), Some(320)), "as shown: turned");
    assert_eq!(video.video[0].codec, "h264");
    assert_eq!(video.video[0].rotation % 180, 90);
    assert_eq!(video.audio[0].codec, "aac");
    assert!(video.taken_at.as_deref().is_some_and(|at| at.starts_with("2026-05-17T10:31:00")), "{:?}", video.taken_at);
}

#[tokio::test]
async fn thumbnails_of_photos_and_videos() {
    let Some(media) = media().await else { return };
    let square = media
        .thumbnail(sample("photo.heic"), &options(100, 100, Fit::Cover, ImageFormat::Jpeg), LIMIT, Priority::Request)
        .await
        .unwrap();
    assert!(square.starts_with(&[0xFF, 0xD8, 0xFF]));
    let frame = media
        .thumbnail(sample("video.mp4"), &options(160, 160, Fit::Inside, ImageFormat::Webp), LIMIT, Priority::Request)
        .await
        .unwrap();
    assert_eq!(&frame[8..12], b"WEBP");
    let png = media
        .thumbnail(sample("photo.jpg"), &options(64, 64, Fit::Inside, ImageFormat::Png), LIMIT, Priority::Background)
        .await
        .unwrap();
    assert!(png.starts_with(b"\x89PNG"));
    let small = media
        .thumbnail(sample("photo.jpg"), &options(64, 64, Fit::Inside, ImageFormat::Png), 100, Priority::Request)
        .await;
    assert_eq!(small.unwrap_err().code, "MEDIA_FAILED", "larger than the plugin takes");
}

#[tokio::test]
async fn bad_requests_and_broken_files_are_refused() {
    let Some(media) = media().await else { return };
    let bad = PictureOptions { quality: 0, ..options(100, 100, Fit::Inside, ImageFormat::Jpeg) };
    assert_eq!(
        media.thumbnail(sample("photo.jpg"), &bad, LIMIT, Priority::Request).await.unwrap_err().code,
        "INVALID_OPTIONS"
    );
    let huge = options(100_000, 100, Fit::Inside, ImageFormat::Jpeg);
    assert_eq!(
        media.thumbnail(sample("photo.jpg"), &huge, LIMIT, Priority::Request).await.unwrap_err().code,
        "INVALID_OPTIONS"
    );

    let dir = std::env::temp_dir().join(format!("rustcloud-media-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let junk = dir.join("junk.mp4");
    std::fs::write(&junk, b"this is not a video at all").unwrap();
    let err = media.metadata(std::fs::File::open(&junk).unwrap(), Priority::Request).await.unwrap_err();
    assert_eq!(err.code, "MEDIA_FAILED", "{err:?}");
    std::fs::remove_dir_all(dir).unwrap();
}

/// RAW photos from real cameras: set RUSTCLOUD_TEST_RAW_SAMPLES to a folder
/// of them (raw.pixls.us has some for every camera); too large to keep here.
#[tokio::test]
async fn raw_photos_get_pictures_from_the_jpeg_inside() {
    let Some(media) = media().await else { return };
    let Some(dir) = std::env::var_os("RUSTCLOUD_TEST_RAW_SAMPLES") else {
        eprintln!("RUSTCLOUD_TEST_RAW_SAMPLES is not set: skipped");
        return;
    };
    let mut seen = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let info = media.metadata(std::fs::File::open(&path).unwrap(), Priority::Request).await;
        let info = info.unwrap_or_else(|err| panic!("{}: {err:?}", path.display()));
        let picture = options(320, 320, Fit::Inside, ImageFormat::Jpeg);
        let thumbnail = media.thumbnail(std::fs::File::open(&path).unwrap(), &picture, LIMIT, Priority::Request);
        let thumbnail = thumbnail.await.unwrap_or_else(|err| panic!("{}: {err:?}", path.display()));
        eprintln!(
            "{}: {} {}×{} {:?} {:?}, thumbnail {} bytes",
            path.display(),
            info.container,
            info.width.unwrap(),
            info.height.unwrap(),
            info.camera_model,
            info.taken_at,
            thumbnail.len()
        );
        assert!(info.still && thumbnail.starts_with(&[0xFF, 0xD8, 0xFF]));
        // To look at them: RUSTCLOUD_TEST_RAW_OUT, a folder.
        if let Some(out) = std::env::var_os("RUSTCLOUD_TEST_RAW_OUT") {
            let name = format!("{}.jpg", path.file_name().unwrap().to_string_lossy());
            std::fs::write(Path::new(&out).join(name), &thumbnail).unwrap();
        }
        seen += 1;
    }
    assert!(seen > 0);
}

#[tokio::test]
async fn videos_stream_in_pieces_that_line_up() {
    let Some(media) = media().await else { return };
    // H.264 in MKV, repacked and re-encoded; HEVC (8 s, no sound) re-encoded.
    // The samples have a keyframe every 2 s.
    let cases =
        [("long.mkv", StreamMode::Copy, 3), ("long.mkv", StreamMode::H264, 3), ("hevc.mkv", StreamMode::H264, 2)];
    for (name, mode, count) in cases {
        let original = media.metadata(sample(name), Priority::Request).await.unwrap();
        let pieces = media.stream_pieces(sample(name), mode, Priority::Request).await.unwrap();
        assert_eq!(pieces.len(), count, "{name} {mode:?}: {pieces:?}");
        let total: u64 = pieces.iter().map(|p| p.duration_ms).sum();
        assert_eq!(total, original.duration_ms.unwrap(), "{name} {mode:?}: the pieces cover the video");
        // The init of the first piece and the media of all of them make the whole video.
        let mut whole = Vec::new();
        for (index, piece) in pieces.iter().enumerate() {
            let (init, part) = media.stream_piece(sample(name), mode, *piece, Priority::Request).await.unwrap();
            assert_eq!(&init[4..8], b"ftyp");
            assert_eq!(&part[4..8], b"moof", "{name} {mode:?} piece {index}");
            if index == 0 {
                whole.extend(init);
            }
            whole.extend(part);
        }
        let mut file = tempfile::tempfile().unwrap();
        std::io::Write::write_all(&mut file, &whole).unwrap();
        let streamed = media.metadata(file, Priority::Request).await.unwrap();
        assert_eq!(streamed.video[0].codec, "h264", "{name} {mode:?}");
        assert_eq!(streamed.audio.len(), original.audio.len(), "{name} {mode:?}");
        let (a, b) = (streamed.duration_ms.unwrap() as i64, original.duration_ms.unwrap() as i64);
        assert!((a - b).abs() < 300, "{name} {mode:?}: {a} ms streamed of {b}");
    }
}

#[tokio::test]
async fn pixels_for_neural_networks_are_exactly_the_size_asked() {
    let Some(media) = media().await else { return };
    let options = |fit, crop| PixelOptions { width: 64, height: 64, fit, crop };

    let stretched =
        media.pixels(sample("photo.jpg"), &options(PixelFit::Stretch, None), Priority::Request).await.unwrap();
    assert_eq!((stretched.width, stretched.height, stretched.rgb.len()), (640, 480, 64 * 64 * 3));

    // 640×480 inside 64×64: 64×48 at the top, black below.
    let inside = media.pixels(sample("photo.jpg"), &options(PixelFit::Inside, None), Priority::Request).await.unwrap();
    let row = |y: usize| &inside.rgb[y * 64 * 3..(y + 1) * 64 * 3];
    assert!(row(56).iter().all(|&v| v == 0), "padded with black");
    assert!(row(10).iter().any(|&v| v > 0), "the picture at the top");

    let heic = media.pixels(sample("photo.heic"), &options(PixelFit::Cover, None), Priority::Request).await.unwrap();
    assert_eq!(heic.rgb.len(), 64 * 64 * 3);

    let half = Crop { x: 0, y: 0, width: 320, height: 240 };
    assert!(media
        .pixels(sample("photo.jpg"), &options(PixelFit::Stretch, Some(half)), Priority::Request)
        .await
        .is_ok());
    let outside = Crop { x: 600, y: 0, width: 100, height: 100 };
    let err = media.pixels(sample("photo.jpg"), &options(PixelFit::Stretch, Some(outside)), Priority::Request).await;
    assert_eq!(err.unwrap_err().code, "INVALID_OPTIONS");

    // A video's frame, turned as shown.
    let frame = media.pixels(sample("video.mp4"), &options(PixelFit::Stretch, None), Priority::Request).await.unwrap();
    assert_eq!((frame.width, frame.height), (240, 320));
}

#[tokio::test]
async fn sound_for_neural_networks_is_mono_floats_of_a_part() {
    let Some(media) = media().await else { return };
    let options = SampleOptions { rate: 16000, start_ms: 1000, duration_ms: Some(1000) };
    let second = media.samples(sample("video.mp4"), &options, LIMIT, Priority::Request).await.unwrap();
    assert!((15_000 * 4..=16_500 * 4).contains(&second.len()), "{} bytes", second.len());
    assert!(second.as_chunks::<4>().0.iter().all(|b| f32::from_le_bytes(*b).abs() <= 1.0));

    let all = SampleOptions { start_ms: 0, duration_ms: None, ..options };
    let too_long = media.samples(sample("video.mp4"), &all, 16_000 * 4, Priority::Request).await.unwrap_err();
    assert!(too_long.message.contains("ask for a part"), "{}", too_long.message);

    let silent = media.samples(sample("photo.jpg"), &options, LIMIT, Priority::Request).await.unwrap_err();
    assert_eq!(silent.code, "MEDIA_UNSUPPORTED");
}
