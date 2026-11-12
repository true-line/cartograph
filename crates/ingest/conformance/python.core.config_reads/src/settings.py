import os


def load(name):
    return os.getenv("DATABASE_URL"), os.environ[name]


def getenv(key):
    return key


def lookalike():
    return getenv("SHADOWED")
