import io

from flask import render_template


def render_pdf(template_name, **context):
    html = render_template(template_name, **context)
    buf = io.BytesIO()
    buf.write(html.encode())
    buf.seek(0)
    return buf
