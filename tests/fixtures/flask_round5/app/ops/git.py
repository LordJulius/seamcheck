import shutil
import subprocess

from flask import Blueprint, request

bp = Blueprint("ops", __name__)


def _git(args):
    git_path = shutil.which("git") or "/usr/bin/git"
    return subprocess.run([git_path] + args, capture_output=True, check=False)


@bp.route("/ops/log")
def log():
    ref = request.args.get("ref", "HEAD")
    return _git(["log", "-n", "5", ref]).stdout


@bp.route("/ops/run")
def run():
    return subprocess.run(request.args["cmd"], shell=True).stdout  # TP
