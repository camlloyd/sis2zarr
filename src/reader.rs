use std::fs::File;
use std::io::Read;
use std::path::Path;

use flate2::bufread::GzDecoder;
use memmap2::Mmap;

use crate::channels::{ChannelInfo, read_channels};
use crate::error::{SisError, format_err};
use crate::grid::TileGrid;
use crate::tile_index::{self, Chunk};

// The files' first line uses SIS's older name, Single Image Stack.
const MAGIC: &[u8] = b"#Single Image Stack\n";

/// Sample type of the stored pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PixelType {
    U8,
    U16,
    F32,
}

impl PixelType {
    /// Bytes per stored sample.
    pub(crate) fn size(self) -> usize {
        match self {
            PixelType::U8 => 1,
            PixelType::U16 => 2,
            PixelType::F32 => 4,
        }
    }

    pub(crate) fn zarr_dtype(self) -> &'static str {
        match self {
            PixelType::U8 => "|u1",
            PixelType::U16 => "<u2",
            PixelType::F32 => "<f4",
        }
    }

    /// OME-XML `Pixels` type.
    pub(crate) fn ome_type(self) -> &'static str {
        match self {
            PixelType::U8 => "uint8",
            PixelType::U16 => "uint16",
            PixelType::F32 => "float",
        }
    }

    /// Largest value of an integer type, which bounds its display range.
    pub(crate) fn max(self) -> Option<f64> {
        match self {
            PixelType::U8 => Some(u8::MAX as f64),
            PixelType::U16 => Some(u16::MAX as f64),
            PixelType::F32 => None,
        }
    }
}

/// One decoded plane at one resolution, row-major.
#[non_exhaustive]
pub enum Plane {
    U8(Vec<u8>),
    U16(Vec<u16>),
    F32(Vec<f32>),
}

/// Image geometry and metadata of one scope, from the XML header.
#[derive(Debug, Clone)]
pub struct SisHeader {
    /// The scope's `visibleName`; can be empty.
    pub name: String,
    /// Sample type of every channel-plane.
    pub pixel_type: PixelType,
    /// Image size, tiling and stored resolutions, shared by every channel-plane.
    pub grid: TileGrid,
    /// Time points; every one holds `size_z` planes.
    pub size_t: u64,
    /// Z-planes per time point.
    pub size_z: u64,
    /// Channels, in index order.
    pub channels: Vec<ChannelInfo>,
    /// Physical pixel size in µm: x, y, z.
    pub pixel_size: [f64; 3],
}

impl SisHeader {
    /// Channel-planes stored for this scope: one per time point, z-plane and channel.
    pub(crate) fn plane_count(&self) -> usize {
        self.size_t as usize * self.size_z as usize * self.channels.len()
    }

    /// Position of a channel-plane in stored order: time point by time point, then
    /// z-plane by z-plane, then channel by channel.
    pub(crate) fn plane_index(&self, t: u64, z: u64, c: u64) -> usize {
        ((t * self.size_z + z) as usize * self.channels.len()) + c as usize
    }
}

/// An opened `.sis` file: every scope's header and the data chunks it indexes.
pub struct SisReader {
    data: Mmap,
    scopes: Vec<(SisHeader, Vec<Chunk>)>,
}

impl SisReader {
    /// Open `path`, parsing its header and tile index once.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::path::Path;
    /// use sis2zarr::SisReader;
    ///
    /// let reader = SisReader::open(Path::new("sample.sis"))?;
    /// for scope in reader.scopes() {
    ///     let h = scope.header();
    ///     println!("{}: {}x{} px, {} z, {} t",
    ///         h.name, h.grid.width(), h.grid.height(), h.size_z, h.size_t);
    /// }
    /// # Ok::<(), sis2zarr::SisError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`SisError::Io`] if the file cannot be opened or mapped,
    /// [`SisError::Format`] if it is not a well-formed `.sis` file, and
    /// [`SisError::Unsupported`] if any scope uses a layout this crate has not been
    /// verified against.
    pub fn open(path: &Path) -> Result<Self, SisError> {
        let file = File::open(path)?;
        // SAFETY: the file is opened read-only and only read through this map.
        let data = unsafe { Mmap::map(&file)? };
        let scopes = parse(&data)?;
        Ok(Self { data, scopes })
    }

