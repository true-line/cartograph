import requests

USERS = "https://api.example.com/users"


def list_users():
    return requests.get(USERS, timeout=5)


def create_user(body):
    return requests.post("https://api.example.com/users", json=body)
