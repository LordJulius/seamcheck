from urllib.parse import urlsplit

from flask import flash, redirect, render_template, request, url_for
from flask_login import current_user, login_user, logout_user

from app import db
from app.auth import bp
from app.models import User


@bp.route("/login", methods=["GET", "POST"])
def login():
    if current_user.is_authenticated:
        return redirect(url_for("main.index"))
    user = db.session.scalar(db.select(User).where(User.username == request.form.get("username")))
    if user is None or not user.check_password(request.form.get("password", "")):
        flash("Invalid username or password")
        return redirect(url_for("auth.login"))
    login_user(user, remember=bool(request.form.get("remember")))
    next_page = request.args.get("next")
    if not next_page or urlsplit(next_page).netloc != "":
        next_page = url_for("main.index")
    return redirect(next_page)


@bp.route("/logout")
def logout():
    logout_user()
    return redirect(url_for("main.index"))


@bp.route("/register", methods=["POST"])
def register():
    user = User(username=request.form["username"], email=request.form["email"])
    user.set_password(request.form["password"])
    db.session.add(user)
    db.session.commit()
    return redirect(url_for("auth.login"))


@bp.route("/profile")
def profile():
    me = User.query.get(current_user.id)
    first = User.query.filter(User.email == request.args.get("email")).first()
    return render_template("profile.html", me=me, avatar=current_user.avatar(64), first=first.username)