    /// Every scope (image) in the file, in stored order.
    pub fn scopes(&self) -> Vec<Scope<'_>> {
        self.scopes
            .iter()
            .map(|(header, chunks)| Scope {
                data: &self.data,
                header,
                chunks,
            })
            .collect()
    }
}

/// One image stored in a `.sis` file: an XML `<Scope>` element and the chunks it indexes.
#[derive(Clone, Copy)]
pub struct Scope<'a> {
    data: &'a [u8],
    header: &'a SisHeader,
    chunks: &'a [Chunk],
}

impl<'a> Scope<'a> {
    /// The scope's image geometry, channels and pixel size.
    pub fn header(&self) -> &'a SisHeader {
        self.header
    }

    /// Decode one z-plane of one channel at time point `t` at every stored resolution,
    /// full resolution first, each cropped to [`TileGrid::level_size`].
    ///
    /// # Errors
    ///
    /// Returns [`SisError::Io`] if a gzip chunk is corrupt, and [`SisError::Format`] if the
    /// plane's chunks do not hold exactly one plane of samples.
    pub fn read_levels(&self, t: u64, z: u64, c: u64) -> Result<Vec<Plane>, SisError> {
        let h = self.header;
        let per_plane = self.chunks.len() / h.plane_count();
        let first = h.plane_index(t, z, c) * per_plane;

        let expected = h.grid.samples() * h.pixel_type.size();
        // Chunks are equal slices of the plane's stream; an empty one stands for zeros.
        let chunk_len = expected / per_plane;
        let mut raw = Vec::with_capacity(expected);
        for &(offset, len, gzip) in &self.chunks[first..first + per_plane] {
            let bytes = &self.data[offset as usize..(offset + len) as usize];
            if len == 0 {
                raw.resize(raw.len() + chunk_len, 0);
            } else if gzip {
                // Stop one byte past the plane, so a chunk that unpacks to far more than it
                // should fails the length check below instead of filling memory.
                let room = (expected - raw.len().min(expected)) as u64 + 1;
                GzDecoder::new(bytes).take(room).read_to_end(&mut raw)?;
            } else {
                raw.extend_from_slice(bytes);
            }
        }
        if raw.len() != expected {
            return Err(format_err(format!(
                "plane z={z} c={c} holds {} bytes, expected {expected}",
                raw.len()
            )));
        }
        Ok(match h.pixel_type {
            PixelType::U8 => decode(raw.iter().copied(), &h.grid)
                .into_iter()
                .map(Plane::U8)
                .collect(),
            PixelType::U16 => {
                let stream = raw
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]));
                decode(stream, &h.grid)
                    .into_iter()
                    .map(Plane::U16)
                    .collect()
            }
            PixelType::F32 => {
                let stream = raw
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes(b.try_into().unwrap()));
                decode(stream, &h.grid)
                    .into_iter()
                    .map(Plane::F32)
                    .collect()
            }
        })
    }
}

