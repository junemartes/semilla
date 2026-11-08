//! Serves the review UI with the demo data set on http://127.0.0.1:7878/.
//!
//! Run: `cargo run -p semilla-ui --example demo`
//! Options: `--addr HOST:PORT` sets the address. `--json FILE` writes the data set as JSON and stops.

use std::net::SocketAddr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut addr: SocketAddr = "127.0.0.1:7878".parse()?;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--addr" => addr = args.next().ok_or_else(|| anyhow::anyhow!("--addr needs a value"))?.parse()?,
            "--json" => {
                let path = args.next().ok_or_else(|| anyhow::anyhow!("--json needs a file"))?;
                std::fs::write(&path, serde_json::to_string_pretty(&semilla_ui::fixture::demo_data())?)?;
                eprintln!("wrote {path}");
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let review_file = std::env::temp_dir().join("semilla-demo").join("review.json");
    eprintln!("review state: {}", review_file.display());
    semilla_ui::serve(semilla_ui::fixture::demo_data(), review_file, addr).await
}
