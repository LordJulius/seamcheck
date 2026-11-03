import io
import os
import re
import subprocess
import sys
from io import BytesIO
from uuid import uuid4

import requests
from flask import current_app, redirect, render_template, request, send_file, send_from_directory, url_for
from flask_login import current_user, login_required
from sqlalchemy import select, text

from app import db
from app.main import bp
from app.models import Export, Post, User
from app.tasks import build_report
from app.safety import coerce_safe_redirect_target

NAME = re.compile(r"[a-z0-9_-]{1,64}\.pdf")


@bp.route("/user/<username>")
@login_required
def user(username):
    user = User.query.filter_by(username=username).first_or_404()
    page = request.args.get("page", 1, type=int)
    posts = user.recent_posts().paginate(page=page, per_page=10)
    return render_template("user.html", user=user, posts=posts, avatar=user.avatar(128))


@bp.route("/follow/<username>", methods=["POST"])
@login_required
def follow(username):
    user = db.session.scalar(select(User).where(User.username == username))
    current_user.follow(user)
    db.session.commit()
    return redirect(url_for("main.user", username=username))


@bp.route("/post/<int:pid>")
def post(pid):
    post = db.session.get(Post, request.args.get("id", pid))
    return post.to_dict()


@bp.route("/search")
def search():
    q = request.args.get("q", "")
    found = db.session.execute(select(User).where(User.username.like(f"%{q}%"))).scalars().all()
    exact = db.session.execute(text("SELECT id FROM user WHERE username = :name"), {"name": q}).all()
    return {"found": [u.username for u in found], "exact": len(exact)}


@bp.route("/search/raw")
def search_raw():
    q = request.args.get("q", "")
    rows = db.session.execute(text(f"SELECT id FROM user WHERE username = '{q}'")).all()  # TP: SQL injection
    return {"n": len(rows)}


@bp.route("/go")
def go():
    nxt = coerce_safe_redirect_target(request.args.get("next"))
    return redirect(nxt)


@bp.route("/page")
def page():
    n = request.args.get("n", 1, type=int)
    return redirect(f"/items?page={n}")


@bp.route("/jump")
def jump():
    return redirect(request.args.get("to"))  # TP: open redirect


@bp.route("/back")
def back():
    tab = request.args.get("tab", "home")
    return redirect(url_for("main.user", username=current_user.username, tab=tab))


@bp.route("/export/<int:eid>")
def export(eid):
    item = db.session.get(Export, eid)
    fmt = request.args.get("fmt", "pdf")
    buf = io.BytesIO(item.render())
    if fmt == "zip":
        return send_file(BytesIO(item.render() + fmt.encode()), download_name="x.zip")
    return send_file(buf, download_name=f"export.{fmt}")


@bp.route("/files/<name>")
def files(name):
    name = request.args.get("name", name)
    if not NAME.fullmatch(name):
        return "bad", 400
    return send_from_directory(current_app.config["UPLOADS"], name)


@bp.route("/raw-file")
def raw_file():
    path = os.path.join(current_app.config["UPLOADS"], request.args["f"])
    return send_file(path)  # TP: path from the request


@bp.route("/weather")
def weather():
    city = request.args.get("city", "")
    base = current_app.config["WEATHER_API"]
    r = requests.get(f"{base}/forecast", params={"q": city}, timeout=5)
    return r.json()


@bp.route("/fetch")
def fetch():
    url = request.args["url"]
    return requests.get(url, timeout=5).text  # TP: SSRF


@bp.route("/report", methods=["POST"])
def report():
    name = request.form["name"]
    out = os.path.join(current_app.instance_path, f"{uuid4()}.pdf")
    script = os.path.join(os.path.dirname(__file__), "render.py")
    subprocess.run([sys.executable, script, "--out", out], check=True)
    build_report.delay(name)
    return {"name": name, "out": os.path.basename(out)}


@bp.route("/archive")
def archive():
    path = request.args.get("path", "")
    name = request.args.get("name", "")
    subprocess.run(["tar", "czf", os.path.join(current_app.instance_path, "all.tgz"), "-C", current_app.root_path, "."])
    return {"path": path, "name": name}


@bp.route("/ping")
def ping():
    host = request.args.get("host", "")
    subprocess.run(f"ping -c1 {host}", shell=True)  # TP: shell injection
    return "ok"
