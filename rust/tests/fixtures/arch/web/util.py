import subprocess


def run_it(text):
    subprocess.run(text, shell=True)


def safe_name(n):
    return n
