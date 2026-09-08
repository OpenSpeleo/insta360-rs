//! Read an unstitched frame directly from an original recording, without output files.

use std::path::PathBuf;
use std::time::Duration;

use insta360_rs::{InputSet, MediaSource};

fn main() -> insta360_rs::Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| {
            insta360_rs::Error::InvalidMedia("usage: read_stream <recording.insv>".into())
        })?;
    let source = MediaSource::open(InputSet::discover(path)?)?;
    for video in source.video_streams() {
        let info = video.info();
        let mut frames = video.open_video()?;
        if let Some(frame) = frames.frame_at(Duration::from_secs(10))? {
            println!(
                "input {} stream {} ({}): {}x{} RGB frame at {:?}, {} bytes in memory",
                info.input_index,
                info.stream_index,
                info.codec,
                frame.width,
                frame.height,
                frame.timestamp,
                frame.data.len(),
            );
        }
    }
    Ok(())
}
