import httpx


def remove_user(user_id):
    return httpx.request("delete", "https://api.example.com/users/1")
