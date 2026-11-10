import os
import subprocess
import tempfile

from flask import Blueprint, request

bp = Blueprint("mail", __name__)


def _coerce_text(v):
    return v if isinstance(v, str) else str(v or "")


def _normalize_filename(v):
    name = os.path.basename(_coerce_text(v)).strip()
    return name or "attachment"


def _run_attachment_command(args):
    return subprocess.run(args, check=False, capture_output=True)


@bp.route("/mail/attach", methods=["POST"])
def attach():
    upload = request.files["file"]
    name = _normalize_filename(upload.filename)
    tmp = tempfile.mkdtemp()
    pdf_path = os.path.join(tmp, name)
    with open(pdf_path, "wb") as fh:
        fh.write(upload.read())
    out = _run_attachment_command(["pdftotext", "-layout", "-nopgbrk", pdf_path, "-"])
    return out.stdout


@bp.route("/mail/attach2", methods=["POST"])
def attach2():
    filename = request.form["name"]
    filename = filename.replace("/", "_").replace("\\", "_")
    with tempfile.TemporaryDirectory() as d:
        with open(os.path.join(d, filename), "wb") as fh:
            fh.write(b"x")
    base = os.path.basename(request.form["other"])
    with open(os.path.join(tempfile.gettempdir(), base), "rb") as fh:
        return fh.read()


@bp.route("/mail/bad", methods=["POST"])
def bad():
    filename = request.form["name"].replace("/", "_")
    return open(os.path.join("/srv/mail", filename)).read()  # TP: only `/` replaced (`..\\x` climbs where `\\` separates)
