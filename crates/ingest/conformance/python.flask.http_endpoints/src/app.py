from flask import Flask
app = Flask(__name__)
@app.route("/users/<id>", methods=["GET"])
def user(id):
    return {}
