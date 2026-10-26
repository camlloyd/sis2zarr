//! The tile grid a channel-plane is stored on, and the resolutions it holds.

use crate::error::{SisError, format_err};

/// Image size, tile size, tile counts and stored resolutions of every channel-plane in a
/// scope. `TileGrid::new` checks the layout, so a grid always has even tiles that cover
/// the image and resolutions that halve evenly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileGrid {
    width: u64,
    height: u64,
    tile_width: u64,
    tile_height: u64,
    tiles_x: u64,
    tiles_y: u64,
    zoom_level: u64,
}

impl TileGrid {
    /// A `width` x `height` image on `tiles_x` x `tiles_y` tiles of `tile_width` x
    /// `tile_height`, with the planes' `zoomLevel` attribute.
    pub(crate) fn new(
        width: u64,
        height: u64,
        (tile_width, tile_height): (u64, u64),
        (tiles_x, tiles_y): (u64, u64),
        zoom_level: u64,
    ) -> Result<Self, SisError> {
        if [width, height, tile_width, tile_height, tiles_x, tiles_y].contains(&0) {
            return Err(format_err(format!(
                "empty image or tile grid: {width}x{height} image, {tiles_x}x{tiles_y} tiles of \
                 {tile_width}x{tile_height}"
            )));
        }
        // shortcut: the stored pixel order is only verified on even-sized tiles; support odd
        // sizes once a sample file with them is available.
        if !tile_width.is_multiple_of(2) || !tile_height.is_multiple_of(2) {
            return Err(SisError::Unsupported(format!(
                "odd tile size {tile_width}x{tile_height}"
            )));
        }
        if width > tile_width * tiles_x || height > tile_height * tiles_y {
            return Err(format_err("image is larger than its tiles"));
        }
        let grid = Self {
            width,
            height,
            tile_width,
            tile_height,
            tiles_x,
            tiles_y,
            zoom_level,
        };
        // Every level that stores tiles halves in size, so it must have even dimensions.
        for k in 0..grid.level_count() - 1 {
            let (w, h) = grid.padded_size(k);
            if !w.is_multiple_of(2) || !h.is_multiple_of(2) {
                return Err(SisError::Unsupported(format!(
                    "resolution {k} is {w}x{h}, which cannot be halved"
                )));
            }
        }
        Ok(grid)
    }

    /// Image width at full resolution.
    pub fn width(&self) -> u64 {
        self.width
    }

    /// Image height at full resolution.
    pub fn height(&self) -> u64 {
        self.height
    }

    /// Number of resolutions stored per plane, full resolution first. Each level but the
    /// last stores the pixels its half-resolution successor lacks.
    // shortcut: zoomLevel 1 and 4 are the only values seen (2 and 4 levels); other values
    // follow the 4 case untested. Revisit with a zoomLevel 2 or 3 sample.
    pub fn level_count(&self) -> usize {
        self.zoom_level.saturating_sub(1).max(1) as usize + 1
    }

    /// Width and height of resolution `level`, cropped to the image.
    pub fn level_size(&self, level: usize) -> (u64, u64) {
        (
            self.width.div_ceil(1 << level),
            self.height.div_ceil(1 << level),
        )
    }

    /// Width and height of resolution `level` including the tile padding.
    pub(crate) fn padded_size(&self, level: usize) -> (usize, usize) {
        (
            (self.tile_width * self.tiles_x) as usize >> level,
            (self.tile_height * self.tiles_y) as usize >> level,
        )
    }

    /// Tile width and height, the same at every level.
    pub(crate) fn tile_size(&self) -> (usize, usize) {
        (self.tile_width as usize, self.tile_height as usize)
    }

    /// Samples stored per plane, all resolutions together.
    pub(crate) fn samples(&self) -> usize {
        let (w, h) = self.padded_size(0);
        w * h
    }
}
