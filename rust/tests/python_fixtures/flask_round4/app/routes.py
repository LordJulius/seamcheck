from flask import Blueprint, abort, current_app, request, send_file

from app import db

bp = Blueprint("po", __name__)


@bp.route("/po/<int:id>/pdf")
def export_pdf(id):
    try:
        from app.utils.pdf_generator import pdf_filename, render_pdf

        po = db.session.get(PurchaseOrder, id)
        customer = request.args.get("customer")
        pdf_buffer = render_pdf(
            "client_po.html",
            po=po,
            customer=customer,
        )
        filename = pdf_filename("po", id)
        return send_file(pdf_buffer, mimetype="application/pdf", as_attachment=True, download_name=filename)
    except Exception as e:
        current_app.logger.exception(e)
        abort(500)
