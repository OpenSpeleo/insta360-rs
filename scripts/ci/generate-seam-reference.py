"""Generate independent, full-tensor model-213 references with pinned CPU MNN.

Run through FrameForge's `make reference-insv-stitch`, or the independent
repository's prepared MNN_ROOT environment. No vendor runtime is executed.
"""

from array import array
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import statistics
import struct
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[2]
CASES = [(0, 0, False), (4, 0, False), (-4, 0, False), (0, 4, False),
         (0, -4, False), (4, 4, False), (4, 0, True)]
ORIGINAL_SHA = "a855aa2106101edd5c28ed0922d21223c2e420c3520917cbd7d371b7437e0cce"
DECODED_SHA = "2fc715cb0b4a8de1b079d74079039cfa2f32e22b4db182e7cff0014d603257cf"


def gray(x: int, y: int) -> float:
    """Byte-valued bilinear value noise on an eight-pixel integer lattice."""
    gx, rx = divmod(x, 8)
    gy, ry = divmod(y, 8)

    def lattice(a: int, b: int) -> int:
        value = ((a & 0xFFFFFFFF) * 0x9E3779B1 + (b & 0xFFFFFFFF) * 0x85EBCA77) & 0xFFFFFFFF
        value ^= value >> 16
        value = (value * 0x7FEB352D) & 0xFFFFFFFF
        return (value >> 24) & 255

    return float((lattice(gx, gy) * (8-rx) * (8-ry)
                  + lattice(gx+1, gy) * rx * (8-ry)
                  + lattice(gx, gy+1) * (8-rx) * ry
                  + lattice(gx+1, gy+1) * rx * ry) // 64)


def floats(values: list[float]) -> bytes:
    data = array("f", values)
    if sys.byteorder != "little":
        data.byteswap()
    return data.tobytes()


def generate() -> None:
    prefix = Path(os.environ["MNN_ROOT"])
    asset = ROOT / "data/ai-stitch-video/assets/models/ai-seam-studio-video-213/model.ins"
    original = asset.read_bytes()
    assert hashlib.sha256(original).hexdigest() == ORIGINAL_SHA
    # The reference decodes original bytes independently through OpenSSL. The
    # immutable wrapper key is read from its one production definition.
    source = (ROOT / "src-rust/mnn.rs").read_text()
    definition = re.search(r"const WRAPPER_KEY: \[u8; 32\] = \[([^]]+)\]", source)
    assert definition
    key = bytes(int(value) for value in re.findall(r"\d+", definition[1]))
    model = bytearray(original[1:-1])
    for start in (0, len(model)-2000):
        result = subprocess.run(["openssl", "enc", "-aes-256-ecb", "-d", "-nopad", "-K", key.hex()],
                                input=model[start:start+2000], capture_output=True, check=True)
        model[start:start+2000] = result.stdout
    assert hashlib.sha256(model).hexdigest() == DECODED_SHA
    with tempfile.TemporaryDirectory(prefix="insta360-seam-reference-") as temporary:
        directory = Path(temporary)
        executable = directory / "reference"
        compiler = shlex.split(os.environ.get("CXX", "c++"))
        command = [*compiler, "-std=c++17", *shlex.split(os.environ.get("CXXFLAGS", "")),
                   "-I", str(prefix / "include"), str(ROOT / "tests/reference/seam-model-reference.cpp"),
                   str(prefix / "lib/libMNN.a"), "-o", str(executable)]
        if sys.platform == "darwin":
            command.extend(["-framework", "Accelerate", "-framework", "Foundation"])
        else:
            command.extend(["-pthread", "-ldl"])
        subprocess.run(command, check=True)
        (directory / "model.mnn").write_bytes(model)
        with (directory / "inputs.bin").open("wb") as inputs:
            for dx, dy, masked in CASES:
                first = [gray(x, y) for y in range(544) for x in range(64)]
                second = [gray(x-dx, y-dy) for y in range(544) for x in range(64)]
                mask_first = [float(not masked or 8 <= x < 56) for _ in range(544) for x in range(64)]
                mask_second = [float(not masked or 4 <= x < 60) for _ in range(544) for x in range(64)]
                inputs.write(floats(first * 3 + second * 3 + mask_first + mask_second))
        subprocess.run([str(executable), str(directory / "model.mnn"), str(directory / "inputs.bin"),
                        str(directory / "outputs.bin"), str(len(CASES))], check=True)
        outputs = (directory / "outputs.bin").read_bytes()
    assert len(outputs) == len(CASES) * 2 * 2 * 136 * 16 * 4
    fixture = b"ISAIREF1" + struct.pack("<I", len(CASES)) + outputs
    destination = ROOT / "tests/fixtures/seam-mnn-reference-v1.bin"
    destination.write_bytes(fixture)
    measurements = []
    for index, (dx, dy, masked) in enumerate(CASES):
        values = struct.unpack_from("<8704f", outputs, index*8704*4)
        medians = [statistics.median(values[channel*2176+y*16+x]
                                    for y in range(16, 120) for x in range(4, 12))
                   for channel in range(4)]
        measurements.append({"dx": dx, "dy": dy, "masked": masked, "interior_medians_fx_fy_bx_by": medians})
    metadata = {"runtime": "MNN 3.6.1 CPU, Precision_High, one thread", "platform": sys.platform,
                "original_sha256": ORIGINAL_SHA, "decoded_sha256": DECODED_SHA,
                "fixture_sha256": hashlib.sha256(fixture).hexdigest(), "cases": measurements}
    (destination.with_suffix(".json")).write_text(json.dumps(metadata, indent=2) + "\n")
    print(json.dumps(metadata, indent=2))


if __name__ == "__main__":
    generate()
