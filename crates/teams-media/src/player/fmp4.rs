//! Fragmented MP4 as a DASH presentation's parts carry it: a track's initialization segment (its codec and timescale)
//! and its media segments (`moof` and `mdat` pairs: the samples, their times and their bytes), read into what the
//! player hands `GStreamer` (`crate::player::stream`). Only what a meeting recording's parts use is read: one track
//! per part, H.264 (`avc1`/`avc3` with its `avcC`) or AAC (`mp4a` with its `esds`); an encrypted sample entry
//! (`encv`, `enca`) or anything else is none.

use std::ops::Range;

/// What a track's samples are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Codec {
    /// H.264, its `avcC` record (`stream-format` `avc`, or `avc3` when its parameter sets ride in the samples).
    H264 {
        avcc: Vec<u8>,
        avc3: bool,
        width: u32,
        height: u32,
    },
    /// AAC, its `AudioSpecificConfig`.
    Aac { config: Vec<u8>, rate: u32, channels: u32 },
}

/// A track's initialization segment, as the player reads its media segments by it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackInit {
    /// The units of a second its samples' times count.
    pub timescale: u32,
    pub codec: Codec,
    /// Where in its media the track starts being shown (its edit list's first media time, as an encoder writes it for
    /// B-frames' delay): taken off every sample's times.
    pub shift: u64,
    defaults: Defaults,
}

/// A sample's defaults (`trex`, then a fragment's `tfhd`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Defaults {
    duration: u32,
    size: u32,
    flags: u32,
}

/// One sample of a media segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    /// When it is decoded, in the track's timescale.
    pub decode: u64,
    /// How much later than `decode` it is shown.
    pub offset: i64,
    pub duration: u32,
    /// A sync sample (a key frame): decoding can start at it.
    pub key: bool,
    /// Where its bytes are in the segment.
    pub bytes: Range<usize>,
}

/// `sample_is_non_sync_sample` in a sample's flags.
const NON_SYNC: u32 = 0x0001_0000;

/// The boxes of `bytes`: each one's type and body (`size` 1 is a 64-bit size, 0 runs to the end); stops at one
/// that doesn't fit.
fn boxes(bytes: &[u8]) -> impl Iterator<Item = ([u8; 4], Range<usize>)> + '_ {
    let mut at = 0_usize;
    std::iter::from_fn(move || {
        let head = bytes.get(at..at.checked_add(8)?)?;
        let size = u32::from_be_bytes(head[0..4].try_into().ok()?);
        let kind: [u8; 4] = head[4..8].try_into().ok()?;
        let (start, end) = match size {
            0 => (at + 8, bytes.len()),
            1 => {
                let large = bytes.get(at + 8..at + 16)?;
                let size = usize::try_from(u64::from_be_bytes(large.try_into().ok()?)).ok()?;
                (at + 16, at.checked_add(size)?)
            }
            size => (at + 8, at.checked_add(usize::try_from(size).ok()?)?),
        };
        if end < start || end > bytes.len() {
            return None;
        }
        let body = start..end;
        at = end;
        Some((kind, body))
    })
}

/// The body of the first box of type `kind` in `bytes`.
fn find(bytes: &[u8], kind: [u8; 4]) -> Option<&[u8]> {
    boxes(bytes)
        .find(|(found, _)| *found == kind)
        .and_then(|(_, body)| bytes.get(body))
}

/// The body of the box at `path` (each a child of the one before) in `bytes`.
fn find_path<'a>(bytes: &'a [u8], path: &[&[u8; 4]]) -> Option<&'a [u8]> {
    path.iter().try_fold(bytes, |within, kind| find(within, **kind))
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// A full box's version and flags, and the rest of its body.
fn full(body: &[u8]) -> Option<(u8, u32, &[u8])> {
    let head = u32_at(body, 0)?;
    let version = u8::try_from(head >> 24).ok()?;
    Some((version, head & 0x00ff_ffff, body.get(4..)?))
}

