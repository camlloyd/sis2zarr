mod common;

use sis2zarr::{Plane, SisError, SisReader};

use common::{multi_scope_sis_file, scope_value, sis_file, sis_file_with, try_convert};

fn refusal(file: &[u8]) -> SisError {
    try_convert(file).1.unwrap_err()
}

fn assert_format(err: SisError, message: &str) {
    assert!(matches!(err, SisError::Format(_)), "{err:?}");
    assert!(err.to_string().contains(message), "{err}");
}

fn assert_unsupported(err: SisError, message: &str) {
    assert!(matches!(err, SisError::Unsupported(_)), "{err:?}");
    assert!(err.to_string().contains(message), "{err}");
}

#[test]
fn refuses_missing_magic() {
    let mut file = sis_file();
    file[1] = b'X';
    assert_format(refusal(&file), "first line");
}

#[test]
fn refuses_header_length_that_misses_closing_brace() {
    let mut file = sis_file();
    let at = file.windows(9).position(|w| w == b"Header = ").unwrap() + 9;
    let len: usize = std::str::from_utf8(&file[at..at + 8])
        .unwrap()
        .parse()
        .unwrap();
    // One byte short, so the block ends on the newline before its closing brace.
    file[at..at + 8].copy_from_slice(format!("{:08}", len - 1).as_bytes());
    assert_format(refusal(&file), "braces");
}

#[test]
fn refuses_non_numeric_attribute() {
    let file = sis_file_with(r#"PixelSize width="0.5""#, r#"PixelSize width="half""#);
    assert_format(refusal(&file), "not a number");
}

#[test]
fn refuses_missing_element() {
    let file = sis_file_with(r#"<PixelSize width="0.5" height="0.25" depth="2"/>"#, "");
    assert_format(refusal(&file), "no <PixelSize>");
}

#[test]
fn refuses_image_larger_than_its_tiles() {
    let file = sis_file_with(r#"Rect width="5""#, r#"Rect width="9""#);
    assert_format(refusal(&file), "larger than its tiles");
}

#[test]
fn refuses_chunk_count_that_does_not_divide_into_planes() {
    let file = sis_file_with(
        "</Channels>",
        r#"<Channel index="2" name="x" id="9"/></Channels>"#,
    );
    assert_format(refusal(&file), "do not divide");
}

#[test]
fn refuses_plane_with_wrong_byte_count() {
    let file = sis_file_with(r#"tileNumY="2""#, r#"tileNumY="3""#);
    assert_format(refusal(&file), "expected 96");
}

#[test]
fn refuses_unsupported_pixel_type() {
    let file = sis_file_with(r#"name="unsigned short""#, r#"name="double""#);
    assert_unsupported(refusal(&file), "pixel type 'double'");
}

#[test]
fn refuses_time_points_with_different_plane_counts() {
    let file = sis_file_with("</Tick>", "</Tick><Tick/>");
    assert_unsupported(refusal(&file), "different numbers of planes");
}

#[test]
fn refuses_planes_that_differ_in_size() {
    // Only the first plane's width changes.
    let file = common::build_sis_file(
        common::Layout::V34,
        "unsigned short",
        common::u16_sample,
        |xml| xml.replacen(r#"<Rect width="5""#, r#"<Rect width="4""#, 1),
    );
    assert_unsupported(refusal(&file), "planes differ");
}

#[test]
fn refuses_odd_tile_size() {
    let file = sis_file_with(r#"Tiles width="4""#, r#"Tiles width="3""#);
    assert_unsupported(refusal(&file), "odd tile size");
}

#[test]
fn refuses_levels_that_cannot_be_halved() {
    let file = sis_file_with(r#"zoomLevel="1""#, r#"zoomLevel="4""#);
    assert_unsupported(refusal(&file), "cannot be halved");
}

#[test]
fn refuses_whole_file_when_a_later_scope_is_unsupported() {
    // Only the second scope's pixel type changes, to an unknown name of the same length.
    let mut file = multi_scope_sis_file(2);
    let find = |file: &[u8], what: &[u8], from: usize| {
        from + file[from..]
            .windows(what.len())
            .position(|w| w == what)
            .unwrap()
    };
    let at = find(&file, br#"visibleName="Stack 2""#, 0);
    let at = find(&file, b"unsigned short", at);
    file[at..at + 14].copy_from_slice(b"unsigned shorx");
    let (dir, result) = try_convert(&file);

    assert_unsupported(result.unwrap_err(), r#"scope 1 "Stack 2": pixel type"#);
    assert!(!dir.path().join("out.ome.zarr").exists());
}

#[test]
fn refuses_zero_tile_size() {
    let file = common::build_sis_file(
        common::Layout::V34,
        "unsigned short",
        common::u16_sample,
        |xml| {
            xml.replace(r#"Rect width="5""#, r#"Rect width="0""#)
                .replace(r#"Tiles width="4""#, r#"Tiles width="0""#)
        },
    );
    assert_format(refusal(&file), "tile");
}

#[test]
fn refuses_fractional_tile_count() {
    let file = sis_file_with(r#"tileNumX="2""#, r#"tileNumX="2.5""#);
    assert_format(refusal(&file), "tileNumX");
}

#[test]
fn reader_opens_every_scope_with_its_name() {
    let dir = tempfile::TempDir::new().unwrap();
    let input = dir.path().join("in.sis");
    std::fs::write(&input, multi_scope_sis_file(2)).unwrap();

    let reader = SisReader::open(&input).unwrap();
    let scopes = reader.scopes();

    let names: Vec<&str> = scopes.iter().map(|s| s.header().name.as_str()).collect();
    assert_eq!(names, ["Stack", "Stack 2"]);
    let Plane::U16(full) = &scopes[1].read_levels(0, 1, 0).unwrap()[0] else {
        panic!("expected 16-bit samples");
    };
    assert_eq!(full, &vec![scope_value(1, 1, 0) as u16; 15]);
}

#[test]
fn single_scope_errors_do_not_name_the_scope() {
    let file = sis_file_with(r#"Tiles width="4""#, r#"Tiles width="3""#);
    let err = refusal(&file).to_string();
    assert!(!err.contains("scope 0"), "{err}");
}

#[test]
fn refuses_empty_tile_index() {
    let mut file = sis_file();
    // The 3.4 index starts with 16 bytes of 0xff and a u32 section length; a length of 20
    // means no entries, and the chunk data that follows is just ignored.
    let at = file.windows(16).position(|w| w == [0xff; 16]).unwrap() + 16;
    file[at..at + 4].copy_from_slice(&20u32.to_le_bytes());
    assert_format(refusal(&file), "do not divide");
}
