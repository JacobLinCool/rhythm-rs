# taiko-resource-server

HTTP + WebSocket server for delivering taiko resources (`library`, `chart`, `audio`) and multiplayer rooms to `taiko-game`.

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
- `GET /v1/multiplayer/healthz`
- `WS /v1/multiplayer/ws`

## Client

```bash
cargo run -p taiko-game --release -- \
  --resource-endpoint http://127.0.0.1:4150/

# create room / join / spectate
cargo run -p taiko-game --release -- online create --server http://127.0.0.1:4150 --name host
cargo run -p taiko-game --release -- online join --server http://127.0.0.1:4150 --room <CODE> --name p2
cargo run -p taiko-game --release -- online spectate --server http://127.0.0.1:4150 --room <CODE> --name viewer
```
