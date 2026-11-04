from flask import Blueprint, request, send_file
bp = Blueprint('al', __name__)

@bp.route('/a')
def aliased():
    from app.utils.pdf_generator import render_pdf as rp
    buf = rp('x.html', x=request.args.get('d',''))
    return send_file(buf, download_name='x.pdf')

@bp.route('/b')
def plain():
    from app.utils.pdf_generator import render_pdf
    buf = render_pdf('x.html', x=request.args.get('d',''))
    return send_file(buf, download_name='x.pdf')
