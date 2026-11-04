from datetime import datetime, timezone


def utc_now():
    return datetime.now(timezone.utc)


def utc_display(v):
    return v.strftime("%Y-%m-%d %H:%M")
