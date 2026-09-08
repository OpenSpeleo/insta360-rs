use std::fs::File;
use std::path::PathBuf;

use insta360_rs::InsvReader;

fn main() -> insta360_rs::Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| {
            insta360_rs::Error::InvalidMedia("usage: inspect <recording.insv>".into())
        })?;
    let file = File::open(&path).map_err(|error| insta360_rs::Error::Io {
        path: path.clone(),
        source: error,
    })?;
    let mut reader = InsvReader::new(file)?;
    let inspection = reader.inspect()?;

    println!("camera: {:?}", inspection.metadata.camera_name);
    println!("firmware: {:?}", inspection.metadata.firmware);
    println!("trailer: {:?}", inspection.trailer);
    println!("video tracks: {:?}", inspection.video_tracks);
    println!("offsets:");
    for offset in &inspection.metadata.offsets {
        println!(
            "  V{} original={} fields={} bytes={} value={}",
            offset.version,
            offset.original,
            offset.value.split('_').count(),
            offset.value.len(),
            offset.value,
        );
    }
    println!("profiles:");
    for profile in &inspection.metadata.profiles {
        println!(
            "  {} payload_bytes={} payload={:02x?}",
            profile.name,
            profile.payload.len(),
            profile.payload
        );
    }
    println!("records:");
    for record in &inspection.records {
        println!(
            "  id={} format={} offset={} size={}",
            record.id, record.format, record.offset, record.size
        );
    }
    Ok(())
}