/// Reads a track's initialization segment (`ftyp`, `moov` with one `trak` and its `mvex`); `None` for one this
/// doesn't read.
#[must_use]
pub fn read_init(bytes: &[u8]) -> Option<TrackInit> {
    let moov = find(bytes, *b"moov")?;
    let trak = find(moov, *b"trak")?;
    let track_id = full(find(trak, *b"tkhd")?).and_then(|(version, _, rest)| {
        // creation and modification times (4 or 8 bytes each), then the track id
        u32_at(rest, if version == 1 { 16 } else { 8 })
    })?;
    let mdhd = full(find_path(trak, &[b"mdia", b"mdhd"])?)?;
    let timescale = u32_at(mdhd.2, if mdhd.0 == 1 { 16 } else { 8 }).filter(|scale| *scale > 0)?;
    let stsd = full(find_path(trak, &[b"mdia", b"minf", b"stbl", b"stsd"])?)?.2;
    // the entry count, then the first sample entry
    let entry = stsd.get(4..)?;
    let (kind, body) = boxes(entry).next()?;
    let codec = match &kind {
        b"avc1" | b"avc3" => video_entry(entry.get(body)?, kind == *b"avc3")?,
        b"mp4a" => audio_entry(entry.get(body)?)?,
        _ => return None,
    };
    let defaults = find(moov, *b"mvex")
        .and_then(|mvex| {
            boxes(mvex)
                .filter(|(kind, _)| kind == b"trex")
                .filter_map(|(_, body)| full(mvex.get(body)?))
                .find(|(_, _, rest)| u32_at(rest, 0) == Some(track_id))
        })
        .map(|(_, _, rest)| Defaults {
            duration: u32_at(rest, 8).unwrap_or(0),
            size: u32_at(rest, 12).unwrap_or(0),
            flags: u32_at(rest, 16).unwrap_or(0),
        })
        .unwrap_or_default();
    Some(TrackInit {
        timescale,
        codec,
        shift: find_path(trak, &[b"edts", b"elst"]).and_then(edit_start).unwrap_or(0),
        defaults,
    })
}

/// The media time an edit list's first edit that isn't empty starts at.
fn edit_start(elst: &[u8]) -> Option<u64> {
    let (version, _, rest) = full(elst)?;
    let count = usize::try_from(u32_at(rest, 0)?).ok()?;
    let size = if version == 1 { 20 } else { 12 };
    (0..count).find_map(|index| {
        let at = 4 + index * size;
        let time = if version == 1 {
            i64::from_be_bytes(rest.get(at + 8..at + 16)?.try_into().ok()?)
        } else {
            i64::from(i32::from_be_bytes(rest.get(at + 4..at + 8)?.try_into().ok()?))
        };
        // an empty edit (-1) shows nothing of the media
        u64::try_from(time).ok()
    })
}

/// A visual sample entry's size and its `avcC`.
fn video_entry(body: &[u8], inline_sets: bool) -> Option<Codec> {
    // 6 reserved, the data reference index, 16 predefined and reserved, then width and height; 78 bytes before its
    // boxes
    let width = u32::from(u16_at(body, 24)?);
    let height = u32::from(u16_at(body, 26)?);
    let record = find(body.get(78..)?, *b"avcC")?.to_vec();
    (!record.is_empty()).then_some(Codec::H264 {
        avcc: record,
        avc3: inline_sets,
        width,
        height,
    })
}

/// An audio sample entry's channels and rate, and its `esds`'s `AudioSpecificConfig`.
fn audio_entry(body: &[u8]) -> Option<Codec> {
    // 6 reserved, the data reference index, 8 reserved, channels, sample size, 4 predefined and reserved, the rate in
    // 16.16; 28 bytes before its boxes
    let channels = u32::from(u16_at(body, 16)?);
    let rate = u32_at(body, 24)? >> 16;
    let esds = full(find(body.get(28..)?, *b"esds")?)?.2;
    let config = audio_config(esds)?;
    Some(Codec::Aac { config, rate, channels })
}

/// A descriptor's tag and body, and what follows it (its length in up to four 7-bit bytes).
fn descriptor(bytes: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *bytes.first()?;
    let mut len = 0_usize;
    let mut at = 1;
    loop {
        let byte = *bytes.get(at)?;
        len = (len << 7) | usize::from(byte & 0x7f);
        at += 1;
        if byte & 0x80 == 0 {
            break;
        }
        if at > 4 {
            return None;
        }
    }
    let body = bytes.get(at..at.checked_add(len)?)?;
    Some((tag, body, &bytes[at + len..]))
}

