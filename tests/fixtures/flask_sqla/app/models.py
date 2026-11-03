from datetime import datetime, timezone
from hashlib import md5

from flask_login import UserMixin
from werkzeug.security import check_password_hash, generate_password_hash

from app import db, login


class User(UserMixin, db.Model):
    id = db.Column(db.Integer, primary_key=True)
    username = db.Column(db.String(64), index=True, unique=True)
    email = db.Column(db.String(120), index=True, unique=True)
    password_hash = db.Column(db.String(256))
    posts = db.relationship("Post", backref="author", lazy="dynamic")

    def set_password(self, password):
        self.password_hash = generate_password_hash(password)

    def check_password(self, password):
        return check_password_hash(self.password_hash, password)

    def avatar(self, size):
        digest = md5(self.email.lower().encode("utf-8")).hexdigest()
        return f"https://www.gravatar.com/avatar/{digest}?d=identicon&s={size}"

    def follow(self, user):
        if not self.is_following(user):
            self.followed.append(user)

    def is_following(self, user):
        return user.id in [u.id for u in self.followed]

    def to_dict(self):
        return {"id": self.id, "username": self.username, "avatar": self.avatar(64)}

    def recent_posts(self):
        return self.posts.order_by(Post.timestamp.desc())


class Post(db.Model):
    id = db.Column(db.Integer, primary_key=True)
    body = db.Column(db.String(140))
    timestamp = db.Column(db.DateTime, index=True, default=lambda: datetime.now(timezone.utc))
    user_id = db.Column(db.Integer, db.ForeignKey(User.id), index=True)

    def summary(self):
        return self.body[:40]

    def to_dict(self):
        return {"id": self.id, "body": self.body, "summary": self.summary()}


class Export(db.Model):
    id = db.Column(db.Integer, primary_key=True)
    name = db.Column(db.String(64))

    def render(self):
        return self.name.encode()


@login.user_loader
def load_user(id):
    return db.session.get(User, int(id))
