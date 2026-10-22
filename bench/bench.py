"""How exactly seamcheck links calls, per language, on real open-source repos.

    python3 bench/bench.py REPOS_DIR [--only lang,lang]

REPOS_DIR holds shallow clones named as below (`git clone --depth 1
https://github.com/<owner>/<repo>`). For each language and repo it prints the
share of calls into project code that are exact, and how many calls resolved
to nothing although a project function has that name ("named"): a resolver
that turns guesses into nothing raises the first number and shows in the
second. The target is >= 90% exact with "named" not rising.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys

CORPUS = {
    "Python": ([".py"], ["pallets/flask", "encode/httpx", "psf/requests"]),
    "Go": ([".go"], ["spf13/cobra", "junegunn/fzf"]),
    "Rust": ([".rs"], ["BurntSushi/ripgrep", "sharkdp/bat"]),
    "TypeScript": ([".ts", ".tsx", ".mts"], ["excalidraw/excalidraw", "honojs/hono", "plausible/analytics"]),
    "JavaScript": ([".js", ".jsx", ".mjs", ".cjs"], ["expressjs/express", "hexpm/hexpm", "plausible/analytics"]),
    "Java": ([".java"], ["google/gson", "jhy/jsoup"]),
    "Kotlin": ([".kt", ".kts"], ["android/nowinandroid", "square/okhttp"]),
    "C#": ([".cs"], ["App-vNext/Polly", "JamesNK/Newtonsoft.Json"]),
    "Elixir": ([".ex", ".exs"], ["plausible/analytics", "hexpm/hexpm", "phoenixframework/phoenix"]),
    "GDScript": ([".gd"], ["godotengine/godot-demo-projects", "godotengine/tps-demo"]),
    "C++": ([".cpp", ".cc", ".h", ".hpp"], ["flameshot-org/flameshot", "KDE/kdeconnect-kde"]),
    "QML": ([".qml"], ["KDE/kdeconnect-kde", "Swordfish90/cool-retro-term", "KDE/neochat"]),
    "Swift": ([".swift"], ["Alamofire/Alamofire", "apple/swift-argument-parser"]),
    "Ruby": ([".rb"], ["sinatra/sinatra", "jekyll/jekyll"]),
    "PHP": ([".php"], ["slimphp/Slim", "guzzle/guzzle"]),
}
HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def stats(repo_dir: str) -> dict:
    env = dict(os.environ, PYTHONPATH=HERE)
    r = subprocess.run([sys.executable, "-m", "seamcheck", "-C", repo_dir, "stats", "--json", "--no-cache"],
                       capture_output=True, text=True, env=env, cwd=HERE)
    for line in reversed(r.stdout.splitlines()):
        if line.startswith("{"):
            return json.loads(line)
    raise RuntimeError(r.stdout[-2000:] + r.stderr[-2000:])


def main() -> int:
    root = sys.argv[1]
    only = set(sys.argv[sys.argv.index("--only") + 1].split(",")) if "--only" in sys.argv else None
    cache: dict[str, dict] = {}
    worst = 100.0
    for lang, (exts, repos) in CORPUS.items():
        if only and lang not in only:
            continue
        for slug in repos:
            d = os.path.join(root, slug.split("/")[1])
            if not os.path.isdir(d):
                print(f"{lang:<11} {slug:<34} (not cloned)")
                continue
            if d not in cache:
                cache[d] = stats(d)
            tot = {"exact": 0, "guess": 0, "nothing": 0, "named": 0}
            for ext in exts:
                for k, v in cache[d].get(ext, {}).items():
                    tot[k] += v
            calls = tot["exact"] + tot["guess"] + tot["nothing"]
            if not calls:
                print(f"{lang:<11} {slug:<34} no calls parsed")
                worst = 0
                continue
            share = 100.0 * tot["exact"] / max(1, tot["exact"] + tot["guess"])
            worst = min(worst, share)
            print(f"{lang:<11} {slug:<34} {calls:>7} calls  exact {share:5.1f}%  "
                  f"named-nothing {100.0 * tot['named'] / calls:4.1f}%")
    print(f"\nlowest: {worst:.1f}%")
    return 0


if __name__ == "__main__":
    sys.exit(main())
