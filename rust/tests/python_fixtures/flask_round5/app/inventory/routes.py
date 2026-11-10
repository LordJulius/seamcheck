from flask import Blueprint, current_app, request, send_file

from app import db

bp = Blueprint("inventory", __name__)


@bp.route("/inventory/low-stock.pdf")
def low_stock_pdf():
    from app.utils.pdf_generator import format_currency, render_pdf

    threshold = request.args.get("threshold", "5")
    pdf_buffer = render_pdf("inventory_low_stock.html", threshold=threshold, fmt=format_currency)
    return send_file(pdf_buffer, mimetype="application/pdf", as_attachment=True, download_name="low-stock.pdf")


@bp.route("/inventory/valuation.pdf")
def valuation_pdf():
    try:
        from app.utils.pdf_generator import format_currency, render_pdf

        cat = request.args.get("category")
        pdf_buffer = render_pdf("inventory_valuation.html", category=cat, fmt=format_currency)
        return send_file(pdf_buffer, mimetype="application/pdf", as_attachment=True, download_name="valuation.pdf")
    except Exception as e:
        current_app.logger.exception(e)
        return "error", 500


@bp.route("/export")
def export():
    known = db.metadata.tables
    tables = request.args.getlist("tables") or list(known)
    tables = [name for name in tables if name in known]
    out = []
    for table_name in tables:
        out.append(db.session.execute(known[table_name].select()).all())
        db.session.execute(known[table_name].delete())
    return str(out)


@bp.route("/export-filtered")
def export_filtered():
    known = db.metadata.tables
    tables = request.args.getlist("tables")
    tables = [name for name in tables if name in known]
    for table_name in tables:
        db.session.execute(db.text(f"SELECT * FROM {table_name}"))  # (the allow-list alone)


@bp.route("/export-statements")
def export_statements():
    known = db.metadata.tables
    for table_name in request.args.getlist("tables"):
        db.session.execute(known[table_name].select())  # (no SQL text alone)


@bp.route("/export2")
def export2():
    tables = request.args.getlist("tables")
    for table_name in tables:
        db.session.execute(db.text(f"SELECT * FROM {table_name}"))  # TP


@bp.route("/help/<g>.pdf")
def help_pdf(g):
    from app.services.cksuper_help import render_pdf

    return render_pdf(request.args.get("g"))
