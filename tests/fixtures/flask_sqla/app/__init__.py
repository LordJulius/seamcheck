from flask import Flask
from flask_sqlalchemy import SQLAlchemy
from flask_login import LoginManager
from flask_socketio import SocketIO
from celery import Celery

db = SQLAlchemy()
login = LoginManager()
socketio = SocketIO()
celery = Celery(__name__)


def create_app(config_class=None):
    app = Flask(__name__)
    app.config.from_object(config_class)
    db.init_app(app)
    login.init_app(app)
    socketio.init_app(app)

    from app.main import bp as main_bp
    app.register_blueprint(main_bp)
    from app.auth import bp as auth_bp
    app.register_blueprint(auth_bp, url_prefix="/auth")
    from app.api import bp as api_bp
    app.register_blueprint(api_bp, url_prefix="/api")
    return app
