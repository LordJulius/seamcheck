from flask import Blueprint, request, send_file
from app.utils.pdf_generator import render_pdf
bp = Blueprint('inv', __name__)

@bp.route('/1')
def no_comment():                       # clean
    items = [request.args.get('d', '')]
    pdf_buffer = render_pdf('x.html', x=items)
    return send_file(pdf_buffer, download_name='x.pdf')

@bp.route('/2')
def comment_between():                  # FIRES
    items = [request.args.get('d', '')]
    pdf_buffer = render_pdf('x.html', x=items)
    # Log export
    return send_file(pdf_buffer, download_name='x.pdf')

@bp.route('/3')
def comment_before_call():              # clean
    items = [request.args.get('d', '')]
    # Build it
    pdf_buffer = render_pdf('x.html', x=items)
    return send_file(pdf_buffer, download_name='x.pdf')

@bp.route('/4')
def comment_between_str():              # FIRES
    name = request.args.get('d', '')
    pdf_buffer = render_pdf('x.html', x=name)
    # Log export
    return send_file(pdf_buffer, download_name='x.pdf')
