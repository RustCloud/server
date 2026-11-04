//! RAW photos (CR2, NEF, ARW, DNG, CR3…): ffmpeg can't develop them, but
//! every camera puts a JPEG of the shot inside, usually as large as the
//! photo. This finds the largest one with a small parser of TIFF tags (CR2,
//! NEF, ARW, DNG and most others are TIFF inside) and of Canon's CR3 boxes;
//! ffmpeg then makes the picture from that JPEG.
//!
//! The parser reads at offsets the file gives, so every offset and length is
//! checked against the file and a few limits; anything odd means "no
//! preview", never a panic.

use std::fs::File;
use std::os::unix::fs::FileExt;

/// The largest preview taken.
const MAX_PREVIEW: u64 = 64 * 1024 * 1024;
/// Tag folders (IFDs) read at most, and tags in one.
const MAX_IFDS: usize = 32;
const MAX_ENTRIES: u16 = 1000;
/// Where in a CR3 file its preview is looked for.
const CR3_SEARCH: usize = 4 * 1024 * 1024;
/// How far into a JPEG its size is looked for (past its EXIF and thumbnails).
const JPEG_HEADER: usize = 256 * 1024;

/// The JPEG inside a RAW photo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Preview {
    /// `cr2`, `cr3`, `dng`, `nef`, `arw`, `raf`, `rw2`, `orf`, `pef`, or `raw`.
    pub kind: &'static str,
    pub offset: u64,
    pub length: u64,
    pub width: u32,
    pub height: u32,
    /// EXIF orientation of the photo, 1–8.
    pub orientation: u16,
    /// The preview has an orientation of its own, which ffmpeg applies (an
    /// iPhone's DNG): it must not be turned once more.
    pub turns_itself: bool,
    /// What the TIFF tags say of the camera and the moment, for files whose
    /// EXIF `nom-exif` doesn't read (CR2, NEF, ARW).
    pub camera: Camera,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Camera {
    pub make: Option<String>,
    pub model: Option<String>,
    /// `2026-10-30T14:05:00`, without an offset: cameras write none here.
    pub taken_at: Option<String>,
}

impl Preview {
    /// Its size as shown, turned as the camera was held.
    pub fn shown(&self) -> (u32, u32) {
        match self.orientation {
            5..=8 => (self.height, self.width),
            _ => (self.width, self.height),
        }
    }

    /// How ffmpeg must turn the preview: 1 for not at all.
    pub fn turn(&self) -> u16 {
        if self.turns_itself {
            1
        } else {
            self.orientation
        }
    }

    /// The bytes of the JPEG.
    pub fn read(&self, file: &File) -> Option<Vec<u8>> {
        let mut data = vec![0; usize::try_from(self.length).ok()?];
        file.read_exact_at(&mut data, self.offset).ok()?;
        Some(data)
    }
}

/// The largest JPEG preview of a RAW photo; none for any other file.
pub(crate) fn find(file: &File) -> Option<Preview> {
    let size = file.metadata().ok()?.len();
    let mut head = [0u8; 16];
    file.read_exact_at(&mut head, 0).ok()?;
    if &head[4..12] == b"ftypcrx " {
        return cr3(file, size);
    }
    if head.starts_with(b"FUJIFILMCCD-RAW") {
        return raf(file, size);
    }
    // TIFF, and its variants: Olympus ORF, Panasonic RW2.
    let big_endian = match &head[..4] {
        b"II*\0" | b"IIRO" | b"IIRS" | b"IIU\0" => false,
        b"MM\0*" | b"MMOR" => true,
        _ => return None,
    };
    Tiff { file, size, big_endian }.preview(&head[8..10] == b"CR")
}

/// The preview of a CR3 file: the `PRVW` box, 1620×1080 or so.
fn cr3(file: &File, size: u64) -> Option<Preview> {
    let mut head = vec![0u8; CR3_SEARCH.min(usize::try_from(size).ok()?)];
    file.read_exact_at(&mut head, 0).ok()?;
    let at = head.windows(4).position(|w| w == b"PRVW")?;
    let field = |from: usize, len: usize| head.get(at + from..at + from + len);
    let u16_at = |from| field(from, 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let (width, height) = (u16_at(10)?, u16_at(12)?);
    let length = field(16, 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))?;
    let head = jpeg_head(file, (at + 20) as u64, length.into())?;
    let preview = Preview {
        kind: "cr3",
        offset: (at + 20) as u64,
        length: length.into(),
        width: width.into(),
        height: height.into(),
        orientation: 1,
        turns_itself: head.turns_itself,
        camera: Camera::default(),
    };
    (preview.offset + preview.length <= size && preview.length <= MAX_PREVIEW).then_some(preview)
}

