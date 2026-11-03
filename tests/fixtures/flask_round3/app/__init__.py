from flask import Flask


def create_app():
    app = Flask(__name__)
    from app.main import bp

    app.register_blueprint(bp)
    return app
