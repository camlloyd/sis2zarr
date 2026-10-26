use std::fs;
use std::path::Path;
use std::sync::Arc;

use ome_zarr_metadata::v0_4::{
    Channel, Color, CoordinateTransform, CoordinateTransformScale, MultiscaleImage,
    MultiscaleImageDataset, OmeNgffGroupAttributes, Omero, Window,
};
use rayon::prelude::*;
use serde_json::json;
use zarrs::array::{Array, Element};
use zarrs::array_subset::ArraySubset;
use zarrs::filesystem::FilesystemStore;
use zarrs::group::Group;
use zarrs::metadata::v2::{
    ArrayMetadataV2, DataTypeMetadataV2, FillValueMetadataV2, GroupMetadataV2, MetadataV2,
};
use zarrs::metadata::{ChunkKeySeparator, GroupMetadata};

use crate::dims::Dims;
use crate::error::SisError;
use crate::reader::{Plane, Scope, SisHeader};

fn zarr_err<E: std::fmt::Display>(e: E) -> SisError {
    SisError::ZarrWrite(e.to_string())
}

/// Write every scope as OME-Zarr v0.4 in the `bioformats2raw` layout: image groups `0`,
/// `1`, … in scope order, and `OME/METADATA.ome.xml` with one `Image` per group.
pub(crate) fn write_ome_zarr(output: &Path, scopes: &[Scope<'_>]) -> Result<(), SisError> {
    let store = Arc::new(FilesystemStore::new(output).map_err(zarr_err)?);
    let name = output.file_name().unwrap_or_default().to_string_lossy();
    for (i, scope) in scopes.iter().enumerate() {
        write_image(&store, &format!("/{i}"), scope, &name)?;
    }

    let series: Vec<String> = (0..scopes.len()).map(|i| i.to_string()).collect();
    store_group(&store, "/", json!({"bioformats2raw.layout": 3}))?;
    store_group(&store, "/OME", json!({ "series": series }))?;
    let headers: Vec<&SisHeader> = scopes.iter().map(Scope::header).collect();
    write_ome_xml(output, &headers)
}

fn store_group(
    store: &Arc<FilesystemStore>,
    path: &str,
    attrs: serde_json::Value,
) -> Result<(), SisError> {
    let serde_json::Value::Object(attrs) = attrs else {
        unreachable!()
    };
    let group_meta: GroupMetadata = GroupMetadataV2::new().with_attributes(attrs).into();
    Group::new_with_metadata(store.clone(), path, group_meta)
        .map_err(zarr_err)?
        .store_metadata()
        .map_err(zarr_err)
}

/// Write one scope as an OME-Zarr v0.4 image at `path`, with the axes [`Dims`] gives and
/// one resolution level per resolution stored in the file. The stored lower levels are subsampled (every
/// 2nd, 4th, … pixel), not averaged.
fn write_image(
    store: &Arc<FilesystemStore>,
    path: &str,
    scope: &Scope<'_>,
    name: &str,
) -> Result<(), SisError> {
    let header = scope.header();
    let dims = Dims::new(header);

    let compressor: MetadataV2 = serde_json::from_value(json!({
        "id": "blosc",
        "cname": "zstd",
        "clevel": 1,
        "shuffle": 2,
        "blocksize": 0
    }))
    .map_err(zarr_err)?;
    let (dtype, max) = (header.pixel_type.zarr_dtype(), header.pixel_type.max());
    let arrays = (0..header.grid.level_count())
        .map(|level| {
            let array_meta = ArrayMetadataV2::new(
                dims.shape(level),
                dims.chunks(level).try_into().map_err(zarr_err)?,
                DataTypeMetadataV2::Simple(dtype.into()),
                FillValueMetadataV2::Number(serde_json::Number::from(0u16)),
                Some(compressor.clone()),
                None,
            )
            .with_dimension_separator(ChunkKeySeparator::Slash)
            .with_attributes(serde_json::Map::from_iter([(
                "_ARRAY_DIMENSIONS".to_string(),
                json!(dims.names()),
            )]));
            let array = Array::new_with_metadata(
                store.clone(),
                &format!("{path}/{level}"),
                array_meta.into(),
            )
            .map_err(zarr_err)?;
            array.store_metadata().map_err(zarr_err)?;
            Ok(array)
        })
        .collect::<Result<Vec<_>, SisError>>()?;

    // Each channel-plane covers its own chunks, so planes can be written in parallel.
    // Each also reports its coarsest level's data range, for display defaults when the
    // file records none.
    let plane_ranges = dims
        .planes()
        .par_bridge()
        .map(|(ti, zi, c)| {
            let mut range = (f64::INFINITY, f64::NEG_INFINITY);
            for (level, (plane, array)) in scope
                .read_levels(ti, zi, c)?
                .iter()
                .zip(&arrays)
                .enumerate()
            {
                let subset = ArraySubset::new_with_ranges(&dims.plane(level, ti, zi, c));
                let coarsest = level + 1 == arrays.len();
                let range = coarsest.then_some(&mut range);
                match plane {
                    Plane::U8(px) => store_level(array, &subset, px, range),
                    Plane::U16(px) => store_level(array, &subset, px, range),
                    Plane::F32(px) => store_level(array, &subset, px, range),
                }?;
            }
            Ok((c, range))
        })
        .collect::<Result<Vec<_>, SisError>>()?;
    let mut ranges = vec![(f64::INFINITY, f64::NEG_INFINITY); header.channels.len()];
    for (c, (lo, hi)) in plane_ranges {
        let r = &mut ranges[c as usize];
        *r = (r.0.min(lo), r.1.max(hi));
    }

    let channels = header
        .channels
        .iter()
        .zip(ranges)
        .map(|(ch, (lo, hi))| {
            let [r, g, b] = ch.color_rgb;
            let (lo, hi) = if lo <= hi { (lo, hi) } else { (0., 0.) };
            let (start, end) = ch.window.unwrap_or((lo, hi));
            Channel {
                color: Color { r, g, b },
                window: Window {
                    min: max.map_or(lo, |_| 0.),
                    max: max.unwrap_or(hi),
                    start,
                    end,
                },
                other: serde_json::Map::from_iter([
                    ("active".into(), json!(true)),
                    ("coefficient".into(), json!(1)),
                    ("family".into(), json!("linear")),
                    ("inverted".into(), json!(false)),
                    ("label".into(), json!(ch.name)),
                ]),
            }
        })
        .collect();

    let ome_attrs = OmeNgffGroupAttributes {
        multiscales: Some(vec![MultiscaleImage {
            version: Default::default(),
            name: Some(header.name.clone()),
            axes: dims.axes(),
            datasets: (0..arrays.len())
                .map(|level| MultiscaleImageDataset {
                    path: level.to_string(),
                    coordinate_transformations: vec![CoordinateTransform::Scale(
                        CoordinateTransformScale::List {
                            scale: dims.scale(level),
                        },
                    )],
                })
                .collect(),
            coordinate_transformations: None,
            r#type: None,
            metadata: None,
        }]),
        omero: Some(Omero {
            channels,
            other: serde_json::Map::from_iter([
                ("id".into(), json!(1)),
                ("name".into(), json!(name)),
                ("version".into(), json!("0.4")),
                (
                    "rdefs".into(),
                    json!({"defaultT": 0, "defaultZ": header.size_z / 2, "model": "color"}),
                ),
            ]),
        }),
        ..Default::default()
    };
    store_group(
        store,
        path,
        serde_json::to_value(ome_attrs).map_err(zarr_err)?,
    )
}

/// Store one level of a plane, widening `range` to its finite values when given.
fn store_level<T: Element + Copy + Into<f64>>(
    array: &Array<FilesystemStore>,
    subset: &ArraySubset,
    px: &[T],
    range: Option<&mut (f64, f64)>,
) -> Result<(), SisError> {
    if let Some(r) = range {
        for v in px.iter().map(|&v| v.into()).filter(|v: &f64| v.is_finite()) {
            *r = (r.0.min(v), r.1.max(v));
        }
    }
    array
        .store_array_subset_elements(subset, px)
        .map_err(zarr_err)
}

fn escape_xml_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Write `OME/METADATA.ome.xml`, one `Image` per scope in image-group order. QuPath reads
/// pixel sizes and z-spacing from here, not from the multiscales scale transforms.
fn write_ome_xml(output: &Path, headers: &[&SisHeader]) -> Result<(), SisError> {
    let images = headers
        .iter()
        .enumerate()
        .map(|(i, header)| ome_xml_image(i, header))
        .collect::<String>();
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><OME xmlns="http://www.openmicroscopy.org/Schemas/OME/2016-06" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" Creator="sis2zarr {version}" xsi:schemaLocation="http://www.openmicroscopy.org/Schemas/OME/2016-06 https://www.openmicroscopy.org/Schemas/OME/2016-06/ome.xsd">{images}</OME>"#,
        version = env!("CARGO_PKG_VERSION"),
    );
    fs::write(output.join("OME/METADATA.ome.xml"), xml).map_err(zarr_err)
}

