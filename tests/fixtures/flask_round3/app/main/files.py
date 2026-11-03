import io
import os
from urllib.parse import urlencode

from flask import redirect, request, send_file, url_for

from app.hosts import host_controller
from app.main import bp
from app.pdf import render_pdf


def mode_query_url(endpoint, include_request_args=False, **values):
    q = request.args.to_dict(flat=False) if include_request_args else {}
    q.pop("mode", None)
    return url_for(endpoint, **values) + "?" + urlencode(q, doseq=True)


@bp.route("/invoice/<int:inv>.pdf")
def invoice(inv):
    pdf_buffer = render_pdf("invoice.html", inv=inv, note=request.args.get("note"))
    return send_file(pdf_buffer, mimetype="application/pdf", download_name=f"invoice-{inv}.pdf")


@bp.route("/notes.txt")
def notes():
    buffer = io.BytesIO()
    buffer.write(request.args.get("text", "").encode())
    buffer.seek(0)
    return send_file(buffer, mimetype="text/plain")


@bp.route("/managed")
def managed():
    path = host_controller.resolve_managed_path(request.args["path"], must_exist=True)
    return send_file(path)


@bp.route("/raw")
def raw():
    return send_file(os.path.join("/srv/data", request.args["path"]))  # TP


@bp.route("/mode")
def mode():
    return redirect(mode_query_url("main.index", include_request_args=True, view="list"))


@bp.route("/theme")
def theme():
    return redirect(request.referrer or url_for("main.index"))  # a warning: back to the Referer


@bp.route("/done", methods=["POST"])
def done():
    return redirect(request.form.get("return_to"))  # TP: the field's real open redirect
