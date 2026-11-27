import pytest

import app as tiny


@pytest.fixture
def client(tmp_path, monkeypatch):
    monkeypatch.setattr(tiny, "DB_PATH", str(tmp_path / "test.db"))
    return tiny.app.test_client()


def test_create_and_list(client):
    r = client.post("/tasks", json={"title": "Buy compost"})
    assert r.status_code == 201
    assert client.get("/tasks").get_json()[0]["title"] == "Buy compost"


def test_title_required(client):
    assert client.post("/tasks", json={}).status_code == 400


def test_mark_done(client):
    task = client.post("/tasks", json={"title": "Water greenhouse"}).get_json()
    r = client.patch(f"/tasks/{task['id']}", json={"done": True})
    assert r.get_json()["done"] is True


def test_filter_not_done(client):
    a = client.post("/tasks", json={"title": "A"}).get_json()
    client.post("/tasks", json={"title": "B"})
    client.patch(f"/tasks/{a['id']}", json={"done": True})
    titles = [t["title"] for t in client.get("/tasks?done=false").get_json()]
    assert titles == ["B"]  # fails: returns ["A"]
