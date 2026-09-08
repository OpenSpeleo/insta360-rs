"""Exercise a repaired wheel on a host without any FFmpeg shared libraries."""

from importlib.metadata import distribution
from pathlib import Path
import sys
import tempfile

import insta360_rs


def smoke(source: Path) -> None:
    print(insta360_rs.capabilities())
    info = insta360_rs.probe(source)
    assert info.video_tracks, "fixture must contain a video track"
    media = insta360_rs.open_media(source)
    frame = media.video_streams[0].open_video().read_frame()
    assert frame is not None and frame.width == 32 and frame.height == 16
    with tempfile.TemporaryDirectory() as directory:
        report = insta360_rs.extract(source, Path(directory) / "extracted")
        assert report.stream_count == 1
        assert report.manifest_path.is_file()
    files = distribution("insta360-rs").files
    assert files is not None
    assert any(str(path).endswith("py.typed") for path in files)
    assert any(str(path).endswith("__init__.pyi") for path in files)
    print("Repaired wheel: import, probe, decode, extraction, and typing files passed")


if __name__ == "__main__":
    smoke(Path(sys.argv[1]))
