//! Videos a browser can't play as they are, streamed as HLS of fragmented
//! MP4: the server makes each piece when the player asks for it.
//!
//! - [`StreamMode::Copy`] repacks without re-encoding: an H.264 or HEVC video
//!   in MKV, AVI or MPEG-TS. Pieces start on the video's keyframes, which
//!   ffprobe lists from the packets, without decoding.
//! - [`StreamMode::H264`] re-encodes to H.264 (OpenH264, BSD) at most 720p:
//!   HEVC where the browser has no decoder, MPEG-2, VP9 in Safari… Pieces
//!   are every [`PIECE`]; each starts with a keyframe of its own.
//!
//! Sound is AAC in both. Every piece keeps the video's own timestamps, so
//! pieces made by separate runs line up, and one init segment (`ftyp` and
//! `moov`) fits them all.

use std::ffi::OsString;
use std::time::Duration;

use rustcloud_core::{Error, Result};
use serde::{Deserialize, Serialize};

/// How long a piece is, about: what HLS players expect.
pub const PIECE: Duration = Duration::from_secs(6);
/// The tallest re-encoded picture.
const MAX_HEIGHT: u32 = 720;
/// Most of ffprobe's list of packets: a two-hour film has ~200 000.
pub(crate) const PACKETS_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamMode {
    /// Repacked, not re-encoded.
    Copy,
    /// Re-encoded to H.264.
    H264,
}

/// One piece of a stream, in the video's own time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    pub start_ms: u64,
    pub duration_ms: u64,
    /// Where ffmpeg stops reading: for `Copy`, just before the decoding time
    /// of the next piece's keyframe, so no frame is in two pieces; none at
    /// the end.
    pub until_ms: Option<u64>,
}

/// A keyframe: when it is shown, and when it is decoded (earlier, with
/// B-frames).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Keyframe {
    pub pts_ms: u64,
    pub dts_ms: u64,
}

/// The pieces of a video of `duration_ms` from `first_ms`: every [`PIECE`],
/// or for `Copy` on the `keyframes` (ascending) at least [`PIECE`] apart.
pub(crate) fn pieces(mode: StreamMode, keyframes: &[Keyframe], first_ms: u64, duration_ms: u64) -> Vec<Piece> {
    let piece = PIECE.as_millis() as u64;
    let end = first_ms + duration_ms;
    // Where each piece starts, and where the one before it stops reading.
    let mut starts: Vec<(u64, Option<u64>)> = vec![(first_ms, None)];
    match mode {
        StreamMode::H264 => {
            let more = (1..).map(|i| first_ms + i * piece).take_while(|start| *start + 500 < end);
            starts.extend(more.map(|start| (start, Some(start))));
        }
        StreamMode::Copy => {
            for keyframe in keyframes {
                if keyframe.pts_ms >= starts[starts.len() - 1].0 + piece && keyframe.pts_ms + 500 < end {
                    starts.push((keyframe.pts_ms, Some(keyframe.dts_ms.saturating_sub(1))));
                }
            }
        }
    }
    let ends = starts.iter().skip(1).copied().chain([(end, None)]);
    starts
        .iter()
        .zip(ends)
        .map(|(&(start, _), (end, until_ms))| Piece { start_ms: start, duration_ms: end - start, until_ms })
        .collect()
}

/// ffprobe's list of the first video stream's packets: `pts_time,dts_time,flags`.
pub(crate) fn packets_args() -> Vec<OsString> {
    let args = ["-v", "error", "-protocol_whitelist", "fd", "-select_streams", "v:0"];
    let rest = ["-show_entries", "packet=pts_time,dts_time,flags", "-of", "csv=p=0", "-i", "fd:"];
    args.into_iter().chain(rest).map(OsString::from).collect()
}

/// The keyframes in ffprobe's list, ascending.
pub(crate) fn keyframes(list: &[u8]) -> Vec<Keyframe> {
    let ms = |time: &str| time.parse::<f64>().ok().filter(|t| *t >= 0.0).map(|t| (t * 1000.0).round() as u64);
    let mut frames: Vec<Keyframe> = String::from_utf8_lossy(list)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(',');
            let (pts, dts, flags) = (fields.next()?, fields.next()?, fields.next()?);
            let pts_ms = ms(pts)?;
            flags.starts_with('K').then(|| Keyframe { pts_ms, dts_ms: ms(dts).unwrap_or(pts_ms).min(pts_ms) })
        })
        .collect();
    frames.sort_unstable();
    frames.dedup_by_key(|frame| frame.pts_ms);
    frames
}

/// How much past a keyframe a repacked piece seeks: ffmpeg seeks a little
/// before the asked time when there are B-frames (3/23 s) and, copying,
/// keeps everything from the keyframe it lands on.
const PAST_KEYFRAME_MS: u64 = 200;

