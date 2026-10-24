from .store import save


def show(x):
    db.execute(x)
    return save(x)
