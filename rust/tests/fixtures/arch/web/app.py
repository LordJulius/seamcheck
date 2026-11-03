import subprocess
from web.util import run_it, safe_name


def handler(request):
    cmd = request.args.get("c")
    full = "ls " + cmd
    run_it(full)
    subprocess.run("date")


def other(request):
    name = safe_name(request.args.get("n"))
    subprocess.run(name)
