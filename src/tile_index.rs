//! The tile index follows the text header: 16 bytes of 0xff, then a version-specific
//! prefix and fixed-size entries. Each entry starts with a u64 offset and says whether
//! its chunk is raw or gzip-compressed.

use crate::error::{SisError, format_err};

/// A stored chunk: offset, length, and whether it is gzip-compressed.
pub(crate) type Chunk = (u64, u64, bool);

enum Layout {
    /// SIS 3.4: u32 section length (counting the 0xff bytes), then entries of u64 offset,
    /// u64 length, u8 flag (0 raw, 1 gzip).
    V34,
    /// SIS 3.7/3.8: u64 section length, u32 16, u32 entry count, u32 0, u32 entry length,
    /// then entries of u64 offset, u32 length, u32 (compression codec in its first byte,
    /// channel in its second), u8 0, u8 flag. The codec is the header's
    /// `CompressionCodes` id (0 none, 1 gzip); the flag does not follow it, as files
    /// saved without compression set it to 1. The section can continue past the entries
    /// (a 3.7 file has a further 16-byte table header there), so the count, not the
    /// length, ends the entries.
    V38,
}

impl Layout {
    fn prefix_len(&self) -> usize {
        match self {
            Layout::V34 => 20,
            Layout::V38 => 40,
        }
    }

    fn entry_len(&self) -> usize {
        match self {
            Layout::V34 => 17,
            Layout::V38 => 18,
        }
    }
}

