from io import BytesIO

import weasyprint
from flask import render_template


def render_pdf(template_name, **context):
    html_string = render_template(template_name, **context)
    buf = BytesIO()
    weasyprint.HTML(string=html_string).write_pdf(buf)
    buf.seek(0)
    return buf


def format_currency(v):
    return f"${v:,.2f}"
