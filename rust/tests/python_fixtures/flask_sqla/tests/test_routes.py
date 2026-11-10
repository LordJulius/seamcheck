from app import create_app


def test_index():
    app = create_app()
    client = app.test_client()
    assert client.get("/").status_code == 200