/// Read the tile index that starts at `start`, in stored order. Every chunk is checked
/// to lie within `data`.
pub(crate) fn read(data: &[u8], start: usize) -> Result<Vec<Chunk>, SisError> {
    let prefix = data
        .get(start..start + Layout::V38.prefix_len())
        .ok_or_else(|| format_err("truncated tile index"))?;
    let u32_at = |i: usize| u32::from_le_bytes(prefix[i..i + 4].try_into().unwrap()) as usize;
    let v38_len = u64::from_le_bytes(prefix[16..24].try_into().unwrap());
    let v38_entries_end = Layout::V38.prefix_len() + u32_at(28) * Layout::V38.entry_len();
    let (layout, entries_end) = if u32_at(24) == 16
        && u32_at(36) == Layout::V38.entry_len()
        && v38_len >= v38_entries_end as u64
    {
        (Layout::V38, v38_entries_end)
    } else if u32_at(16) >= Layout::V34.prefix_len()
        && (u32_at(16) - Layout::V34.prefix_len()).is_multiple_of(Layout::V34.entry_len())
    {
        (Layout::V34, u32_at(16))
    } else {
        return Err(SisError::Unsupported(
            "unrecognised tile index layout".into(),
        ));
    };

    let entries = data
        .get(start + layout.prefix_len()..start + entries_end)
        .ok_or_else(|| format_err("tile index exceeds file size"))?;
    entries
        .chunks_exact(layout.entry_len())
        .map(|e| {
            let offset = u64::from_le_bytes(e[0..8].try_into().unwrap());
            let len = match layout {
                Layout::V34 => u64::from_le_bytes(e[8..16].try_into().unwrap()),
                Layout::V38 => u32::from_le_bytes(e[8..12].try_into().unwrap()) as u64,
            };
            let gzip = match layout {
                Layout::V34 => match e[16] {
                    0 => false,
                    1 => true,
                    flag => return Err(SisError::Unsupported(format!("chunk flag {flag}"))),
                },
                Layout::V38 => match e[12] {
                    0 => false,
                    1 => true,
                    codec => {
                        return Err(SisError::Unsupported(format!("compression codec {codec}")));
                    }
                },
            };
            match offset.checked_add(len) {
                Some(end) if end <= data.len() as u64 => Ok((offset, len, gzip)),
                _ => Err(format_err(format!(
                    "chunk at {offset} (+{len}) exceeds file size {}",
                    data.len()
                ))),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3.4 index at offset 0 for `chunks`, followed by 64 bytes of chunk data.
    fn v34(chunks: &[(u64, u64, u8)]) -> Vec<u8> {
        let mut d = vec![0xff; 16];
        d.extend((20 + chunks.len() as u32 * 17).to_le_bytes());
        for &(offset, len, flag) in chunks {
            d.extend(offset.to_le_bytes());
            d.extend(len.to_le_bytes());
            d.push(flag);
        }
        d.resize(d.len() + 64, 0);
        d
    }

    /// A 3.8 index at offset 0 for `chunks` of (offset, length, codec), each with flag
    /// byte 1, followed by 64 bytes of chunk data.
    fn v38(chunks: &[(u64, u32, u8)]) -> Vec<u8> {
        v38_with_flag(chunks, 1)
    }

    fn v38_with_flag(chunks: &[(u64, u32, u8)], flag: u8) -> Vec<u8> {
        let mut d = vec![0xff; 16];
        d.extend((40 + chunks.len() as u64 * 18).to_le_bytes());
        for v in [16, chunks.len() as u32, 0, 18] {
            d.extend(v.to_le_bytes());
        }
        for &(offset, len, codec) in chunks {
            d.extend(offset.to_le_bytes());
            d.extend(len.to_le_bytes());
            d.extend([codec, 1, 0, 0]);
            d.extend([0, flag]);
        }
        d.resize(d.len() + 64, 0);
        d
    }

    #[test]
    fn v38_entries_end_at_the_count_when_the_section_runs_on() {
        // A 3.7 file's section length also covers a 16-byte table header after the entries.
        let mut d = v38(&[(100, 10, 1)]);
        let len = u64::from_le_bytes(d[16..24].try_into().unwrap()) + 16;
        d[16..24].copy_from_slice(&len.to_le_bytes());
        let end = 40 + 18;
        d.splice(
            end..end,
            [16u32, 0, 0, 29].iter().flat_map(|v| v.to_le_bytes()),
        );

        assert_eq!(read(&d, 0).unwrap(), vec![(100, 10, true)]);
    }

    #[test]
    fn reads_v34_entries_in_stored_order() {
        let d = v34(&[(80, 10, 1), (60, 20, 0)]);
        assert_eq!(read(&d, 0).unwrap(), vec![(80, 10, true), (60, 20, false)]);
    }

    #[test]
    fn reads_v38_entries_in_stored_order() {
        let d = v38(&[(100, 10, 0), (80, 20, 1)]);
        assert_eq!(read(&d, 0).unwrap(), vec![(100, 10, false), (80, 20, true)]);
    }

    #[test]
    fn reads_index_after_a_header() {
        let mut d = vec![b' '; 7];
        d.extend(v38(&[(7 + 58, 4, 1)]));
        assert_eq!(read(&d, 7).unwrap(), vec![(65, 4, true)]);
    }

    #[test]
    fn refuses_unknown_layout() {
        let mut d = v34(&[(60, 10, 0)]);
        d[16] = 21; // section length no longer fits whole 3.4 entries
        assert!(matches!(read(&d, 0), Err(SisError::Unsupported(_))));
    }

    #[test]
    fn v38_compression_follows_the_codec_not_the_flag() {
        let raw_flag_1 = v38_with_flag(&[(60, 10, 0)], 1);
        assert_eq!(read(&raw_flag_1, 0).unwrap(), vec![(60, 10, false)]);
        let gzip_flag_0 = v38_with_flag(&[(60, 10, 1)], 0);
        assert_eq!(read(&gzip_flag_0, 0).unwrap(), vec![(60, 10, true)]);
    }

    #[test]
    fn refuses_unknown_chunk_flag() {
        let d = v34(&[(60, 10, 2)]);
        assert!(matches!(read(&d, 0), Err(SisError::Unsupported(m)) if m == "chunk flag 2"));
    }

    #[test]
    fn refuses_unknown_compression_codec() {
        let d = v38(&[(60, 10, 2)]);
        assert!(matches!(read(&d, 0), Err(SisError::Unsupported(m)) if m == "compression codec 2"));
    }

    /// Chunk lists that fit in the 64 bytes of data after the index, including empty
    /// and zero-length chunks at either end of the data.
    const CHUNK_LISTS: &[&[(u64, u64, u8)]] = &[
        &[],
        &[(0, 64, 1)],
        &[(64, 0, 0), (0, 0, 1)],
        &[(30, 4, 0), (10, 20, 1), (0, 10, 0), (34, 30, 1)],
    ];

    fn narrow(chunks: &[(u64, u64, u8)]) -> Vec<(u64, u32, u8)> {
        chunks.iter().map(|&(o, l, f)| (o, l as u32, f)).collect()
    }

    #[test]
    fn both_layouts_round_trip() {
        for chunks in CHUNK_LISTS {
            let expected: Vec<Chunk> = chunks.iter().map(|&(o, l, f)| (o, l, f == 1)).collect();
            assert_eq!(read(&v34(chunks), 0).unwrap(), expected, "3.4 {chunks:?}");
            assert_eq!(
                read(&v38(&narrow(chunks)), 0).unwrap(),
                expected,
                "3.8 {chunks:?}"
            );
        }
    }

    /// Overwriting any one byte, or truncating anywhere, gives an error or chunks, never
    /// a panic.
    #[test]
    fn never_panics_on_corrupt_index() {
        let chunks = CHUNK_LISTS[3];
        for d in [v34(chunks), v38(&narrow(chunks))] {
            for i in 0..d.len() {
                for byte in [0, 1, 0x7f, 0xff] {
                    let mut d = d.clone();
                    d[i] = byte;
                    let _ = read(&d, 0);
                }
            }
            for len in 0..d.len() {
                let _ = read(&d[..len], 0);
            }
        }
    }

    #[test]
    fn refuses_chunk_past_end_of_file() {
        let d = v34(&[(u64::MAX, 1, 0)]);
        assert!(matches!(read(&d, 0), Err(SisError::Format(_))));
        let d = v34(&[(50, 1000, 0)]);
        assert!(matches!(read(&d, 0), Err(SisError::Format(_))));
    }
}
