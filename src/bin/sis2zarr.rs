use clap::Parser;
use std::fs;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "sis2zarr", version)]
#[command(about = "Convert arivis SIS (.sis) files to OME-Zarr")]
struct Args {
    /// Input .sis file
    input: PathBuf,
    /// Output .ome.zarr store
    output: PathBuf,
    /// Overwrite the output store if it already exists
    #[arg(long)]
    overwrite: bool,
    /// Threads for plane decode and write. 0 (default) uses all CPU cores.
    #[arg(long, default_value_t = 0)]
    threads: usize,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    if args.threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(args.threads)
            .build_global()
            .map_err(|e| anyhow::anyhow!("failed to configure thread pool: {e}"))?;
    }

    if args.output.exists() {
        if !args.overwrite {
            anyhow::bail!(
                "output path already exists: {} (pass --overwrite to replace it)",
                args.output.display()
            );
        }

        // Check the input before removing anything, so a bad input keeps the old output.
        sis2zarr::SisReader::open(&args.input)?;
        let metadata = fs::symlink_metadata(&args.output)?;
        if metadata.is_dir() {
            fs::remove_dir_all(&args.output)?;
        } else {
            fs::remove_file(&args.output)?;
        }
    }

    eprintln!(
        "Converting {} → {}",
        args.input.display(),
        args.output.display()
    );
    sis2zarr::convert_to_zarr(&args.input, &args.output)?;
    eprintln!("Done.");
    Ok(())
}
