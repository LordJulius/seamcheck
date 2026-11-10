from app import db


class Division(db.Model):
    id = db.Column(db.Integer, primary_key=True)
    name = db.Column(db.String)
