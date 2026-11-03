from flask import Blueprint

bp = Blueprint("main", __name__)

from app.main import files  # noqa: E402,F401
