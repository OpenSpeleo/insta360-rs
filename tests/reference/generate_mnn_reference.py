"""Regenerate complete adapter references with a separate C++ MNN executable.

Requires MNN_ROOT, a C++17 compiler and the OpenSSL command-line tool. This is
maintenance tooling, never imported or executed by production or normal tests.
"""

import hashlib
import os
import pathlib
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[2]
KEY = bytes(
    [
        57,
        98,
        117,
        97,
        113,
        109,
        112,
        99,
        48,
        116,
        51,
        98,
        48,
        53,
        106,
        119,
        52,
        121,
        120,
        117,
        106,
        50,
        118,
        103,
        115,
        105,
        107,
        118,
        106,
        118,
        97,
        50,
    ]
)


def unwrap(original, expected):
    if original[0] != 0 or original[-1] != 0:
        raise ValueError("Unexpected model wrapper")
    decoded = bytearray(original[1:-1])
    for start in (0, len(decoded) - 2000):
        decoded[start : start + 2000] = subprocess.run(
            ["openssl", "enc", "-d", "-aes-256-ecb", "-nopad", "-K", KEY.hex()],
            input=decoded[start : start + 2000],
            capture_output=True,
            check=True,
        ).stdout
    if hashlib.sha256(decoded).hexdigest() != expected:
        raise ValueError(
            "Decoded model does not match the independently recorded identity"
        )
    return decoded


def main():
    prefix = pathlib.Path(os.environ["MNN_ROOT"])
    part0 = ROOT / "data/underwater-model-a/assets/underwater/model197.ins.part0"
    part1 = ROOT / "data/underwater-model-b/assets/underwater/model197.ins.part1"
    model198 = ROOT / "data/underwater-resources/assets/underwater/model198.ins"
    with tempfile.TemporaryDirectory(prefix="mnn-reference-") as temporary:
        temporary = pathlib.Path(temporary)
        first, second = temporary / "197.mnn", temporary / "198.mnn"
        first.write_bytes(
            unwrap(
                part0.read_bytes() + part1.read_bytes(),
                "67bd7fa68fdc488b9cd139a850ced8b59107745023f110896f6859f193a98736",
            )
        )
        second.write_bytes(
            unwrap(
                model198.read_bytes(),
                "e1b7c4189166c1bf4b451146c87d295d12850fc069f01976b5ef9aa077d19bad",
            )
        )
        executable = temporary / "reference"
        subprocess.run(
            [
                os.environ.get("CXX", "c++"),
                "-std=c++17",
                "-O2",
                str(ROOT / "tests/reference/mnn_reference.cpp"),
                "-I",
                str(prefix / "include"),
                str(prefix / "lib/libMNN.a"),
                "-pthread",
                "-o",
                str(executable),
            ],
            check=True,
        )
        output = ROOT / "tests/fixtures/underwater-mnn-reference-v1.bin"
        subprocess.run(
            [str(executable), str(first), str(second), str(output)], check=True
        )
        print(
            f"{output.name}: {len(output.read_bytes())} bytes, sha256 {hashlib.sha256(output.read_bytes()).hexdigest()}"
        )


if __name__ == "__main__":
    main()