/// The preview of a Fujifilm RAF: its offset and length are in the header.
fn raf(file: &File, size: u64) -> Option<Preview> {
    let mut header = [0u8; 8];
    file.read_exact_at(&mut header, 84).ok()?;
    let offset = u64::from(u32::from_be_bytes([header[0], header[1], header[2], header[3]]));
    let length = u64::from(u32::from_be_bytes([header[4], header[5], header[6], header[7]]));
    if length > MAX_PREVIEW || offset.saturating_add(length) > size {
        return None;
    }
    let head = jpeg_head(file, offset, length)?;
    Some(Preview {
        kind: "raf",
        offset,
        length,
        width: head.width,
        height: head.height,
        orientation: 1,
        turns_itself: head.turns_itself,
        camera: Camera::default(),
    })
}

struct Tiff<'a> {
    file: &'a File,
    size: u64,
    big_endian: bool,
}

/// One tag of an IFD.
struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    /// Where its value, or the offset of its value, is.
    at: u64,
}

impl Tiff<'_> {
    fn preview(&self, canon: bool) -> Option<Preview> {
        let mut queue = vec![u64::from(self.u32(4)?)];
        let mut seen = Vec::new();
        let (mut best, mut orientation, mut dng): (Option<Preview>, u16, bool) = (None, 1, false);
        while let Some(ifd) = queue.pop() {
            if seen.len() >= MAX_IFDS || seen.contains(&ifd) || ifd == 0 || ifd >= self.size {
                continue;
            }
            seen.push(ifd);
            let count = self.u16(ifd)?.min(MAX_ENTRIES);
            let entries: Vec<Entry> = (0..u64::from(count))
                .filter_map(|i| {
                    let at = ifd + 2 + i * 12;
                    Some(Entry { tag: self.u16(at)?, kind: self.u16(at + 2)?, count: self.u32(at + 4)?, at: at + 8 })
                })
                .collect();
            let first = |tag: u16| entries.iter().find(|e| e.tag == tag).and_then(|e| self.values(e).first().copied());
            if seen.len() == 1 {
                orientation = first(0x0112).map_or(1, |o| o as u16);
                // DNGVersion is four bytes, not a number.
                dng = entries.iter().any(|e| e.tag == 0xC612);
            }
            // Pointers to more IFDs: the next one, SubIFDs.
            queue.extend(self.u32(ifd + 2 + u64::from(count) * 12).map(u64::from));
            if let Some(sub) = entries.iter().find(|e| e.tag == 0x014A) {
                queue.extend(self.values(sub).into_iter().map(u64::from));
            }
            // A JPEG: as a thumbnail pointer, Panasonic's JpgFromRaw, or the
            // one strip of a JPEG image.
            let from_raw = entries.iter().find(|e| e.tag == 0x002E && e.kind == 7);
            let jpeg = match (first(0x0201), first(0x0202), from_raw) {
                (Some(offset), Some(length), _) => Some((offset, length)),
                (_, _, Some(blob)) => self.u32(blob.at).map(|offset| (offset, blob.count)),
                _ => match (first(0x0103), entries.iter().find(|e| e.tag == 0x0111), first(0x0117)) {
                    (Some(6 | 7), Some(strips), Some(length)) if strips.count == 1 => {
                        self.values(strips).first().map(|offset| (*offset, length))
                    }
                    _ => None,
                },
            };
            let Some((offset, length)) = jpeg else { continue };
            let (offset, length) = (u64::from(offset), u64::from(length));
            if length > MAX_PREVIEW || offset.saturating_add(length) > self.size {
                continue;
            }
            // Raw data compressed as lossless JPEG is not a picture ffmpeg shows.
            let Some(JpegHead { width, height, turns_itself }) = jpeg_head(self.file, offset, length) else {
                continue;
            };
            let larger = best
                .as_ref()
                .is_none_or(|b| u64::from(width) * u64::from(height) > u64::from(b.width) * u64::from(b.height));
            if larger {
                best = Some(Preview {
                    kind: "raw",
                    offset,
                    length,
                    width,
                    height,
                    orientation: 1,
                    turns_itself,
                    camera: Camera::default(),
                });
            }
        }
        // Olympus keeps its preview in the maker's notes.
        if let Some((offset, length)) = self.olympus() {
            if let Some(head) = jpeg_head(self.file, offset, length) {
                let larger = best.as_ref().is_none_or(|b| head.width * head.height > b.width * b.height);
                if larger && length <= MAX_PREVIEW {
                    best = Some(Preview {
                        kind: "orf",
                        offset,
                        length,
                        width: head.width,
                        height: head.height,
                        orientation: 1,
                        turns_itself: head.turns_itself,
                        camera: Camera::default(),
                    });
                }
            }
        }
        let mut preview = best?;
        preview.orientation = if (1..=8).contains(&orientation) { orientation } else { 1 };
        preview.camera = self.camera();
        let make = preview.camera.make.as_deref().unwrap_or_default().to_ascii_uppercase();
        preview.kind = match () {
            _ if dng => "dng",
            _ if canon => "cr2",
            _ if make.starts_with("NIKON") => "nef",
            _ if make.starts_with("SONY") => "arw",
            _ if make.starts_with("PANASONIC") => "rw2",
            _ if make.starts_with("OLYMPUS") || make.starts_with("OM DIGITAL") => "orf",
            _ if make.starts_with("PENTAX") || make.starts_with("RICOH") => "pef",
            _ => "raw",
        };
        Some(preview)
    }

    /// The camera and the moment: IFD0's Make and Model, the EXIF IFD's
    /// DateTimeOriginal.
    fn camera(&self) -> Camera {
        let Some(ifd0) = self.u32(4).map(u64::from) else { return Camera::default() };
        let exif = self.tag_at(ifd0, 0x8769).and_then(|at| self.u32(at + 8)).map(u64::from);
        let taken_at = exif.and_then(|exif| self.text_of(exif, 0x9003)).and_then(|date| {
            // `2026:10:30 14:05:00`
            let (day, time) = date.split_once(' ')?;
            let day = day.replace(':', "-");
            (day.len() == 10 && time.len() == 8 && !day.starts_with("0000")).then(|| format!("{day}T{time}"))
        });
        Camera { make: self.text_of(ifd0, 0x010F), model: self.text_of(ifd0, 0x0110), taken_at }
    }

    /// The preview in Olympus's maker notes: EXIF IFD → MakerNote
    /// (`OLYMPUS\0` and a TIFF of its own, offsets from its start) →
    /// CameraSettings → PreviewImageStart and PreviewImageLength.
    fn olympus(&self) -> Option<(u64, u64)> {
        let ifd0 = u64::from(self.u32(4)?);
        let exif = u64::from(self.u32(self.tag_at(ifd0, 0x8769)? + 8)?);
        let notes = u64::from(self.u32(self.tag_at(exif, 0x927C)? + 8)?);
        let mut magic = [0u8; 10];
        self.file.read_exact_at(&mut magic, notes).ok()?;
        if &magic[..8] != b"OLYMPUS\0" {
            return None;
        }
        let inner = Tiff { file: self.file, size: self.size, big_endian: &magic[8..10] == b"MM" };
        let settings = notes + u64::from(inner.u32(inner.tag_at(notes + 12, 0x2020)? + 8)?);
        let start = notes + u64::from(inner.u32(inner.tag_at(settings, 0x0101)? + 8)?);
        let length = u64::from(inner.u32(inner.tag_at(settings, 0x0102)? + 8)?);
        (start.saturating_add(length) <= self.size).then_some((start, length))
    }

    /// Where the entry of `tag` in an IFD is.
    fn tag_at(&self, ifd: u64, tag: u16) -> Option<u64> {
        let count = self.u16(ifd)?.min(MAX_ENTRIES);
        (0..u64::from(count)).map(|i| ifd + 2 + i * 12).find(|at| self.u16(*at) == Some(tag))
    }

    /// An ASCII tag of an IFD, up to 64 characters; none if empty.
    fn text_of(&self, ifd: u64, tag: u16) -> Option<String> {
        let at = self.tag_at(ifd, tag)?;
        let count = self.u32(at + 4)?;
        let from = if count <= 4 { at + 8 } else { u64::from(self.u32(at + 8)?) };
        let mut text = vec![0u8; count.min(64) as usize];
        self.file.read_exact_at(&mut text, from).ok()?;
        let text: String = String::from_utf8_lossy(&text).chars().filter(|c| !c.is_control()).collect();
        Some(text.trim().to_owned()).filter(|t| !t.is_empty())
    }

    /// The numbers of a SHORT or LONG tag, at most 64.
    fn values(&self, entry: &Entry) -> Vec<u32> {
        let width = match entry.kind {
            3 => 2,
            4 | 13 => 4,
            _ => return Vec::new(),
        };
        let count = entry.count.min(64) as u64;
        let from = if count * width <= 4 { Some(entry.at) } else { self.u32(entry.at).map(u64::from) };
        let Some(from) = from else { return Vec::new() };
        (0..count)
            .map_while(|i| match width {
                2 => self.u16(from + i * 2).map(u32::from),
                _ => self.u32(from + i * 4),
            })
            .collect()
    }

    fn u16(&self, at: u64) -> Option<u16> {
        let mut bytes = [0u8; 2];
        self.file.read_exact_at(&mut bytes, at).ok()?;
        Some(if self.big_endian { u16::from_be_bytes(bytes) } else { u16::from_le_bytes(bytes) })
    }

    fn u32(&self, at: u64) -> Option<u32> {
        let mut bytes = [0u8; 4];
        self.file.read_exact_at(&mut bytes, at).ok()?;
        Some(if self.big_endian { u32::from_be_bytes(bytes) } else { u32::from_le_bytes(bytes) })
    }
}