/// Rebuild every resolution of a plane from its stored order.
///
/// Each level but the last is cut into tiles of the grid's tile size, in row-major
/// order. Each tile stores its odd rows, then its even rows at odd columns. Its even rows
/// at even columns make up the next level, at half the size. The last level is stored as
/// those even-even samples of the previous level's tiles, tile by tile.
fn decode<T: Copy + Default>(mut src: impl Iterator<Item = T>, grid: &TileGrid) -> Vec<Vec<T>> {
    let (tw, th) = grid.tile_size();
    let last = grid.level_count() - 1;
    let sizes: Vec<(usize, usize)> = (0..=last).map(|k| grid.padded_size(k)).collect();
    let mut levels: Vec<Vec<T>> = sizes
        .iter()
        .map(|&(w, hgt)| vec![T::default(); w * hgt])
        .collect();
    // Tile rectangles (y0, y1, x0, x1) of level k, clipped to the level.
    let tiles = |k: usize| {
        let (w, hgt) = sizes[k];
        (0..hgt).step_by(th).flat_map(move |y0| {
            (0..w)
                .step_by(tw)
                .map(move |x0| (y0, (y0 + th).min(hgt), x0, (x0 + tw).min(w)))
        })
    };

    for k in 0..last {
        let w = sizes[k].0;
        for (y0, y1, x0, x1) in tiles(k) {
            for y in (y0 + 1..y1).step_by(2) {
                for x in x0..x1 {
                    levels[k][y * w + x] = src.next().unwrap();
                }
            }
            for y in (y0..y1).step_by(2) {
                for x in (x0 + 1..x1).step_by(2) {
                    levels[k][y * w + x] = src.next().unwrap();
                }
            }
        }
    }
    let coarse_w = sizes[last].0;
    for (y0, y1, x0, x1) in tiles(last - 1) {
        for y in (y0..y1).step_by(2) {
            for x in (x0..x1).step_by(2) {
                levels[last][y / 2 * coarse_w + x / 2] = src.next().unwrap();
            }
        }
    }
    for k in (0..last).rev() {
        let ((w, hgt), next_w) = (sizes[k], sizes[k + 1].0);
        for y in (0..hgt).step_by(2) {
            for x in (0..w).step_by(2) {
                levels[k][y * w + x] = levels[k + 1][y / 2 * next_w + x / 2];
            }
        }
    }

    // Crop the tile padding in place: a second copy of a plane costs hundreds of MB.
    levels
        .into_iter()
        .enumerate()
        .map(|(k, mut img)| {
            let (w, hgt) = grid.level_size(k);
            let (w, hgt, padded_w) = (w as usize, hgt as usize, sizes[k].0);
            for y in 1..hgt {
                img.copy_within(y * padded_w..y * padded_w + w, y * w);
            }
            img.truncate(w * hgt);
            img
        })
        .collect()
}

fn parse(data: &[u8]) -> Result<Vec<(SisHeader, Vec<Chunk>)>, SisError> {
    if !data.starts_with(MAGIC) {
        return Err(format_err("missing the '#Single Image Stack' first line"));
    }
    let line_end = data[MAGIC.len()..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|i| MAGIC.len() + i)
        .ok_or_else(|| format_err("truncated header line"))?;
    let header_len: usize = std::str::from_utf8(&data[MAGIC.len()..line_end])
        .ok()
        .and_then(|line| line.strip_prefix("Header = "))
        .and_then(|n| n.trim().parse().ok())
        .ok_or_else(|| format_err("bad 'Header = <length>' line"))?;

    // The header block runs from '{' to '}' and is exactly `header_len` bytes long.
    let start = line_end + 1;
    let block = data
        .get(start..start + header_len)
        .ok_or_else(|| format_err("header length exceeds file size"))?;
    if block.first() != Some(&b'{') || block.last() != Some(&b'}') {
        return Err(format_err("header block is not enclosed in braces"));
    }
    let text = String::from_utf8_lossy(block);
    let xml_start = text
        .find("<Header")
        .ok_or_else(|| format_err("no <Header> element"))?;
    let xml_end = text
        .find("</Header>")
        .ok_or_else(|| format_err("no </Header> element"))?
        + "</Header>".len();
    let headers = parse_xml(&text[xml_start..xml_end])?;

    let chunks = tile_index::read(data, start + header_len)?;

    // Chunks run scope by scope, each in its plane order, with the same number of chunks
    // for every channel-plane.
    let planes: usize = headers.iter().map(SisHeader::plane_count).sum();
    if planes == 0 || chunks.is_empty() || !chunks.len().is_multiple_of(planes) {
        return Err(format_err(format!(
            "{} chunks do not divide evenly into {planes} channel-planes",
            chunks.len()
        )));
    }
    let per_plane = chunks.len() / planes;
    let mut rest = &chunks[..];
    Ok(headers
        .into_iter()
        .map(|header| {
            let (own, after) = rest.split_at(header.plane_count() * per_plane);
            rest = after;
            (header, own.to_vec())
        })
        .collect())
}

