//! Read-only probe: `verify_probe <file>` checks the Authenticode signature and
//! that the signer is "Parsec Cloud, Inc.". Also prints the driver status and
//! the custom modes currently in the registry. Never runs the file.
use vdd_app::{driver, modes};

fn main() {
    let st = driver::status();
    println!("driver status: {st:?}");
    println!("custom modes: {:?}", modes::read_custom_modes());
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // `--download <dir>`: fetch the official installer there (never run it) and verify it.
    if let Some(i) = args.iter().position(|a| a == "--download") {
        let dir = args.get(i + 1).cloned().unwrap_or_else(|| ".".into());
        match driver::download_installer(std::path::Path::new(&dir)) {
            Ok(p) => args.push(p.to_string_lossy().into_owned()),
            Err(e) => println!("download FAILED: {e:#}"),
        }
        args.retain(|a| a != "--download" && a != &dir);
    }
    for path in args {
        match driver::verify_signature(std::path::Path::new(&path)) {
            Ok(signer) => println!("OK   {path}: signed by {signer}"),
            Err(e) => println!("FAIL {path}: {e:#}"),
        }
    }
}
