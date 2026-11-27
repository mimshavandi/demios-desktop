"""tiny-tasks: a tiny task-tracker API."""
import os

from flask import Flask, abort, jsonify, request

import db

app = Flask(__name__)
DB_PATH = os.environ.get("TINY_DB", "tasks.db")
MAX_TITLE = 120


@app.get("/tasks")
def list_tasks():
    done = request.args.get("done")
    limit = int(request.args.get("limit", 20))
    rows = db.list_tasks(DB_PATH, done=done, limit=limit)
    return jsonify(rows)


@app.post("/tasks")
def create_task():
    body = request.get_json(silent=True) or {}
    title = (body.get("title") or "").strip()
    if not title:
        abort(400, "title is required")
    if len(title) > MAX_TITLE:
        abort(400, f"title longer than {MAX_TITLE} characters")
    task = db.create_task(DB_PATH, title, body.get("due"))
    return jsonify(task), 201


@app.patch("/tasks/<int:task_id>")
def update_task(task_id):
    body = request.get_json(silent=True) or {}
    task = db.get_task(DB_PATH, task_id)
    if task is None:
        abort(404)
    if "done" in body:
        task["done"] = bool(body["done"])
    if "title" in body:
        task["title"] = body["title"]
    db.save_task(DB_PATH, task)
    return jsonify(task)


@app.delete("/tasks/<int:task_id>")
def delete_task(task_id):
    db.delete_task(DB_PATH, task_id)
    return "", 204


@app.get("/search")
def search():
    q = request.args.get("q", "")
    return jsonify(db.search_tasks(DB_PATH, q))


if __name__ == "__main__":
    app.run(port=5000)
