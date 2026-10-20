from fastapi import FastAPI
app = FastAPI()
@app.get("/users/{id}")
def user(id: str):
    return {}