/// The ffmpeg command for a piece: fragmented MP4 to the pipe, the video's
/// own timestamps kept, so pieces of separate runs line up. Numbers and
/// fixed words only.
pub(crate) fn piece_args(mode: StreamMode, piece: Piece, max_pixels: u64) -> Vec<OsString> {
    let time = |ms: u64| format!("{}.{:03}", ms / 1000, ms % 1000);
    let mut args: Vec<String> =
        ["-v", "error", "-nostdin", "-protocol_whitelist", "fd", "-threads", "2", "-copyts"].map(str::to_owned).into();
    args.extend(["-max_pixels".into(), max_pixels.to_string()]);
    let seek = match (mode, piece.start_ms) {
        (_, 0) => 0,
        // From the keyframe, and the sound from there too, not from the seek.
        (StreamMode::Copy, start) => {
            args.push("-noaccurate_seek".into());
            start + PAST_KEYFRAME_MS
        }
        (StreamMode::H264, start) => start,
    };
    args.extend(["-ss".into(), time(seek)]);
    if let Some(until) = piece.until_ms {
        args.extend(["-to".into(), time(until)]);
    }
    args.extend(["-i".into(), "fd:".into()]);
    args.extend(["-map", "0:v:0", "-map", "0:a:0?", "-sn", "-dn", "-map_metadata", "-1"].map(str::to_owned));
    match mode {
        StreamMode::Copy => args.extend(["-c:v".into(), "copy".into()]),
        StreamMode::H264 => args.extend([
            "-vf".into(),
            format!("scale=-2:trunc(min({MAX_HEIGHT}\\,ih)/2)*2,format=yuv420p"),
            "-c:v".into(),
            "libopenh264".into(),
            "-b:v".into(),
            "2500k".into(),
            "-maxrate".into(),
            "3500k".into(),
        ]),
    }
    args.extend(
        ["-c:a", "aac", "-b:a", "160k", "-ac", "2", "-avoid_negative_ts", "make_non_negative"].map(str::to_owned),
    );
    let flags = "frag_keyframe+empty_moov+default_base_moof+frag_discont";
    args.extend(["-f", "mp4", "-movflags", flags, "-use_editlist", "0", "pipe:1"].map(str::to_owned));
    args.into_iter().map(OsString::from).collect()
}

/// A fragmented MP4 split into its init segment (`ftyp`, `moov`) and the
/// rest (`moof`, `mdat`…).
pub(crate) fn split_init(mp4: Vec<u8>) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut at = 0;
    while at + 8 <= mp4.len() {
        let size = u32::from_be_bytes([mp4[at], mp4[at + 1], mp4[at + 2], mp4[at + 3]]) as usize;
        let kind = &mp4[at + 4..at + 8];
        if kind == b"moof" {
            let mut init = mp4;
            let media = init.split_off(at);
            return Ok((init, media));
        }
        if size < 8 {
            break;
        }
        at += size;
    }
    Err(Error::bad_request("MEDIA_FAILED", "no video came out"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces_follow_keyframes_or_the_clock() {
        let keyframes: Vec<Keyframe> = [0, 2000, 4000, 6500, 9000, 13000, 19500]
            .map(|pts_ms| Keyframe { pts_ms, dts_ms: pts_ms.saturating_sub(80) })
            .into();
        let copy = pieces(StreamMode::Copy, &keyframes, 0, 20_000);
        let cuts: Vec<(u64, Option<u64>)> = copy.iter().map(|p| (p.start_ms, p.until_ms)).collect();
        assert_eq!(cuts, [(0, Some(6419)), (6500, Some(12919)), (13000, None)], "until the next keyframe's decoding");
        assert_eq!(copy.last().unwrap().duration_ms, 7000, "no piece of half a second at the end");
        let h264 = pieces(StreamMode::H264, &[], 1400, 13_000);
        let spans: Vec<(u64, u64)> = h264.iter().map(|p| (p.start_ms, p.duration_ms)).collect();
        assert_eq!(spans, [(1400, 6000), (7400, 6000), (13400, 1000)]);
        let one = pieces(StreamMode::Copy, &[], 0, 3000);
        assert_eq!(one, [Piece { start_ms: 0, duration_ms: 3000, until_ms: None }]);
    }

    #[test]
    fn keyframes_come_from_the_packets() {
        let list = b"0.000000,N/A,K__\n0.033367,0.0,___\n2.002000,1.922000,K_\n\nN/A,N/A,K__\n-0.04,-0.1,K__\n";
        let found = keyframes(list);
        assert_eq!(found, [Keyframe { pts_ms: 0, dts_ms: 0 }, Keyframe { pts_ms: 2002, dts_ms: 1922 }]);
    }

    #[test]
    fn a_repacked_piece_seeks_past_its_keyframe_and_stops_before_the_next() {
        let piece = Piece { start_ms: 6000, duration_ms: 6000, until_ms: Some(11919) };
        let args: Vec<String> =
            piece_args(StreamMode::Copy, piece, 1000).into_iter().map(|a| a.into_string().unwrap()).collect();
        let at = |flag: &str| args[args.iter().position(|a| a == flag).unwrap() + 1].clone();
        assert_eq!((at("-ss"), at("-to"), at("-c:v")), ("6.200".into(), "11.919".into(), "copy".into()));
        assert!(args.contains(&"-noaccurate_seek".into()) && args.contains(&"-copyts".into()));
    }

    #[test]
    fn a_fragment_splits_into_init_and_media() {
        let mut mp4 = Vec::new();
        for (kind, size) in [(b"ftyp", 16u32), (b"moov", 24), (b"moof", 12), (b"mdat", 10)] {
            mp4.extend(size.to_be_bytes());
            mp4.extend(kind);
            mp4.resize(mp4.len() + size as usize - 8, 0);
        }
        let (init, media) = split_init(mp4).unwrap();
        assert_eq!((init.len(), media.len()), (40, 22));
        assert_eq!(&media[4..8], b"moof");
        assert!(split_init(b"\0\0\0\x08ftyp".to_vec()).is_err());
    }
}
