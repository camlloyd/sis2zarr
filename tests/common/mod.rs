#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::Value;
use tempfile::TempDir;
use zarrs::array::{Array, ElementOwned};
use zarrs::array_subset::ArraySubset;
use zarrs::filesystem::FilesystemStore;

/// Samples per channel-plane: a 5x3 image on a 2x2 grid of 4x2 tiles (8x4 padded),
/// zoomLevel 1 (two levels). Each scope has two channels and two z-planes, and each
/// channel-plane is two chunks.
const SAMPLES: usize = 8 * 4;

/// Constant value of channel-plane (z, c) in the first scope.
pub fn value(z: usize, c: usize) -> u8 {
    scope_value(0, z, c)
}

/// Constant value of channel-plane (z, c) in `scope`. Plane z=0 c=1 is stored as empty
/// chunks.
pub fn scope_value(scope: usize, z: usize, c: usize) -> u8 {
    if (z, c) == (0, 1) {
        0
    } else {
        (100 * z + 10 * c + 1 + 2 * scope) as u8
    }
}

/// Tile index layouts the reader recognises.
#[derive(Clone, Copy, Debug)]
pub enum Layout {
    /// u32 section length, entries of u64 offset, u64 length, u8 flag.
    V34,
    /// u64 section length and an entry count, entries of u64 offset, u32 length, u32
    /// with the compression codec in its first byte, u8 0, u8 flag; the section ends with
    /// a further 16-byte table header.
    V37,
    /// As 3.7, with the section ending at the last entry.
    V38,
}

pub const LAYOUTS: [Layout; 3] = [Layout::V34, Layout::V37, Layout::V38];

pub fn u16_sample(v: u8) -> Vec<u8> {
    (v as u16).to_le_bytes().into()
}

/// A SIS 3.4 file of 16-bit samples holding the image above.
pub fn sis_file() -> Vec<u8> {
    build_sis_file(Layout::V34, "unsigned short", u16_sample, |xml| xml)
}

/// Like [`sis_file`], with `scopes` scopes; the first is named "Stack", the others
/// "Stack 2", "Stack 3", ….
pub fn multi_scope_sis_file(scopes: usize) -> Vec<u8> {
    build_scopes(
        scopes,
        1,
        Layout::V34,
        "unsigned short",
        u16_sample,
        |xml| xml,
    )
}

/// Like [`sis_file`], with `ticks` time points; time point t adds `20 * t` to each value.
pub fn multi_time_sis_file(ticks: usize) -> Vec<u8> {
    build_scopes(1, ticks, Layout::V34, "unsigned short", u16_sample, |xml| {
        xml
    })
}

/// Constant value of channel-plane (z, c) at time point `t` in the first scope.
pub fn time_value(t: usize, z: usize, c: usize) -> u8 {
    match value(z, c) {
        0 => 0,
        v => v + 20 * t as u8,
    }
}

/// Like [`sis_file`], with `from` replaced by `to` in the header XML.
pub fn sis_file_with(from: &str, to: &str) -> Vec<u8> {
    build_sis_file(Layout::V34, "unsigned short", u16_sample, |xml| {
        xml.replace(from, to)
    })
}

/// The image above with the given tile index layout and pixel type, each sample encoded
/// by `sample`, and `edit` applied to the header XML.
pub fn build_sis_file(
    layout: Layout,
    pixel_type: &str,
    sample: impl Fn(u8) -> Vec<u8>,
    edit: impl Fn(String) -> String,
) -> Vec<u8> {
    build_scopes(1, 1, layout, pixel_type, sample, edit)
}

