import subprocess
import sys

from PyQt6.QtCore import QUrl
from PyQt6.QtNetwork import QNetworkAccessManager, QNetworkRequest
from PyQt6.QtWidgets import QApplication, QFileDialog, QMainWindow


class Viewer(QMainWindow):
    def __init__(self):
        super().__init__()
        self.net = QNetworkAccessManager(self)

    def open_file(self):
        path, _ = QFileDialog.getOpenFileName(self, "Open log")
        if path:
            with open(path) as f:
                self.show_text(f.read())
            subprocess.run(["xdg-open", path])

    def show_text(self, text):
        self.setWindowTitle(text[:40])

    def fetch(self, url):
        request = QNetworkRequest(QUrl(url))
        print(request.url().toString(), request.headers)
        self.net.get(request)

    def run_script(self):
        name, _ = QFileDialog.getOpenFileName(self, "Script")
        subprocess.run(f"python3 {name}", shell=True)  # desktop: the user's own pick


def main():
    app = QApplication(sys.argv)
    w = Viewer()
    if len(sys.argv) > 1:
        subprocess.run(["less", sys.argv[1]])
    w.show()
    sys.exit(app.exec())
