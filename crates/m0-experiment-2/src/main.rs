use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use m0_experiment_2::run_fixture;

#[derive(Debug, Parser)]
#[command(about = "Disposable M0 Experiment 2 operator-walk trace")]
struct Args {
    fixture_id: String,
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,
    #[arg(long)]
    pdfium_library: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let report = run_fixture(
        &args.repo_root,
        &args.fixture_id,
        args.pdfium_library.as_deref(),
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
