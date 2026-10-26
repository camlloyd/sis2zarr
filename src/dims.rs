//! The axes of a scope's OME-Zarr image: which there are, their order, and the shape,
//! chunking, plane slices and scale at each resolution level (OME-NGFF 0.4 `axes` and
//! `multiscales`).

use std::ops::Range;

use ome_zarr_metadata::v0_4::{Axis, AxisType, AxisUnit, AxisUnitSpace};

use crate::reader::SisHeader;

/// Largest chunk side; a chunk holds one channel of one z-plane at one time point.
const CHUNK_SIZE: u64 = 1024;

/// A scope's image dimensions: always t, c, z, y, x, even with a single time point.
pub(crate) struct Dims<'a> {
    header: &'a SisHeader,
}

impl<'a> Dims<'a> {
    pub(crate) fn new(header: &'a SisHeader) -> Self {
        Self { header }
    }

    /// NGFF axes, in array order.
    pub(crate) fn axes(&self) -> Vec<Axis> {
        let space = |name: &str| Axis {
            name: name.into(),
            r#type: Some(AxisType::Space),
            unit: Some(AxisUnit::Space(AxisUnitSpace::Micrometer)),
        };
        vec![
            // The file's tick spacing is not confirmed, so time is in steps, not seconds.
            Axis {
                name: "t".into(),
                r#type: Some(AxisType::Time),
                unit: None,
            },
            Axis {
                name: "c".into(),
                r#type: Some(AxisType::Channel),
                unit: None,
            },
            space("z"),
            space("y"),
            space("x"),
        ]
    }

    /// Axis names, in array order.
    pub(crate) fn names(&self) -> Vec<&'static str> {
        vec!["t", "c", "z", "y", "x"]
    }

    /// Array shape of resolution `level`.
    pub(crate) fn shape(&self, level: usize) -> Vec<u64> {
        let h = self.header;
        let (w, hgt) = h.grid.level_size(level);
        vec![h.size_t, h.channels.len() as u64, h.size_z, hgt, w]
    }

    /// Chunk shape of resolution `level`: one channel of one z-plane at one time point, at
    /// most `CHUNK_SIZE` on a side.
    pub(crate) fn chunks(&self, level: usize) -> Vec<u64> {
        let (w, hgt) = self.header.grid.level_size(level);
        vec![1, 1, 1, hgt.min(CHUNK_SIZE), w.min(CHUNK_SIZE)]
    }

    /// The region of resolution `level` that channel-plane (t, z, c) fills.
    pub(crate) fn plane(&self, level: usize, t: u64, z: u64, c: u64) -> Vec<Range<u64>> {
        let (w, hgt) = self.header.grid.level_size(level);
        vec![t..t + 1, c..c + 1, z..z + 1, 0..hgt, 0..w]
    }

    /// NGFF scale of resolution `level`, one entry per axis.
    pub(crate) fn scale(&self, level: usize) -> Vec<f32> {
        let [px, py, pz] = self.header.pixel_size;
        let step = (1u64 << level) as f64;
        vec![1.0, 1.0, pz as f32, (py * step) as f32, (px * step) as f32]
    }

    /// Every channel-plane (t, z, c).
    pub(crate) fn planes(&self) -> impl Iterator<Item = (u64, u64, u64)> + Send + use<> {
        let h = self.header;
        let (t, z, c) = (h.size_t, h.size_z, h.channels.len() as u64);
        (0..t).flat_map(move |ti| (0..z).flat_map(move |zi| (0..c).map(move |ci| (ti, zi, ci))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::ChannelInfo;
    use crate::grid::TileGrid;
    use crate::reader::PixelType;

    /// A 2-channel, 3-plane scope with `t` time points, `width` x 3 pixels on 4x2 tiles,
    /// pixel size 0.5 x 0.25 x 2 µm.
    fn header(t: u64, width: u64) -> SisHeader {
        let channel = ChannelInfo {
            name: String::new(),
            color_rgb: [0; 3],
            window: None,
        };
        SisHeader {
            name: "s".into(),
            pixel_type: PixelType::U16,
            grid: TileGrid::new(width, 3, (4, 2), (width.div_ceil(4), 2), 1).unwrap(),
            size_t: t,
            size_z: 3,
            channels: vec![channel.clone(), channel],
            pixel_size: [0.5, 0.25, 2.0],
        }
    }

    #[test]
    fn single_time_image_keeps_a_t_axis() {
        let h = header(1, 5);
        let d = Dims::new(&h);
        assert_eq!(d.names(), ["t", "c", "z", "y", "x"]);
        assert_eq!(d.shape(0), [1, 2, 3, 3, 5]);
        assert_eq!(d.shape(1), [1, 2, 3, 2, 3]);
        assert_eq!(d.chunks(0), [1, 1, 1, 3, 5]);
        assert_eq!(d.scale(1), [1.0, 1.0, 2.0, 0.5, 1.0]);
        assert_eq!(d.plane(1, 0, 2, 1), [0..1, 1..2, 2..3, 0..2, 0..3]);
    }

    #[test]
    fn time_series_has_t_first() {
        let h = header(4, 5);
        let d = Dims::new(&h);
        assert_eq!(d.names(), ["t", "c", "z", "y", "x"]);
        assert_eq!(d.shape(0), [4, 2, 3, 3, 5]);
        assert_eq!(d.chunks(0), [1, 1, 1, 3, 5]);
        assert_eq!(d.scale(0), [1.0, 1.0, 2.0, 0.25, 0.5]);
        assert_eq!(d.plane(0, 3, 2, 1), [3..4, 1..2, 2..3, 0..3, 0..5]);
        let t = &d.axes()[0];
        assert!(matches!(t.r#type, Some(AxisType::Time)));
        assert!(
            t.unit.is_none(),
            "tick spacing is unconfirmed, so time has no unit"
        );
    }

    /// The spec's rules: axes ordered time, channel, space, and as many axes as array
    /// dimensions and scale entries.
    #[test]
    fn axes_follow_ngff_order_and_match_the_arrays() {
        for t in [1, 4] {
            let h = header(t, 5);
            let d = Dims::new(&h);
            let kinds: Vec<u8> = d
                .axes()
                .iter()
                .map(|a| match a.r#type {
                    Some(AxisType::Time) => 0,
                    Some(AxisType::Channel) => 1,
                    _ => 2,
                })
                .collect();
            assert!(kinds.is_sorted(), "{kinds:?}");
            assert_eq!(kinds.iter().filter(|&&k| k == 2).count(), 3);
            for level in 0..h.grid.level_count() {
                assert_eq!(d.shape(level).len(), d.axes().len());
                assert_eq!(d.scale(level).len(), d.axes().len());
                assert_eq!(d.chunks(level).len(), d.axes().len());
            }
        }
    }

    #[test]
    fn chunks_are_capped_at_chunk_size() {
        let h = header(1, 3000);
        assert_eq!(*Dims::new(&h).chunks(0).last().unwrap(), CHUNK_SIZE);
    }

    #[test]
    fn planes_cover_every_time_point_z_and_channel_once() {
        let h = header(4, 5);
        let planes: Vec<_> = Dims::new(&h).planes().collect();
        assert_eq!(planes.len(), 4 * 3 * 2);
        let mut unique = planes.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), planes.len());
    }
}
