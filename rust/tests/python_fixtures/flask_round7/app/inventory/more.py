from flask import Blueprint, request, send_file
from app.utils.pdf_generator import render_pdf
bp = Blueprint('mo', __name__)

@bp.route('/t')
def trailing_comment():
    items = [request.args.get('d', '')]
    pdf_buffer = render_pdf('x.html', x=items)  # the report
    return send_file(pdf_buffer, download_name='x.pdf')

@bp.route('/d')
def docstring_between():
    items = [request.args.get('d', '')]
    pdf_buffer = render_pdf('x.html', x=items)
    """Log export"""
    return send_file(pdf_buffer, download_name='x.pdf')

@bp.route('/b')
def blank_between():
    items = [request.args.get('d', '')]
    pdf_buffer = render_pdf('x.html', x=items)

    return send_file(pdf_buffer, download_name='x.pdf')
