import subprocess
from flask import request


def handler():
    host = request.args.get("host")
    page = load("index")             # a constant: load's parameter is clean here
    run(host)
    if request.args.get("level") == "high":
        host = escape(host)          # sanitized in one branch only
    subprocess.run("ping " + host, shell=True)


def careful():
    name = request.args.get("name")
    name = escape(name)              # sanitized for the rest of the function
    subprocess.run("echo " + name, shell=True)


def load(name):
    return open("pages/" + name).read()


def run(h):
    subprocess.call("dig " + h, shell=True)


def escape(s):
    return s
