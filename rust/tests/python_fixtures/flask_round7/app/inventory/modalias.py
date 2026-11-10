from flask import Blueprint, request, send_file
from app.utils.pdf_generator import render_pdf as make_pdf
bp = Blueprint('ma', __name__)

@bp.route('/m')
def module_alias():
    buf = make_pdf('x.html', x=request.args.get('d',''))
    return send_file(buf, download_name='x.pdf')