/// The `AudioSpecificConfig` of an `esds`: its `ES_Descriptor`'s `DecoderConfigDescriptor`'s `DecoderSpecificInfo`.
fn audio_config(esds: &[u8]) -> Option<Vec<u8>> {
    let (tag, es, _) = descriptor(esds)?;
    if tag != 3 {
        return None;
    }
    // the stream's id, then its flags, which say what follows them
    let flags = *es.get(2)?;
    let mut at = 3;
    if flags & 0x80 != 0 {
        at += 2;
    }
    if flags & 0x40 != 0 {
        at += 1 + usize::from(*es.get(at)?);
    }
    if flags & 0x20 != 0 {
        at += 2;
    }
    let (tag, config, _) = descriptor(es.get(at..)?)?;
    if tag != 4 {
        return None;
    }
    // the object type, the stream type, the buffer size and the bit rates: 13 bytes
    let (tag, specific, _) = descriptor(config.get(13..)?)?;
    (tag == 5 && !specific.is_empty()).then(|| specific.to_vec())
}

/// Reads a media segment's samples (its `moof`s' first track fragment each, their bytes in the segment); `None` for
/// one that doesn't add up (a sample's bytes past the segment's end).
#[must_use]
pub fn read_samples(bytes: &[u8], init: &TrackInit) -> Option<Vec<Sample>> {
    let mut samples = Vec::new();
    let mut next_decode: Option<u64> = None;
    for (kind, body) in boxes(bytes) {
        if &kind != b"moof" {
            continue;
        }
        // the box's own start: its header is 8 bytes (a moof is never large)
        let moof_start = body.start.checked_sub(8)?;
        let moof = bytes.get(body)?;
        let traf = find(moof, *b"traf")?;
        fragment(bytes, moof_start, traf, init, &mut next_decode, &mut samples)?;
    }
    Some(samples)
}

/// One track fragment's samples.
fn fragment(
    bytes: &[u8],
    moof_start: usize,
    traf: &[u8],
    init: &TrackInit,
    next_decode: &mut Option<u64>,
    samples: &mut Vec<Sample>,
) -> Option<()> {
    let (base, defaults) = fragment_header(find(traf, *b"tfhd")?, moof_start, init.defaults)?;
    let mut run = Run {
        decode: match find(traf, *b"tfdt").and_then(full) {
            Some((1, _, rest)) => u64_at(rest, 0)?,
            Some((_, _, rest)) => u64::from(u32_at(rest, 0)?),
            None => next_decode.unwrap_or(0),
        },
        data: base,
        base,
        defaults,
    };
    for (kind, body) in boxes(traf) {
        if kind == *b"trun" {
            run.read(traf.get(body)?, bytes.len(), samples)?;
        }
    }
    *next_decode = Some(run.decode);
    Some(())
}

/// A track fragment's header (`tfhd`): where its data's offsets count from, and its samples' defaults.
fn fragment_header(tfhd: &[u8], moof_start: usize, defaults: Defaults) -> Option<(usize, Defaults)> {
    let (_, flags, tfhd) = full(tfhd)?;
    let mut at = 4;
    let mut base = moof_start;
    if flags & 0x1 != 0 {
        base = usize::try_from(u64_at(tfhd, at)?).ok()?;
        at += 8;
    }
    if flags & 0x2 != 0 {
        at += 4;
    }
    let mut defaults = defaults;
    for (flag, field) in [
        (0x8, &mut defaults.duration),
        (0x10, &mut defaults.size),
        (0x20, &mut defaults.flags),
    ] {
        if flags & flag != 0 {
            *field = u32_at(tfhd, at)?;
            at += 4;
        }
    }
    Some((base, defaults))
}

/// Where a track fragment's runs are: the next sample's decode time and bytes.
struct Run {
    decode: u64,
    data: usize,
    base: usize,
    defaults: Defaults,
}

