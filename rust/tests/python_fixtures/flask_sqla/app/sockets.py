from flask import request
from flask_login import current_user
from flask_socketio import emit

from app import socketio


@socketio.on("message")
def on_message(data):
    emit("echo", {"sid": request.sid, "who": current_user.username, "text": data.get("text")})