/// What the start of a JPEG says.
#[derive(Debug, PartialEq, Eq)]
struct JpegHead {
    width: u32,
    height: u32,
    /// It has an EXIF orientation of its own.
    turns_itself: bool,
}

/// The head of a baseline or progressive JPEG at `offset`; none for anything
/// else (lossless JPEG of raw data included).
fn jpeg_head(file: &File, offset: u64, length: u64) -> Option<JpegHead> {
    let mut head = vec![0u8; usize::try_from(length).ok()?.min(JPEG_HEADER)];
    file.read_exact_at(&mut head, offset).ok()?;
    parse_head(&head)
}

fn parse_head(jpeg: &[u8]) -> Option<JpegHead> {
    if !jpeg.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let (mut at, mut turns_itself) = (2, false);
    loop {
        // Fill bytes before a marker are allowed.
        while jpeg.get(at) == Some(&0xFF) && jpeg.get(at + 1) == Some(&0xFF) {
            at += 1;
        }
        let (&0xFF, &marker) = (jpeg.get(at)?, jpeg.get(at + 1)?) else { return None };
        let length = usize::from(u16::from_be_bytes([*jpeg.get(at + 2)?, *jpeg.get(at + 3)?]));
        match marker {
            // Baseline, extended, progressive.
            0xC0..=0xC2 => {
                let height = u16::from_be_bytes([*jpeg.get(at + 5)?, *jpeg.get(at + 6)?]);
                let width = u16::from_be_bytes([*jpeg.get(at + 7)?, *jpeg.get(at + 8)?]);
                return (width > 0 && height > 0).then_some(JpegHead {
                    width: width.into(),
                    height: height.into(),
                    turns_itself,
                });
            }
            // Lossless and other frames; the image data before any frame.
            0xC3..=0xCF if marker != 0xC4 && marker != 0xC8 && marker != 0xCC => return None,
            0xDA | 0xD9 => return None,
            0xE1 => {
                let segment = jpeg.get(at + 4..(at + 2 + length).min(jpeg.len()))?;
                turns_itself |= segment.strip_prefix(b"Exif\0\0").is_some_and(has_orientation);
                at += 2 + length;
            }
            _ => at += 2 + length,
        }
    }
}

