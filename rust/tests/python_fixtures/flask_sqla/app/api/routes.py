from flask import jsonify, request

from app import db
from app.api import bp
from app.models import Post, User


@bp.route("/users/<int:id>", methods=["GET"])
def get_user(id):
    return jsonify(db.get_or_404(User, id).to_dict())


@bp.route("/posts/<int:id>", methods=["GET"])
def get_post(id):
    post = Post.query.get_or_404(id)
    return jsonify(post.to_dict())


@bp.route("/posts", methods=["GET"])
def list_posts():
    page = request.args.get("page", 1, type=int)
    posts = Post.query.order_by(Post.timestamp.desc()).paginate(page=page, per_page=20)
    return jsonify([p.to_dict() for p in posts.items])


@bp.route("/users/<int:id>/posts", methods=["GET"])
def user_posts(id):
    user = db.session.get(User, id)
    items = [p.summary() for p in user.recent_posts()]
    return jsonify(items)


@bp.route("/users", methods=["POST"])
def create_user():
    data = request.get_json() or {}
    user = User(username=data["username"], email=data["email"])
    user.set_password(data["password"])
    db.session.add(user)
    db.session.commit()
    return jsonify(user.to_dict()), 201
