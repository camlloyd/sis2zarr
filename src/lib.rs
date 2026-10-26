//! Read arivis SIS files (`.sis`) and convert them to OME-Zarr v0.4 (Zarr v2). SIS stands
//! for Scalable Image Storage; older documentation, and the files' own first line, say
//! Single Image Stack.
//!
//! Two entry points:
//!
//! - [`SisReader`] — opens a `.sis` file and exposes every scope (image) in it, with its
//!   header and decoded planes.
//! - [`convert_to_zarr`] — decodes every plane in parallel and writes a multi-resolution
//!   OME-Zarr v0.4 store in the `bioformats2raw` layout.
//!
//! These are not the Olympus SIS TIFF files that Bio-Formats reads: they hold an XML
//! header, a tile index and 8-bit, 16-bit or float pixel data, raw or gzip-compressed,
//! in an interleaved order.
//!
//! # Examples
//!
//! ```no_run
//! use std::path::Path;
//!
//! sis2zarr::convert_to_zarr(Path::new("sample.sis"), Path::new("sample.ome.zarr"))?;
//! # Ok::<(), sis2zarr::SisError>(())
//! ```

pub(crate) mod channels;
pub(crate) mod dims;
pub(crate) mod error;
pub(crate) mod grid;
pub(crate) mod reader;
pub(crate) mod tile_index;
pub(crate) mod zarr;

use std::path::Path;

pub use channels::ChannelInfo;
pub use error::SisError;
pub use grid::TileGrid;
pub use reader::{PixelType, Plane, Scope, SisHeader, SisReader};

/// Read a `.sis` file and write every scope (image) in it to `output` as OME-Zarr v0.4,
/// in the `bioformats2raw` layout: image groups `0`, `1`, … in scope order.
///
/// `output` must not exist yet. Every scope's header and tile index are checked before
/// anything is written, so a file with an unsupported scope leaves no output.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use sis2zarr::convert_to_zarr;
///
/// convert_to_zarr(Path::new("sample.sis"), Path::new("sample.ome.zarr"))?;
/// # Ok::<(), sis2zarr::SisError>(())
/// ```
///
/// # Errors
///
/// Returns [`SisError::Io`] if `output` already exists or the input cannot be read,
/// [`SisError::Format`] or [`SisError::Unsupported`] if the input is malformed or uses a
/// layout this crate has not been verified against, and [`SisError::ZarrWrite`] if
/// writing the output store fails.
pub fn convert_to_zarr(input: &Path, output: &Path) -> Result<(), SisError> {
    // Writing into an earlier store would leave its other image groups behind.
    if output.exists() {
        return Err(SisError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("output path already exists: {}", output.display()),
        )));
    }
    let reader = SisReader::open(input)?;
    zarr::write_ome_zarr(output, &reader.scopes())
}
