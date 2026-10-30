import aiohttp


async def create_user(body):
    async with aiohttp.request("post", "https://api.example.com/users", json=body) as response:
        return await response.json()
