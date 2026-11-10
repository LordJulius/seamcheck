from flask import Blueprint, jsonify, request

from app.b2w.db import lookup_b2w_ticket

bp = Blueprint("b2w", __name__)


@bp.route("/b2w/lookup")
def b2w_lookup_ticket():
    ticket = request.args.get("ticket", "")
    return jsonify(lookup_b2w_ticket(ticket, preview=request.args.get("preview") == "1"))