fn build_scopes(
    scopes: usize,
    ticks: usize,
    layout: Layout,
    pixel_type: &str,
    sample: impl Fn(u8) -> Vec<u8>,
    edit: impl Fn(String) -> String,
) -> Vec<u8> {
    let plane = |v: u8| -> Vec<u8> { (0..SAMPLES).flat_map(|_| sample(v)).collect() };
    let gzip = |bytes: &[u8]| {
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(bytes).unwrap();
        e.finish().unwrap()
    };
    // (bytes, flag) per chunk, scope by scope, z by z, channel by channel.
    let mut chunks: Vec<(Vec<u8>, u8)> = Vec::new();
    for (scope, t) in (0..scopes).flat_map(|s| (0..ticks).map(move |t| (s, t))) {
        for z in 0..2 {
            for c in 0..2 {
                let v = scope_value(scope, z, c);
                let p = plane(if v == 0 { 0 } else { v + 20 * t as u8 });
                let (first, second) = p.split_at(p.len() / 2);
                match (z, c) {
                    (0, 0) => chunks.extend([(gzip(first), 1), (second.to_vec(), 0)]),
                    (0, 1) => chunks.extend([(vec![], 0), (vec![], 0)]),
                    _ => chunks.extend([(first.to_vec(), 0), (second.to_vec(), 0)]),
                }
            }
        }
    }

    let planes = r#"<Plane zoomLevel="1"><Rect width="5" height="3"/><Tiles width="4" height="2" tileNumX="2" tileNumY="2"/></Plane>"#.repeat(2);
    let ticks_xml = format!("<Tick>{planes}</Tick>").repeat(ticks);
    let scope_xml = (0..scopes)
        .map(|s| {
            let visible = if s == 0 { "Stack".to_string() } else { format!("Stack {}", s + 1) };
            format!(
                r#"<Scope id="{id}" name="s{s}" visibleName="{visible}" scopeType="0"><PixelType name="{pixel_type}"/><PixelSize width="0.5" height="0.25" depth="2"/><Channels number="2"><Channel index="0" name="Alpha" id="7"/><Channel index="1" name="" id="8"/></Channels><Ticks>{ticks_xml}</Ticks></Scope>"#,
                id = s + 1
            )
        })
        .collect::<String>();
    let xml = format!(
        r#"<Header><ColorSettings value='{{"Scopes":[{{"Id":"s0","ChannelSettings":[{{"Id":7,"CtfR":[0,10],"CtfG":[0,20],"CtfB":[0,30],"RangeMin":5,"RangeMax":90}}]}}]}}'/>{scope_xml}</Header>"#
    );
    let xml = edit(xml);
    let version = match layout {
        Layout::V34 => "3.4",
        Layout::V37 => "3.7",
        Layout::V38 => "3.8",
    };
    let block = format!("{{SISVersion = {version}\n{xml}\n}}");
    let mut file =
        format!("#Single Image Stack\nHeader = {:08}\n{block}", block.len()).into_bytes();

    let (prefix, entry, trailer) = match layout {
        Layout::V34 => (20, 17, 0),
        Layout::V37 => (40, 18, 16),
        Layout::V38 => (40, 18, 0),
    };
    let index_len = prefix + chunks.len() * entry + trailer;
    let mut offset = (file.len() + index_len) as u64;
    file.extend([0xff; 16]);
    match layout {
        Layout::V34 => file.extend((index_len as u32).to_le_bytes()),
        Layout::V37 | Layout::V38 => {
            file.extend((index_len as u64).to_le_bytes());
            for v in [16, chunks.len() as u32, 0, 18] {
                file.extend(v.to_le_bytes());
            }
        }
    }
    for (bytes, flag) in &chunks {
        let len = bytes.len() as u64;
        file.extend(if len == 0 { 0 } else { offset }.to_le_bytes());
        match layout {
            Layout::V34 => {
                file.extend(len.to_le_bytes());
                file.push(*flag);
            }
            // The codec byte carries compression; the flag is 1 even for raw chunks.
            Layout::V37 | Layout::V38 => {
                file.extend((len as u32).to_le_bytes());
                file.extend([*flag, 0, 0, 0, 0, 1]);
            }
        }
        offset += len;
    }
    file.extend(vec![0; trailer]);
    for (bytes, _) in &chunks {
        file.extend(bytes);
    }
    file
}

/// Write `file` to a temp dir and convert it; the dir lives as long as the returned
/// [`TempDir`].
pub fn try_convert(file: &[u8]) -> (TempDir, Result<PathBuf, sis2zarr::SisError>) {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("in.sis");
    std::fs::write(&input, file).unwrap();
    let output = dir.path().join("out.ome.zarr");
    let result = sis2zarr::convert_to_zarr(&input, &output).map(|()| output);
    (dir, result)
}

pub fn convert(file: &[u8]) -> (TempDir, PathBuf) {
    let (dir, result) = try_convert(file);
    (dir, result.unwrap())
}

/// Shape and every element of resolution `level` of image group `image`.
pub fn read_level<T: ElementOwned>(
    output: &Path,
    image: usize,
    level: usize,
) -> (Vec<u64>, Vec<T>) {
    let store = Arc::new(FilesystemStore::new(output).unwrap());
    let array = Array::open(store, &format!("/{image}/{level}")).unwrap();
    let shape = array.shape().to_vec();
    let all = ArraySubset::new_with_shape(shape.clone());
    let elements = array.retrieve_array_subset_elements::<T>(&all).unwrap();
    (shape, elements)
}

pub fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}