/// Parse every scope's header. In a file with several scopes, errors name the scope they
/// are about.
fn parse_xml(xml: &str) -> Result<Vec<SisHeader>, SisError> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format_err(e.to_string()))?;
    let root = doc.root_element();

    let scopes: Vec<_> = root
        .descendants()
        .filter(|n| n.has_tag_name("Scope"))
        .collect();
    if scopes.is_empty() {
        return Err(format_err("no <Scope> elements"));
    }
    let parse_scope =
        |scope: roxmltree::Node<'_, '_>, name: String| -> Result<SisHeader, SisError> {
            let pixel_type = match attr(find(scope, "PixelType")?, "name")?.as_str() {
                "unsigned char" => PixelType::U8,
                "unsigned short" => PixelType::U16,
                "float" => PixelType::F32,
                other => return Err(SisError::Unsupported(format!("pixel type '{other}'"))),
            };

            let ticks: Vec<_> = scope
                .descendants()
                .filter(|n| n.has_tag_name("Tick"))
                .collect();
            let per_tick: Vec<Vec<_>> = ticks
                .iter()
                .map(|t| t.children().filter(|n| n.has_tag_name("Plane")).collect())
                .collect();
            let size_z = per_tick
                .first()
                .ok_or_else(|| format_err("no time points"))?
                .len();
            if per_tick.iter().any(|planes| planes.len() != size_z) {
                return Err(SisError::Unsupported(
                    "time points hold different numbers of planes".into(),
                ));
            }
            let planes: Vec<_> = per_tick.concat();
            let first = *planes.first().ok_or_else(|| format_err("no planes"))?;
            let geometry = |plane: roxmltree::Node<'_, '_>| -> Result<[u64; 6], SisError> {
                let (rect, tiles) = (find(plane, "Rect")?, find(plane, "Tiles")?);
                Ok([
                    whole(rect, "width")?,
                    whole(rect, "height")?,
                    whole(tiles, "width")?,
                    whole(tiles, "height")?,
                    whole(tiles, "tileNumX")?,
                    whole(tiles, "tileNumY")?,
                ])
            };
            let [width, height, tile_width, tile_height, tiles_x, tiles_y] = geometry(first)?;
            for plane in &planes {
                if geometry(*plane)? != geometry(first)?
                    || whole(*plane, "zoomLevel")? != whole(first, "zoomLevel")?
                {
                    return Err(SisError::Unsupported(
                        "planes differ in size, tiling or zoom level".into(),
                    ));
                }
            }
            let grid = TileGrid::new(
                width,
                height,
                (tile_width, tile_height),
                (tiles_x, tiles_y),
                whole(first, "zoomLevel")?,
            )?;

            let size = find(scope, "PixelSize")?;
            let pixel_size = [
                num(size, "width")?,
                num(size, "height")?,
                num(size, "depth")?,
            ];

            Ok(SisHeader {
                name,
                pixel_type,
                grid,
                size_t: ticks.len() as u64,
                size_z: size_z as u64,
                channels: read_channels(root, scope)?,
                pixel_size,
            })
        };
    scopes
        .iter()
        .enumerate()
        .map(|(i, &scope)| {
            let name = attr(scope, "visibleName")?;
            match parse_scope(scope, name.clone()) {
                Ok(header) => Ok(header),
                Err(e) if scopes.len() > 1 => Err(e.in_scope(i, &name)),
                Err(e) => Err(e),
            }
        })
        .collect()
}

