"""Why calls in one language resolve as they do: a sample of guesses (and of
calls that reached nothing though a project function has the name), with
their receiver chains.

    python3 bench/trace.py REPO_DIR .ext [guess|named|exact] [N]
"""
import collections
import os
import random
import sys

os.environ["SEAMCHECK_TRACE"] = "1"
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

root, ext = sys.argv[1], sys.argv[2]
want = sys.argv[3] if len(sys.argv) > 3 else "guess"
n = int(sys.argv[4]) if len(sys.argv) > 4 else 30
p = core.load(root, use_cache=False)
rows = [r for r in core._TRACE if r[0].endswith(ext) and r[4] == want]
print(f"{len(rows)} {want} calls")
heads = collections.Counter((r[3][0] if r[3] else "<bare>") for r in rows)
print("first segment:", heads.most_common(15))
print("name:", collections.Counter(r[2] for r in rows).most_common(15))
random.seed(int(os.environ.get("SEED", "3")))
for f, line, name, chain, _, tg in random.sample(rows, min(n, len(rows))):
    src = p.by_file[f][0].raw[line - 1].strip()[:90] if f in p.by_file else ""
    print(f"{f}:{line} {'.'.join(chain)}.{name}  -> {', '.join(tg)}\n      {src}")
