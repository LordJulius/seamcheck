import os
import subprocess

from app import celery, db
from app.models import User


@celery.task(bind=True)
def build_report(self, name):
    attempt = self.request.retries
    args = self.request.args
    out = os.path.join("/srv/reports", f"{self.request.id}.txt")
    subprocess.run(["/usr/bin/report", "--out", out, "--attempt", str(attempt), "--for", str(args[0])], check=True)
    with open(out, "w") as f:
        f.write(f"{name} {args}")
    return out


@celery.task
def recount():
    for user in User.query.all():
        user.recent_posts().count()
    db.session.commit()