pub(crate) fn find<'a, 'i>(
    node: roxmltree::Node<'a, 'i>,
    name: &str,
) -> Result<roxmltree::Node<'a, 'i>, SisError> {
    node.descendants()
        .find(|n| n.has_tag_name(name))
        .ok_or_else(|| format_err(format!("no <{name}> element")))
}

pub(crate) fn attr(node: roxmltree::Node<'_, '_>, name: &str) -> Result<String, SisError> {
    node.attribute(name)
        .map(str::to_owned)
        .ok_or_else(|| format_err(format!("<{}> has no {name}", node.tag_name().name())))
}

pub(crate) fn num(node: roxmltree::Node<'_, '_>, name: &str) -> Result<f64, SisError> {
    attr(node, name)?.parse().map_err(|_| {
        format_err(format!(
            "<{}> {name} is not a number",
            node.tag_name().name()
        ))
    })
}

fn whole(node: roxmltree::Node<'_, '_>, name: &str) -> Result<u64, SisError> {
    attr(node, name)?.parse().map_err(|_| {
        format_err(format!(
            "<{}> {name} is not a whole number",
            node.tag_name().name()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(width: u64, height: u64, tw: u64, th: u64, tiles_x: u64, tiles_y: u64) -> TileGrid {
        zoomed(width, height, tw, th, tiles_x, tiles_y, 1)
    }

    fn zoomed(
        width: u64,
        height: u64,
        tw: u64,
        th: u64,
        tiles_x: u64,
        tiles_y: u64,
        zoom_level: u64,
    ) -> TileGrid {
        TileGrid::new(width, height, (tw, th), (tiles_x, tiles_y), zoom_level).unwrap()
    }

    /// Full-resolution level of a decoded plane.
    fn deinterleave<T: Copy + Default>(stream: &[T], h: &TileGrid) -> Vec<T> {
        decode(stream.iter().copied(), h).swap_remove(0)
    }

    #[test]
    fn decode_restores_two_tiles_and_crops_padding() {
        // Two 4x2 tiles; pixel value = 10*y + x across the 8-wide plane.
        let px = |y: u16, x: u16| 10 * y + x;
        let mut stream = vec![];
        for x0 in [0, 4] {
            stream.extend((0..4).map(|x| px(1, x0 + x)));
            stream.extend([1, 3].map(|x| px(0, x0 + x)));
        }
        for x0 in [0, 4] {
            stream.extend([0, 2].map(|x| px(0, x0 + x)));
        }

        let img = deinterleave(&stream, &grid(7, 2, 4, 2, 2, 1));

        let expected: Vec<u16> = (0..2).flat_map(|y| (0..7).map(move |x| px(y, x))).collect();
        assert_eq!(img, expected);
    }

    /// Every stored sample lands in exactly one pixel, and cropping keeps the top-left of
    /// the full tile grid, for every even tile size up to 16x16 and grids up to 4x4.
    #[test]
    fn decode_is_a_cropped_permutation() {
        for (tw, th) in (2..=16)
            .step_by(2)
            .flat_map(|w| (2..=16).step_by(2).map(move |h| (w, h)))
        {
            for (tiles_x, tiles_y) in (1..=4).flat_map(|x| (1..=4).map(move |y| (x, y))) {
                let (full_w, full_h) = (tw * tiles_x, th * tiles_y);
                let stream: Vec<u32> = (0..(full_w * full_h) as u32).collect();

                let full = deinterleave(&stream, &grid(full_w, full_h, tw, th, tiles_x, tiles_y));
                let mut sorted = full.clone();
                sorted.sort_unstable();
                assert_eq!(sorted, stream, "{tw}x{th} tiles, {tiles_x}x{tiles_y} grid");

                for (crop_x, crop_y) in [(1, 0), (0, 1), (tw - 1, th - 1)] {
                    let (w, h) = (full_w - crop_x, full_h - crop_y);
                    let cropped = deinterleave(&stream, &grid(w, h, tw, th, tiles_x, tiles_y));
                    let expected: Vec<u32> = full
                        .chunks_exact(full_w as usize)
                        .take(h as usize)
                        .flat_map(|row| &row[..w as usize])
                        .copied()
                        .collect();
                    assert_eq!(cropped, expected, "{tw}x{th} tiles cropped to {w}x{h}");
                }
            }
        }
    }

    #[test]
    fn decode_places_tile_rows_in_row_major_order() {
        // A 2x2 grid of 2x2 tiles; pixel value = 10*y + x across the 4x4 plane.
        let px = |y: u16, x: u16| 10 * y + x;
        let origins = [(0, 0), (0, 2), (2, 0), (2, 2)];
        let mut stream = vec![];
        for (y0, x0) in origins {
            stream.extend([px(y0 + 1, x0), px(y0 + 1, x0 + 1), px(y0, x0 + 1)]);
        }
        for (y0, x0) in origins {
            stream.push(px(y0, x0));
        }

        let img = deinterleave(&stream, &grid(3, 4, 2, 2, 2, 2));

        let expected: Vec<u16> = (0..4).flat_map(|y| (0..3).map(move |x| px(y, x))).collect();
        assert_eq!(img, expected);
    }

    #[test]
    fn decode_stores_each_level_before_the_next() {
        // zoomLevel 3: a 4x4 plane of 2x2 tiles, then a 2x2 level (one tile), then 1x1.
        // Pixel value = 10*y + x at full resolution.
        let px = |y: u16, x: u16| 10 * y + x;
        let mut stream = vec![];
        for (y0, x0) in [(0, 0), (0, 2), (2, 0), (2, 2)] {
            stream.extend([px(y0 + 1, x0), px(y0 + 1, x0 + 1), px(y0, x0 + 1)]);
        }
        // Level 1's only tile, in full-resolution coordinates.
        stream.extend([px(2, 0), px(2, 2), px(0, 2)]);
        // Level 2, as the even-even sample of level 1's tile.
        stream.push(px(0, 0));

        let levels = decode(stream.iter().copied(), &zoomed(4, 4, 2, 2, 2, 2, 3));

        let full: Vec<u16> = (0..4).flat_map(|y| (0..4).map(move |x| px(y, x))).collect();
        assert_eq!(
            levels,
            vec![full, vec![px(0, 0), px(0, 2), px(2, 0), px(2, 2)], vec![0]]
        );
    }

    /// With several stored levels, level 0 is still a permutation of the stream and every
    /// level is level 0 sampled at every 2^k-th row and column, including partial edge
    /// tiles at the coarser levels.
    #[test]
    fn decode_levels_are_subsampled_full_resolution() {
        for (tw, th, tiles_x, tiles_y, zoom) in [
            (4, 4, 4, 4, 3),
            (6, 4, 4, 2, 2),
            (4, 8, 12, 8, 4),
            (8, 4, 3, 4, 4),
        ] {
            let (full_w, full_h) = (tw * tiles_x, th * tiles_y);
            let stream: Vec<u32> = (0..(full_w * full_h) as u32).collect();
            let h = zoomed(full_w - 1, full_h - 1, tw, th, tiles_x, tiles_y, zoom);

            let levels = decode(stream.iter().copied(), &h);
            let full = decode(
                stream.iter().copied(),
                &zoomed(full_w, full_h, tw, th, tiles_x, tiles_y, zoom),
            )
            .swap_remove(0);
            let mut sorted = full.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, stream, "{tw}x{th} tiles, zoom {zoom}");

            assert_eq!(levels.len(), h.level_count());
            for (k, level) in levels.iter().enumerate() {
                let (w, lh) = h.level_size(k);
                let step = 1 << k;
                let expected: Vec<u32> = (0..lh as usize)
                    .flat_map(|y| (0..w as usize).map(move |x| (y * step, x * step)))
                    .map(|(y, x)| full[y * full_w as usize + x])
                    .collect();
                assert_eq!(level, &expected, "level {k}, {tw}x{th} tiles, zoom {zoom}");
            }
        }
    }
}
