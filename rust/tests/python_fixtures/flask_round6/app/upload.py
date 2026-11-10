import os
import tempfile

from flask import Blueprint, request

bp = Blueprint("upload", __name__)


def _coerce_text(v):
    return v if isinstance(v, str) else str(v or "")


def _normalize_filename(v):
    name = os.path.basename(_coerce_text(v)).strip()
    return name or "attachment"


@bp.route("/upload", methods=["POST"])
def upload():
    f = request.files["file"]
    safe_name = _normalize_filename(f.filename)
    with tempfile.TemporaryDirectory() as temp_dir:
        p1 = os.path.join(temp_dir, safe_name)
        p2 = os.path.join(temp_dir, safe_name if safe_name.lower().endswith('.pdf') else f"{safe_name}.pdf")
        p3 = os.path.join(temp_dir, f"{safe_name}.pdf")
        for p in (p1, p2, p3):
            f.save(p)
        open(p1, "rb").close()
        open(p2, "rb").close()
        open(p3, "rb").close()
        open(os.path.join(temp_dir, f"../{safe_name}"), "rb").close()  # TP: the literal climbs out of temp_dir
    return "ok"
