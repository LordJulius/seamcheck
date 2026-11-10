def render_report(template_name, **context):
    logo = context.get("logo_path", "app/static/logo.png")
    with open(logo, "rb") as fh:
        return template_name, fh.read()


def render_summary(template_name, **context):
    logo = context.get("logo_path", "app/static/logo.png")
    with open(logo, "rb") as fh:
        return template_name, fh.read()
