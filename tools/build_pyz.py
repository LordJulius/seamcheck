"""Build seamcheck.pyz: one file, any Python 3.11+, nothing to install.

    python3 tools/build_pyz.py [OUT]        (default: dist/seamcheck.pyz)
    python3 dist/seamcheck.pyz check

seamcheck reads its packs and data files from disk next to its modules, so
the archive's `__main__` unpacks the package once into a cache directory
named by the archive's content digest (~/.cache/seamcheck/pyz-<digest>) and
runs it from there: later runs start as fast as an install.
"""
import hashlib
import io
import os
import sys
import zipfile

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

BOOT = '''"""seamcheck, as one file (built by tools/build_pyz.py)."""
import os
import sys
import zipfile

DIGEST = "{digest}"


def _unpacked():
    base = os.environ.get("XDG_CACHE_HOME") or os.path.join(os.path.expanduser("~"), ".cache")
    root = os.path.join(base, "seamcheck", "pyz-" + DIGEST)
    if not os.path.exists(os.path.join(root, ".complete")):
        tmp = root + ".tmp-%d" % os.getpid()
        with zipfile.ZipFile(os.path.dirname(os.path.abspath(__file__))) as z:
            z.extractall(tmp, [n for n in z.namelist() if n.startswith("seamcheck/")])
        open(os.path.join(tmp, ".complete"), "w").close()
        try:
            os.replace(tmp, root)
        except OSError:  # another run unpacked it first
            import shutil
            shutil.rmtree(tmp, ignore_errors=True)
    return root


if sys.version_info < (3, 11):
    sys.exit("seamcheck needs Python 3.11 or newer (this is %d.%d)" % sys.version_info[:2])
sys.path.insert(0, _unpacked())
from seamcheck.__main__ import main  # noqa: E402

sys.exit(main())
'''


def build(out: str) -> str:
    files = []
    pkg = os.path.join(HERE, "seamcheck")
    for dirpath, dirs, names in os.walk(pkg):
        dirs[:] = [d for d in dirs if d != "__pycache__"]
        for n in sorted(names):
            if n.endswith((".py", ".toml", ".txt")):
                full = os.path.join(dirpath, n)
                files.append((os.path.relpath(full, HERE).replace(os.sep, "/"), full))
    files.sort()
    h = hashlib.sha256()
    for arc, full in files:
        with open(full, "rb") as fh:
            h.update(arc.encode() + b"\0" + fh.read())
    digest = h.hexdigest()[:16]
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("__main__.py", BOOT.format(digest=digest))
        for arc, full in files:
            z.write(full, arc)
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)
    with open(out, "wb") as fh:
        fh.write(b"#!/usr/bin/env python3\n")
        fh.write(buf.getvalue())
    os.chmod(out, 0o755)
    return digest


if __name__ == "__main__":
    target = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "dist", "seamcheck.pyz")
    d = build(target)
    print(f"{target} ({os.path.getsize(target) // 1024} KiB, {d})")
