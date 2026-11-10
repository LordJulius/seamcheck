import weasyprint
from flask import render_template


def render_pdf(guide):
    html = render_template("help.html", guide=guide)
    return weasyprint.HTML(string=html).write_pdf()
