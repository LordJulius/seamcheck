import subprocess

from flask import Blueprint, request
from app.utils.reports import render_report as rr
bp = Blueprint('tp', __name__)


@bp.route('/logo')
def aliased_logo():
    return rr('x.html', logo_path=request.args['logo'])  # TP: through the alias, the keyword reaches open()


@bp.route('/read')
def comment_then_open():
    path = request.args['p']
    # Read it
    """Still the request's path."""
    with open(path) as fh:  # TP: a comment and a docstring change nothing
        return fh.read()


@bp.route('/grep')
def hash_in_string():
    pattern = f"{request.args['q']}#x"  # the '#' is in the string
    return subprocess.check_output(f"grep {pattern} log.txt", shell=True)  # TP
