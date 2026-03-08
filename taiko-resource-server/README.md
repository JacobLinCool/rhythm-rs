# taiko-resource-server

HTTP server for delivering taiko resources (`library`, `chart`, `audio`) to `taiko-game` remote mode.

You can run it either as:
- `taiko server ...` (subcommand on `taiko-game`)
- standalone binary `taiko-resource-server ...`

## Run

```bash
cargo run -p taiko-game --release -- server \
  --songdir ./taiko-game/songs --host 127.0.0.1 --port 4150

# or standalone
cargo run -p taiko-resource-server --release -- \
  --songdir ./taiko-game/songs --host 127.0.0.1 --port 4150
```

## Endpoints

- `GET /healthz`
- `GET /v1/library`
- `GET /v1/charts/{id}`
- `GET /v1/audio/{id}`

## Client

```bash
cargo run -p taiko-game --release -- \
  --resource-endpoint http://127.0.0.1:4150/
```
