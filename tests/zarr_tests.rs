mod common;

use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::TempDir;
use zarrs::array::ElementOwned;

use common::{
    LAYOUTS, Layout, build_sis_file, convert, multi_scope_sis_file, multi_time_sis_file, read_json,
    read_level, scope_value, sis_file, time_value, value,
};

/// Each channel-plane's value at `w` x `h`, in c, z, y, x order.
fn expected<T: From<u8>>(w: u64, h: u64) -> Vec<T> {
    (0..2)
        .flat_map(|c| (0..2).map(move |z| (z, c)))
        .flat_map(|(z, c)| (0..w * h).map(move |_| T::from(value(z, c))))
        .collect()
}

fn convert_as(pixel_type: &str, sample: impl Fn(u8) -> Vec<u8>) -> (TempDir, PathBuf) {
    convert(&build_sis_file(Layout::V34, pixel_type, sample, |xml| xml))
}

fn assert_level<T: ElementOwned + From<u8> + PartialEq + std::fmt::Debug>(
    out: &Path,
    level: usize,
    (w, h): (u64, u64),
) {
    let (shape, px) = read_level::<T>(out, 0, level);
    assert_eq!(shape, [2, 2, h, w]);
    assert_eq!(px, expected::<T>(w, h));
}

#[test]
fn convert_to_zarr_is_public() {
    let _: fn(&Path, &Path) -> Result<(), sis2zarr::SisError> = sis2zarr::convert_to_zarr;
}

#[test]
fn every_tile_index_layout_gives_the_same_pixels() {
    for layout in LAYOUTS {
        let file = build_sis_file(layout, "unsigned short", common::u16_sample, |xml| xml);
        let (_dir, out) = convert(&file);
        let (_, px) = read_level::<u16>(&out, 0, 0);
        assert_eq!(px, expected::<u16>(5, 3), "{layout:?}");
    }
}

#[test]
fn planes_are_written_in_channel_z_order_and_cropped() {
    let (_dir, out) = convert(&sis_file());
    assert_level::<u16>(&out, 0, (5, 3));
}

#[test]
fn lower_level_is_half_size_rounded_up() {
    let (_dir, out) = convert(&sis_file());
    assert_level::<u16>(&out, 1, (3, 2));
}

#[test]
fn array_dtype_is_u2_for_unsigned_short() {
    let (_dir, out) = convert(&sis_file());
    assert_eq!(read_json(&out.join("0/0/.zarray"))["dtype"], "<u2");
}

#[test]
fn unsigned_char_pixels_are_written_as_u1() {
    let (_dir, out) = convert_as("unsigned char", |v| vec![v]);
    assert_eq!(read_json(&out.join("0/0/.zarray"))["dtype"], "|u1");
    assert_level::<u8>(&out, 0, (5, 3));
}

#[test]
fn float_pixels_are_written_as_f4() {
    let (_dir, out) = convert_as("float", |v| (v as f32).to_le_bytes().into());
    assert_eq!(read_json(&out.join("0/0/.zarray"))["dtype"], "<f4");
    assert_level::<f32>(&out, 0, (5, 3));
}

#[test]
fn multiscales_name_is_the_scope_name() {
    let (_dir, out) = convert(&sis_file());
    assert_eq!(
        read_json(&out.join("0/.zattrs"))["multiscales"][0]["name"],
        "Stack"
    );
}

#[test]
fn datasets_scale_xy_by_two_per_level() {
    let (_dir, out) = convert(&sis_file());
    let datasets = &read_json(&out.join("0/.zattrs"))["multiscales"][0]["datasets"];
    let scale = |level: usize| &datasets[level]["coordinateTransformations"][0]["scale"];
    assert_eq!(scale(0), &json!([1.0, 2.0, 0.25, 0.5]));
    assert_eq!(scale(1), &json!([1.0, 2.0, 0.5, 1.0]));
}

#[test]
fn omero_channel_uses_recorded_name_color_and_window() {
    let (_dir, out) = convert(&sis_file());
    let ch = &read_json(&out.join("0/.zattrs"))["omero"]["channels"][0];
    assert_eq!(ch["label"], "Alpha");
    assert_eq!(ch["color"], "0A141E");
    assert_eq!(ch["window"]["start"], 5.0);
    assert_eq!(ch["window"]["end"], 90.0);
}

#[test]
fn omero_channel_without_settings_uses_defaults_and_data_range() {
    let (_dir, out) = convert(&sis_file());
    let ch = &read_json(&out.join("0/.zattrs"))["omero"]["channels"][1];
    assert_eq!(ch["label"], "Channel 2");
    assert_eq!(ch["color"], "00FF00");
    assert_eq!(ch["window"]["start"], 0.0);
    assert_eq!(ch["window"]["end"], 111.0);
}

#[test]
fn omero_window_max_is_the_integer_type_max() {
    let (_dir, out) = convert(&sis_file());
    let ch = &read_json(&out.join("0/.zattrs"))["omero"]["channels"][0];
    assert_eq!(ch["window"]["min"], 0.0);
    assert_eq!(ch["window"]["max"], 65535.0);
}