impl Run {
    /// A track run's (`trun`) samples, each one's fields as its flags say, else the defaults; `None` for one whose
    /// bytes run past the segment's `len`.
    fn read(&mut self, trun: &[u8], len: usize, samples: &mut Vec<Sample>) -> Option<()> {
        let (version, flags, trun) = full(trun)?;
        let count = usize::try_from(u32_at(trun, 0)?).ok()?;
        let mut at = 4;
        if flags & 0x1 != 0 {
            let offset = i32::from_be_bytes(trun.get(at..at + 4)?.try_into().ok()?);
            self.data = self.base.checked_add_signed(isize::try_from(offset).ok()?)?;
            at += 4;
        }
        let mut first_flags = None;
        if flags & 0x4 != 0 {
            first_flags = Some(u32_at(trun, at)?);
            at += 4;
        }
        let mut field = |flag: u32, default: u32| {
            if flags & flag == 0 {
                return Some(default);
            }
            let value = u32_at(trun, at);
            at += 4;
            value
        };
        for index in 0..count {
            let duration = field(0x100, self.defaults.duration)?;
            let size = field(0x200, self.defaults.size)?;
            let default_flags = first_flags.filter(|_| index == 0).unwrap_or(self.defaults.flags);
            let sample_flags = field(0x400, default_flags)?;
            let raw = field(0x800, 0)?;
            let offset = if version == 0 {
                i64::from(raw)
            } else {
                i64::from(i32::from_be_bytes(raw.to_be_bytes()))
            };
            let end = self.data.checked_add(usize::try_from(size).ok()?)?;
            if end > len {
                return None;
            }
            samples.push(Sample {
                decode: self.decode,
                offset,
                duration,
                key: sample_flags & NON_SYNC == 0,
                bytes: self.data..end,
            });
            self.data = end;
            self.decode = self.decode.checked_add(u64::from(duration))?;
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIDEO_INIT: &[u8] = include_bytes!("../../tests/fixtures/media/dash/init-0.m4s");
    const VIDEO: &[u8] = include_bytes!("../../tests/fixtures/media/dash/chunk-0-00002.m4s");
    const AUDIO_INIT: &[u8] = include_bytes!("../../tests/fixtures/media/dash/init-1.m4s");
    const AUDIO: &[u8] = include_bytes!("../../tests/fixtures/media/dash/chunk-1-00001.m4s");

    /// A box of `kind` around `body`.
    fn bx(kind: [u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(body.len() + 8).unwrap().to_be_bytes().to_vec();
        out.extend_from_slice(&kind);
        out.extend_from_slice(body);
        out
    }

    /// A full box's body: its version and flags, then `rest`.
    fn fb(version: u8, flags: u32, rest: &[u8]) -> Vec<u8> {
        let mut out = ((u32::from(version) << 24) | flags).to_be_bytes().to_vec();
        out.extend_from_slice(rest);
        out
    }

    fn words(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|value| value.to_be_bytes()).collect()
    }

    #[test]
    fn a_recording_s_initialization_segments_read() {
        let video = read_init(VIDEO_INIT).expect("the video's");
        assert_eq!(video.timescale, 16_384);
        assert!(matches!(
            &video.codec,
            Codec::H264 { avcc, avc3: false, width: 320, height: 180 } if !avcc.is_empty()
        ));
        // B-frames: the encoder's edit list starts the picture after their delay
        assert!(video.shift > 0);
        let audio = read_init(AUDIO_INIT).expect("the audio's");
        assert_eq!(audio.timescale, 16_000);
        assert!(matches!(&audio.codec, Codec::Aac { config, rate: 16_000, channels: 1 } if config.len() >= 2));
    }

    #[test]
    fn a_media_segment_reads_into_its_samples() {
        let video = read_init(VIDEO_INIT).unwrap();
        let samples = read_samples(VIDEO, &video).expect("its samples");
        assert_eq!(samples.len(), 16);
        assert!(samples[0].key);
        assert!(samples.iter().skip(1).any(|sample| !sample.key));
        assert!(samples.iter().any(|sample| sample.offset > 0));
        assert!(samples.iter().all(|sample| sample.bytes.end <= VIDEO.len()));
        let audio = read_init(AUDIO_INIT).unwrap();
        let samples = read_samples(AUDIO, &audio).expect("its samples");
        assert!(samples.iter().all(|sample| sample.key));
        assert_eq!(samples[0].decode, 0);
        assert!(samples.windows(2).all(|pair| pair[1].decode > pair[0].decode));
    }

    /// A track's initialization segment made up around a sample entry.
    fn init_with(entry: &[u8], edit_list: Option<&[u8]>, trex: &[u8]) -> Vec<u8> {
        let tkhd = bx(*b"tkhd", &fb(1, 3, &[&[0; 16][..], &words(&[7])].concat()));
        let mdhd = bx(*b"mdhd", &fb(1, 0, &[&[0; 16][..], &words(&[90_000])].concat()));
        let stsd = bx(*b"stsd", &fb(0, 0, &[&words(&[1])[..], entry].concat()));
        let stbl = bx(*b"stbl", &stsd);
        let minf = bx(*b"minf", &stbl);
        let mdia = bx(*b"mdia", &[mdhd, minf].concat());
        let edts = edit_list
            .map(|elst| bx(*b"edts", &bx(*b"elst", elst)))
            .unwrap_or_default();
        let trak = bx(*b"trak", &[tkhd, edts, mdia].concat());
        let mvex = bx(*b"mvex", &bx(*b"trex", trex));
        [bx(*b"ftyp", b"iso6"), bx(*b"moov", &[trak, mvex].concat())].concat()
    }

    fn avc3() -> Vec<u8> {
        let mut entry = vec![0; 78];
        entry[24..26].copy_from_slice(&640_u16.to_be_bytes());
        entry[26..28].copy_from_slice(&360_u16.to_be_bytes());
        entry.extend(bx(*b"avcC", &[1, 0x42, 0, 0x1e]));
        bx(*b"avc3", &entry)
    }

    #[test]
    fn edit_lists_defaults_and_versions_read_as_written() {
        // version 1 boxes, an empty edit before the one that counts, and the track's defaults
        let edits: Vec<u8> = [
            &words(&[2])[..],
            &[0; 8],
            &[0xff; 8],
            &[0, 1, 0, 0],
            &[0; 8],
            &[0, 0, 0, 0, 0, 0, 0x0b, 0xb8],
            &[0, 1, 0, 0],
        ]
        .concat();
        let elst = fb(1, 0, &edits);
        let trex = fb(0, 0, &words(&[7, 1, 3000, 4, NON_SYNC]));
        let init = read_init(&init_with(&avc3(), Some(&elst), &trex)).expect("an init");
        assert_eq!(init.timescale, 90_000);
        assert_eq!(init.shift, 3000);
        assert!(matches!(
            init.codec,
            Codec::H264 {
                avc3: true,
                width: 640,
                height: 360,
                ..
            }
        ));
        assert_eq!(
            init.defaults,
            Defaults {
                duration: 3000,
                size: 4,
                flags: NON_SYNC
            }
        );
        // a version 0 edit list
        let elst = fb(0, 0, &[&words(&[1, 9000, 1500])[..], &[0, 1, 0, 0]].concat());
        assert_eq!(read_init(&init_with(&avc3(), Some(&elst), &trex)).unwrap().shift, 1500);
        // defaults of another track are none of this one's
        let other = fb(0, 0, &words(&[8, 1, 3000, 4, NON_SYNC]));
        assert_eq!(
            read_init(&init_with(&avc3(), None, &other)).unwrap().defaults,
            Defaults::default()
        );
    }

    #[test]
    fn what_this_doesn_t_read_is_none() {
        let trex = fb(0, 0, &words(&[7, 1, 0, 0, 0]));
        // an encrypted entry, an `avc1` without its `avcC`, nothing at all
        let mut encv = avc3();
        encv[4..8].copy_from_slice(b"encv");
        assert!(read_init(&init_with(&encv, None, &trex)).is_none());
        let bare = bx(*b"avc1", &[0; 78]);
        assert!(read_init(&init_with(&bare, None, &trex)).is_none());
        assert!(read_init(b"").is_none());
        assert!(read_init(&bx(*b"moov", &bx(*b"trak", b""))).is_none());
        // an `esds` that isn't one
        let no_specific: Vec<u8> = [&[3, 20, 0, 1, 0][..], &[4, 15], &[0x40, 0x15], &[0; 11], &[6, 0]].concat();
        for esds in [
            vec![4, 0],
            vec![3, 3, 0, 0, 0],
            vec![3, 0x80, 0x80, 0x80, 0x80, 1],
            no_specific,
        ] {
            let mut entry = vec![0; 28];
            entry.extend(bx(*b"esds", &fb(0, 0, &esds)));
            assert!(
                read_init(&init_with(&bx(*b"mp4a", &entry), None, &trex)).is_none(),
                "{esds:?}"
            );
        }
    }

    #[test]
    fn an_esds_s_optional_fields_are_stepped_over() {
        // a dependency, a URL and an OCR stream before the decoder's config
        let config: Vec<u8> = [&[4, 17, 0x40, 0x15][..], &[0; 11], &[5, 2, 0x14, 0x08]].concat();
        let es: Vec<u8> = [&[0, 1, 0xe0][..], &[0, 2], &[3], b"abc", &[0, 3], &config].concat();
        let esds: Vec<u8> = [&[3, u8::try_from(es.len()).unwrap()][..], &es].concat();
        let mut entry = vec![0; 28];
        entry[16..18].copy_from_slice(&2_u16.to_be_bytes());
        entry[24..28].copy_from_slice(&(44_100_u32 << 16).to_be_bytes());
        entry.extend(bx(*b"esds", &fb(0, 0, &esds)));
        let trex = fb(0, 0, &words(&[7, 1, 0, 0, 0]));
        let init = read_init(&init_with(&bx(*b"mp4a", &entry), None, &trex)).expect("an init");
        assert_eq!(
            init.codec,
            Codec::Aac {
                config: vec![0x14, 0x08],
                rate: 44_100,
                channels: 2
            }
        );
    }

    #[test]
    fn a_fragment_s_header_fields_and_run_fields_read_as_written() {
        let init = TrackInit {
            timescale: 1000,
            codec: Codec::Aac {
                config: vec![0x14, 0x08],
                rate: 0,
                channels: 0,
            },
            shift: 0,
            defaults: Defaults::default(),
        };
        // the mdat's bytes first, at the segment's start: a base data offset of 0 points at them
        let mdat = bx(*b"mdat", b"aaaabbbbcc");
        let header: Vec<u8> = [&words(&[1])[..], &[0; 8], &words(&[9, 20, 4, NON_SYNC])].concat();
        let tfhd = bx(*b"tfhd", &fb(0, 0x3b, &header));
        // no tfdt: the first fragment starts at 0, the next where it ended; version 0 offsets, a first sample's flags
        let first = bx(*b"trun", &fb(0, 0x805, &words(&[2, 8, 0, 5, 7])));
        let moof = bx(*b"moof", &bx(*b"traf", &[tfhd.clone(), first].concat()));
        let second = bx(*b"trun", &fb(1, 0x301, &words(&[1, 16, 30, 2])));
        let moof2 = bx(*b"moof", &bx(*b"traf", &[tfhd, second].concat()));
        let segment = [mdat, moof, moof2].concat();
        let samples = read_samples(&segment, &init).expect("its samples");
        assert_eq!(samples.len(), 3);
        assert_eq!((samples[0].decode, samples[0].offset, samples[0].key), (0, 5, true));
        assert_eq!((samples[1].decode, samples[1].offset, samples[1].key), (20, 7, false));
        assert_eq!(samples[0].bytes, 8..12);
        assert_eq!(samples[1].bytes, 12..16);
        assert_eq!((samples[2].decode, samples[2].duration), (40, 30));
        assert_eq!(samples[2].bytes, 16..18);
        // a version 0 decode time and per-sample flags
        let tfdt = bx(*b"tfdt", &fb(0, 0, &words(&[500])));
        let tfhd = bx(*b"tfhd", &fb(0, 0x1, &[&words(&[1])[..], &[0; 8]].concat()));
        let run = bx(*b"trun", &fb(0, 0x601, &words(&[1, 8, 2, NON_SYNC])));
        let moof = bx(*b"moof", &bx(*b"traf", &[tfhd.clone(), tfdt, run].concat()));
        let samples = read_samples(&[bx(*b"mdat", b"xy"), moof].concat(), &init).unwrap();
        assert_eq!((samples[0].decode, samples[0].key), (500, false));
        // a sample past the segment's end
        let long = bx(*b"trun", &fb(0, 0x201, &words(&[1, 8, 400])));
        let moof = bx(*b"moof", &bx(*b"traf", &[tfhd, long].concat()));
        assert!(read_samples(&[bx(*b"mdat", b"x"), moof].concat(), &init).is_none());
        // a moof without a track fragment
        assert!(read_samples(&bx(*b"moof", &bx(*b"mfhd", &[0; 8])), &init).is_none());
    }

    #[test]
    fn large_and_open_ended_boxes_are_walked() {
        let mut large = 1_u32.to_be_bytes().to_vec();
        large.extend_from_slice(b"free");
        large.extend_from_slice(&20_u64.to_be_bytes());
        large.extend_from_slice(b"abcd");
        let mut open = 0_u32.to_be_bytes().to_vec();
        open.extend_from_slice(b"mdat");
        open.extend_from_slice(b"rest of it");
        let bytes = [large, open].concat();
        let kinds: Vec<[u8; 4]> = boxes(&bytes).map(|(kind, _)| kind).collect();
        assert_eq!(kinds, [*b"free", *b"mdat"]);
        assert_eq!(find(&bytes, *b"mdat"), Some(&b"rest of it"[..]));
        // one that claims more than there is ends the walk
        let mut short = 100_u32.to_be_bytes().to_vec();
        short.extend_from_slice(b"free");
        assert_eq!(boxes(&short).count(), 0);
    }
}
