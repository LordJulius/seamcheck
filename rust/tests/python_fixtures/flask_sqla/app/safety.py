from urllib.parse import urlsplit


def coerce_safe_redirect_target(target):
    if not target or urlsplit(target).netloc != "" or not target.startswith("/"):
        return "/"
    return target