#[test]
fn ome_xml_records_type_sizes_and_physical_sizes() {
    let (_dir, out) = convert(&sis_file());
    let xml = std::fs::read_to_string(out.join("OME/METADATA.ome.xml")).unwrap();
    for attr in [
        r#"Type="uint16""#,
        r#"BigEndian="false""#,
        r#"SizeX="5""#,
        r#"SizeY="3""#,
        r#"SizeZ="2""#,
        r#"SizeC="2""#,
        r#"PhysicalSizeX="0.5""#,
        r#"PhysicalSizeY="0.25""#,
        r#"PhysicalSizeZ="2""#,
    ] {
        assert!(xml.contains(attr), "missing {attr}");
    }
}

#[test]
fn ome_xml_uses_channel_names() {
    let (_dir, out) = convert(&sis_file());
    let xml = std::fs::read_to_string(out.join("OME/METADATA.ome.xml")).unwrap();
    assert!(xml.contains(r#"Name="Alpha""#));
    assert!(xml.contains(r#"Name="Channel 2""#));
}

#[test]
fn root_declares_the_bioformats2raw_layout() {
    let (_dir, out) = convert(&sis_file());
    assert_eq!(
        read_json(&out.join(".zattrs")),
        json!({"bioformats2raw.layout": 3})
    );
}

#[test]
fn ome_group_lists_one_series_per_scope() {
    let (_dir, out) = convert(&multi_scope_sis_file(3));
    assert_eq!(
        read_json(&out.join("OME/.zattrs"))["series"],
        json!(["0", "1", "2"])
    );
}

#[test]
fn each_scope_is_its_own_image_group() {
    let (_dir, out) = convert(&multi_scope_sis_file(2));
    for scope in 0..2 {
        let (shape, px) = read_level::<u16>(&out, scope, 0);
        assert_eq!(shape, [2, 2, 3, 5]);
        let expected: Vec<u16> = (0..2)
            .flat_map(|c| (0..2).map(move |z| (z, c)))
            .flat_map(|(z, c)| (0..15).map(move |_| scope_value(scope, z, c) as u16))
            .collect();
        assert_eq!(px, expected, "scope {scope}");
    }
}

#[test]
fn image_groups_are_named_after_their_scopes() {
    let (_dir, out) = convert(&multi_scope_sis_file(2));
    let name =
        |i: usize| read_json(&out.join(format!("{i}/.zattrs")))["multiscales"][0]["name"].clone();
    assert_eq!(name(0), "Stack");
    assert_eq!(name(1), "Stack 2");
}

#[test]
fn ome_xml_has_one_image_per_scope_in_group_order() {
    let (_dir, out) = convert(&multi_scope_sis_file(2));
    let xml = std::fs::read_to_string(out.join("OME/METADATA.ome.xml")).unwrap();
    let first = xml.find(r#"<Image ID="Image:0" Name="Stack">"#).unwrap();
    let second = xml.find(r#"<Image ID="Image:1" Name="Stack 2">"#).unwrap();
    assert!(first < second);
    assert!(xml.contains(r#"<Pixels ID="Pixels:1""#));
    assert!(xml.contains(r#"<Channel ID="Channel:1:0""#));
}

#[test]
fn refuses_to_write_into_an_existing_output() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("in.sis");
    let output = dir.path().join("out.ome.zarr");
    std::fs::write(&input, sis_file()).unwrap();
    std::fs::create_dir_all(output.join("1")).unwrap();

    assert!(sis2zarr::convert_to_zarr(&input, &output).is_err());
    assert!(!output.join("0").exists(), "wrote into the existing store");
}

#[test]
fn time_points_become_a_leading_t_axis() {
    let (_dir, out) = convert(&multi_time_sis_file(3));
    let (shape, px) = read_level::<u16>(&out, 0, 0);
    assert_eq!(shape, [3, 2, 2, 3, 5]);
    let expected: Vec<u16> = (0..3)
        .flat_map(|t| (0..2).flat_map(move |c| (0..2).map(move |z| (t, z, c))))
        .flat_map(|(t, z, c)| (0..15).map(move |_| time_value(t, z, c) as u16))
        .collect();
    assert_eq!(px, expected);
    let attrs = read_json(&out.join("0/.zattrs"));
    assert_eq!(attrs["multiscales"][0]["axes"][0]["type"], "time");
    let xml = std::fs::read_to_string(out.join("OME/METADATA.ome.xml")).unwrap();
    assert!(xml.contains(r#"SizeT="3""#));
}

#[test]
fn channel_elements_outside_channels_do_not_change_the_planes() {
    // A <Channel> elsewhere in a scope is not one of its stored channels.
    let file = common::build_sis_file(Layout::V34, "unsigned short", common::u16_sample, |xml| {
        xml.replace(
            "<Ticks>",
            r#"<Extra><Channel index="9" name="stray" id="99"/></Extra><Ticks>"#,
        )
    });
    let (_dir, out) = convert(&file);
    assert_level::<u16>(&out, 0, (5, 3));
}