fn ome_xml_image(i: usize, header: &SisHeader) -> String {
    let name = escape_xml_attr(&header.name);
    let pixel_type = header.pixel_type.ome_type();
    let [px, py, pz] = header.pixel_size;
    let channels = header
        .channels
        .iter()
        .enumerate()
        .map(|(c, ch)| {
            let [r, g, b] = ch.color_rgb;
            let color = u32::from_be_bytes([r, g, b, 255]) as i32;
            format!(
                r#"<Channel ID="Channel:{i}:{c}" Name="{}" Color="{color}" SamplesPerPixel="1"/>"#,
                escape_xml_attr(&ch.name)
            )
        })
        .collect::<String>();
    format!(
        r#"<Image ID="Image:{i}" Name="{name}"><Pixels ID="Pixels:{i}" DimensionOrder="XYZCT" Type="{pixel_type}" SizeX="{w}" SizeY="{h}" SizeZ="{z}" SizeC="{c}" SizeT="{t}" PhysicalSizeX="{px}" PhysicalSizeXUnit="µm" PhysicalSizeY="{py}" PhysicalSizeYUnit="µm" PhysicalSizeZ="{pz}" PhysicalSizeZUnit="µm">{channels}<MetadataOnly/></Pixels></Image>"#,
        w = header.grid.width(),
        h = header.grid.height(),
        z = header.size_z,
        c = header.channels.len(),
        t = header.size_t,
    )
}
