# tiny-tasks

A tiny task-tracker HTTP API in Python (Flask + SQLite). No accounts, no auth: it's meant to run on your own machine.

## Requirements
- Python 3.11+
- Flask 3

## Run
```sh
pip install -r requirements.txt
python app.py
```
The server listens on http://localhost:8080.

Set `TINY_DB` to choose the database file (default: `tasks.db` in the current directory).

## Endpoints
| Method | Path | Body / query | Notes |
|---|---|---|---|
| GET | /tasks | `?done=true|false`, `?limit=N` (default 20) | newest first |
| POST | /tasks | `{"title": "...", "due": "2026-05-01"}` | title required, max 120 characters; returns 201 |
| PATCH | /tasks/<id> | `{"title": "...", "done": true}` | 404 if the task doesn't exist |
| DELETE | /tasks/<id> | none | returns 204 |
| GET | /search | `?q=text` | matches titles |

## Tests
```sh
pytest
```

## Licence
MIT