/// Whether the first IFD of an EXIF block has an orientation tag.
fn has_orientation(tiff: &[u8]) -> bool {
    let big_endian = tiff.starts_with(b"MM");
    let u16_at = |at: usize| {
        let bytes = [*tiff.get(at)?, *tiff.get(at + 1)?];
        Some(if big_endian { u16::from_be_bytes(bytes) } else { u16::from_le_bytes(bytes) })
    };
    let u32_at = |at: usize| {
        let bytes = [*tiff.get(at)?, *tiff.get(at + 1)?, *tiff.get(at + 2)?, *tiff.get(at + 3)?];
        Some(if big_endian { u32::from_be_bytes(bytes) } else { u32::from_le_bytes(bytes) })
    };
    let Some(ifd) = u32_at(4).map(|ifd| ifd as usize) else { return false };
    let count = u16_at(ifd).unwrap_or(0).min(MAX_ENTRIES);
    (0..usize::from(count)).any(|i| u16_at(ifd + 2 + i * 12) == Some(0x0112))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A JPEG header of `width`×`height` with the frame marker `sof`.
    fn jpeg(sof: u8, width: u16, height: u16) -> Vec<u8> {
        let mut data = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, sof, 0x00, 0x08, 8];
        data.extend(height.to_be_bytes());
        data.extend(width.to_be_bytes());
        data.extend([1, 0xFF, 0xD9]);
        data
    }

    /// A little-endian TIFF: IFD0 with an orientation and a SubIFD with the
    /// JPEG preview; IFD1 with the raw data as one lossless-JPEG strip, which
    /// must be passed over.
    fn tiff(preview: &[u8], raw: &[u8], orientation: u16) -> Vec<u8> {
        let size = |entries: usize| (2 + entries * 12 + 4) as u32;
        let ifd0 = 8;
        let (ifd1, sub) = (ifd0 + size(2), ifd0 + size(2) + size(3));
        let (preview_at, raw_at) = (sub + size(2), sub + size(2) + preview.len() as u32);
        let mut file = b"II*\0".to_vec();
        file.extend(ifd0.to_le_bytes());
        let mut ifd = |entries: &[(u16, u16, u32)], next: u32| {
            file.extend((entries.len() as u16).to_le_bytes());
            for (tag, kind, value) in entries {
                file.extend(tag.to_le_bytes());
                file.extend(kind.to_le_bytes());
                file.extend(1u32.to_le_bytes());
                file.extend(value.to_le_bytes());
            }
            file.extend(next.to_le_bytes());
        };
        ifd(&[(0x0112, 3, orientation.into()), (0x014A, 4, sub)], ifd1);
        ifd(&[(0x0103, 3, 7), (0x0111, 4, raw_at), (0x0117, 4, raw.len() as u32)], 0);
        ifd(&[(0x0201, 4, preview_at), (0x0202, 4, preview.len() as u32)], 0);
        file.extend(preview);
        file.extend(raw);
        file
    }

    fn temp(bytes: &[u8]) -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file
    }

    #[test]
    fn the_largest_jpeg_inside_is_the_preview() {
        let preview = jpeg(0xC0, 6000, 4000);
        let file = temp(&tiff(&preview, &jpeg(0xC3, 6024, 4024), 6));
        let found = find(&file).expect("a preview");
        assert_eq!((found.length, found.width, found.height), (preview.len() as u64, 6000, 4000));
        assert_eq!((found.orientation, found.shown(), found.turn()), (6, (4000, 6000), 6), "held upright");
        assert_eq!(found.read(&file).unwrap(), preview);
    }

    #[test]
    fn other_files_and_broken_ones_have_none() {
        assert_eq!(find(&temp(&jpeg(0xC0, 10, 10))), None, "a JPEG is no RAW");
        assert_eq!(find(&temp(b"II*\0\xff\xff\xff\xff")), None, "IFD past the end");
        let looped = b"II*\0\x08\0\0\0\x00\0\x08\0\0\0".to_vec();
        assert_eq!(find(&temp(&looped)), None, "an IFD pointing to itself");
        let mut cut = tiff(&jpeg(0xC0, 600, 400), &[], 1);
        cut.truncate(cut.len() - 4);
        assert_eq!(find(&temp(&cut)), None, "a preview past the end");
        assert_eq!(parse_head(&jpeg(0xC3, 10, 10)), None, "lossless");
        let progressive = parse_head(&jpeg(0xC2, 10, 20)).unwrap();
        assert_eq!((progressive.width, progressive.height, progressive.turns_itself), (10, 20, false));
    }

    #[test]
    fn a_preview_with_its_own_orientation_is_not_turned_twice() {
        let mut exif = b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0".to_vec();
        let mut preview = vec![0xFF, 0xD8, 0xFF, 0xE1];
        preview.extend(((exif.len() + 2) as u16).to_be_bytes());
        preview.append(&mut exif);
        preview.extend(&jpeg(0xC0, 4032, 3024)[2..]);
        let found = find(&temp(&tiff(&preview, &[], 6))).unwrap();
        assert!(found.turns_itself);
        assert_eq!((found.shown(), found.turn()), ((3024, 4032), 1), "ffmpeg turns it");
    }
}
