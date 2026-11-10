# A second render_pdf, as an app keeps beside the one its routes import: the summary by name no longer
# agrees, the call resolved exactly still does.
def render_pdf(template, data):
    from weasyprint import HTML
    return HTML(string=template.format(**data)).write_pdf()
