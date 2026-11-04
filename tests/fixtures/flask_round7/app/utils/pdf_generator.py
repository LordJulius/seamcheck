import base64
from io import BytesIO

import weasyprint
from flask import render_template

from app.utils.time import utc_display, utc_now


def get_logo_base64():
    with open("app/static/logo.png", "rb") as fh:
        return base64.b64encode(fh.read()).decode()


def format_currency(v):
    return f"${v:,.2f}"


def render_pdf(template_name, **context):
    context.setdefault('logo_base64', get_logo_base64())
    context.setdefault('generated_at', utc_display(utc_now()))
    html_string = render_template(f'pdf/{template_name}', **context)
    buf = BytesIO()
    weasyprint.HTML(string=html_string).write_pdf(buf)
    buf.seek(0)
    return buf
