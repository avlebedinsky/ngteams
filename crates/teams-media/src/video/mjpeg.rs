//! Motion JPEG as a UVC camera sends it: one JPEG picture a frame, often without its Huffman tables (no `DHT`
//! segment), which then means the standard ones (ITU-T T.81, Annex K.3). A still-picture decoder doesn't assume them
//! (`zune-jpeg` does only for a frame marked `AVI1`, which many cameras leave out too), so [`decode`] puts them in
//! first, as the `GStreamer` and `FFmpeg` Motion JPEG decoders do.

use std::borrow::Cow;

use crate::devices::{PixelFormat, VideoFrame};

/// A JPEG marker's second byte: start of scan, after which the tables must already have come.
const SOS: u8 = 0xda;
/// Define Huffman tables.
const DHT: u8 = 0xc4;
/// End of image: the last marker of a whole picture.
const EOI: [u8; 2] = [0xff, 0xd9];

/// One standard Huffman table: its class and id byte (`0x00` DC luma, `0x01` DC chroma, `0x10` AC luma, `0x11` AC
/// chroma), how many codes each length from 1 to 16 bits has, and the values in code order.
struct Table {
    class_and_id: u8,
    counts: [u8; 16],
    values: &'static [u8],
}

const DC_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

/// Annex K.3's tables, in the order a `DHT` segment lists them.
const STANDARD_TABLES: [Table; 4] = [
    Table {
        class_and_id: 0x00,
        counts: [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
        values: &DC_VALUES,
    },
    Table {
        class_and_id: 0x01,
        counts: [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
        values: &DC_VALUES,
    },
    Table {
        class_and_id: 0x10,
        counts: [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d],
        values: &[
            0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71,
            0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72,
            0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37,
            0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59,
            0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
            0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3,
            0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
            0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
            0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
        ],
    },
    Table {
        class_and_id: 0x11,
        counts: [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77],
        values: &[
            0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, 0x13, 0x22,
            0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1,
            0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36,
            0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58,
            0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
            0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a,
            0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba,
            0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
            0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
        ],
    },
];

/// A Motion JPEG `frame` as RGB; `None` for a picture that doesn't decode, or that was cut short (a camera's first
/// one can be, or one whose transfer broke off): the decoder would fill what is missing with grey rather than fail, so
/// a picture without its end marker is dropped before it is decoded. Nothing else in a picture's data can read as
/// that marker (a 0xFF in the coded data is followed by 0x00), so a camera's padding after it is fine. JPEG's colours
/// are full range: the decoder's RGB is converted to limited range as any RGB camera's is.
pub(super) fn decode(frame: &VideoFrame) -> Option<VideoFrame> {
    if !frame.data.windows(2).rev().any(|pair| pair == EOI) {
        return None;
    }
    let picture = image::load_from_memory_with_format(&with_tables(&frame.data), image::ImageFormat::Jpeg)
        .ok()?
        .into_rgb8();
    let (width, height) = picture.dimensions();
    Some(VideoFrame {
        width,
        height,
        format: PixelFormat::Rgb,
        stride: width.checked_mul(3)?,
        data: picture.into_raw(),
    })
}

/// `jpeg` with the standard Huffman tables put in after its start marker when no `DHT` segment comes before its
/// scan; as it is otherwise, or when its segments can't be read (the decoder then says why).
fn with_tables(jpeg: &[u8]) -> Cow<'_, [u8]> {
    if has_tables(jpeg).unwrap_or(true) {
        return Cow::Borrowed(jpeg);
    }
    let tables = standard_tables_segment();
    let mut out = Vec::with_capacity(jpeg.len() + tables.len());
    out.extend_from_slice(&jpeg[..2]);
    out.extend_from_slice(&tables);
    out.extend_from_slice(&jpeg[2..]);
    Cow::Owned(out)
}

/// Whether a `DHT` segment comes before `jpeg`'s scan; `None` for one whose segments can't be walked.
fn has_tables(jpeg: &[u8]) -> Option<bool> {
    if jpeg.get(..2)? != [0xff, 0xd8] {
        return None;
    }
    let mut at = 2;
    loop {
        let [0xff, marker, high, low] = *jpeg.get(at..at + 4)? else {
            return None;
        };
        match marker {
            DHT => return Some(true),
            SOS => return Some(false),
            _ => at += 2 + usize::from(u16::from_be_bytes([high, low])),
        }
    }
}

/// One `DHT` segment holding [`STANDARD_TABLES`].
fn standard_tables_segment() -> Vec<u8> {
    let body: Vec<u8> = STANDARD_TABLES
        .iter()
        .flat_map(|table| {
            std::iter::once(table.class_and_id)
                .chain(table.counts)
                .chain(table.values.iter().copied())
        })
        .collect();
    let length = u16::try_from(body.len() + 2).unwrap_or(u16::MAX);
    let mut segment = vec![0xff, DHT];
    segment.extend_from_slice(&length.to_be_bytes());
    segment.extend(body);
    segment
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_standard_table_holds_as_many_values_as_its_counts_say() {
        for table in &STANDARD_TABLES {
            let codes: usize = table.counts.iter().map(|count| usize::from(*count)).sum();
            assert_eq!(codes, table.values.len(), "table {:#04x}", table.class_and_id);
        }
        assert_eq!(standard_tables_segment().len(), 2 + 2 + 4 * 17 + 12 + 12 + 162 + 162);
    }

    #[test]
    fn a_picture_whose_segments_can_t_be_walked_is_left_as_it_is() {
        for broken in [&b"not a picture"[..], &[0xff, 0xd8], &[0xff, 0xd8, 0x00, 0x00, 0x00, 0x00]] {
            assert!(matches!(with_tables(broken), Cow::Borrowed(_)));
        }
    }
}
