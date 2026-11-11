"""Use embedded MSVC debug information for cached CI native compilations."""
from pathlib import Path
import re
import subprocess
import sys
import tempfile


def embedded_debug(flags):
    # /Z7 retains debug info in each object; /Zi and /ZI use a compiler PDB.
    return re.sub(r'(?<!\S)[/-]Z[iI](?!\S)', '/Z7', flags)


def resource_compilation(command):
    executable = command[0].replace('\\', '/').rsplit('/', 1)[-1].lower()
    if executable.endswith('.exe'):
        executable = executable[:-4]
    return executable in ('rc', 'llvm-rc', 'windres') or (
        executable == 'cmcldeps' and len(command) > 1 and command[1] == 'RC'
    )


# CMake's compile launcher also covers Windows resources. Its dependency helper
# and resource compiler are unsupported by sccache; preserve that command as-is.
if resource_compilation(sys.argv[2:]):
    sys.exit(subprocess.run(sys.argv[2:], check=False).returncode)


temporary = []
try:
    args = []
    for arg in sys.argv[1:]:
        if arg.startswith('@'):
            # Ninja may move flags into a response file for long commands.
            path = Path(arg[1:].strip('"'))
            data = path.read_bytes()
            encoding = 'utf-16' if data.startswith((b'\xff\xfe', b'\xfe\xff')) else 'utf-8'
            flags = data.decode(encoding)
            updated = embedded_debug(flags)
            if updated != flags:
                with tempfile.NamedTemporaryFile(dir=path.resolve().parent, suffix='.rsp', delete=False) as output:
                    temporary.append(Path(output.name))
                    output.write(updated.encode(encoding))
                arg = '@' + str(temporary[-1])
        else:
            arg = embedded_debug(arg)
        args.append(arg)
    result = subprocess.run(args, check=False)
finally:
    for path in temporary:
        path.unlink()
sys.exit(result.returncode)
