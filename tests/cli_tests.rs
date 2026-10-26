mod common;

use std::fs;
use std::process::Command;

use tempfile::TempDir;

fn sis2zarr() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sis2zarr"))
}

#[test]
fn refuses_existing_output_without_overwrite() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("in.sis");
    let output = dir.path().join("existing.ome.zarr");
    fs::write(&input, common::sis_file()).unwrap();
    fs::create_dir(&output).unwrap();
    fs::write(output.join("sentinel"), b"keep me").unwrap();

    let result = sis2zarr().arg(&input).arg(&output).output().unwrap();

    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("output path already exists"));
    assert!(output.join("sentinel").exists());
}

#[test]
fn overwrite_removes_existing_output_file() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("in.sis");
    let output = dir.path().join("existing.ome.zarr");
    fs::write(&input, common::sis_file()).unwrap();
    fs::write(&output, b"old").unwrap();

    let result = sis2zarr()
        .arg(&input)
        .arg(&output)
        .arg("--overwrite")
        .output()
        .unwrap();

    assert!(result.status.success());
    assert!(output.join("0/0/.zarray").exists());
}

#[test]
fn overwrite_removes_existing_output_directory() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("in.sis");
    let output = dir.path().join("existing.ome.zarr");
    fs::write(&input, common::sis_file()).unwrap();
    fs::create_dir(&output).unwrap();
    fs::write(output.join("sentinel"), b"old").unwrap();

    let result = sis2zarr()
        .arg(&input)
        .arg(&output)
        .arg("--overwrite")
        .output()
        .unwrap();

    assert!(result.status.success());
    assert!(!output.join("sentinel").exists());
    assert!(output.join("0/0/.zarray").exists());
}

#[test]
fn threads_option_limits_the_pool_and_still_converts() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("in.sis");
    let output = dir.path().join("out.ome.zarr");
    fs::write(&input, common::sis_file()).unwrap();

    let result = sis2zarr()
        .arg(&input)
        .arg(&output)
        .args(["--threads", "1"])
        .output()
        .unwrap();

    assert!(result.status.success());
    assert!(output.join("0/0/.zarray").exists());
}

#[test]
fn overwrite_keeps_existing_output_when_input_is_invalid() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("in.sis");
    let output = dir.path().join("existing.ome.zarr");
    fs::write(&input, b"not a sis file").unwrap();
    fs::create_dir(&output).unwrap();
    fs::write(output.join("sentinel"), b"keep me").unwrap();

    let result = sis2zarr()
        .arg(&input)
        .arg(&output)
        .arg("--overwrite")
        .output()
        .unwrap();

    assert!(!result.status.success());
    assert!(output.join("sentinel").exists());
}
