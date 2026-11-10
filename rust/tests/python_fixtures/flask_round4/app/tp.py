import json
from io import BytesIO
from zipfile import ZipFile

from flask import Blueprint, request, send_file, session
from sqlalchemy import text

from app import db

bp = Blueprint("tp", __name__)


@bp.route("/export")
def export_tables():
    tables = request.args.getlist("tables") or ["customer", "department", "fuel_surcharge"]
    buf = BytesIO()
    with ZipFile(buf, "w") as zf:
        for table_name in tables:
            rows = db.session.execute(db.text(f"SELECT * FROM {table_name}")).all()  # TP: the tables come from the request
            zf.writestr(f"{table_name}.csv", str(rows))
    buf.seek(0)
    return send_file(buf, download_name="export.zip")


@bp.route("/restore/upload", methods=["POST"])
def restore_upload():
    upload = request.files["backup"]
    data = json.loads(upload.read())
    session["backup_data"] = json.dumps(data)
    return "ok"


@bp.route("/restore/apply", methods=["POST"])
def restore_apply():
    data = json.loads(session["backup_data"])
    for table, rows in data.items():
        db.session.execute(text(f"DELETE FROM {table}"))  # TP: the upload's keys, through the session
        for row in rows:
            cols = ", ".join(row.keys())
            db.session.execute(text(f"INSERT INTO {table} ({cols}) VALUES (:v)"), {"v": 1})  # TP
    return "done"
