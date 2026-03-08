# Taiko Remote Resource Server

## Goal

讓 `taiko-game` 可透過 HTTP 載入遠端資源，且使用流程與本地模式一致：

- Song Menu 讀取歌單與課程摘要
- 選課程後載入譜面
- Demo / 開局播放歌曲音訊

## Protocol Version

- API root: `<endpoint>/v1`
- Version field: `api_version`
- Current version: `1`

Client 會嚴格檢查 `api_version == 1`，不符合即拒絕連線。

## Endpoints

### 1) `GET /v1/library`

回傳整個歌單索引，包含 Song Menu 所需的所有摘要資訊與資源 ID。

```json
{
  "api_version": 1,
  "songs": [
    {
      "source_path": "pack1/demo.tja",
      "source_id": "<chart-id>",
      "audio_path": "pack1/demo.ogg",
      "audio_id": "<audio-id>",
      "title": "Demo Song",
      "subtitle": "",
      "artist": "Alice",
      "demo_start_seconds": 12.5,
      "courses": [
        {
          "index": 0,
          "name": "Oni",
          "level": 9,
          "object_count": 1234,
          "branch_segment_count": 1,
          "base_bpm": 180.0,
          "branch_decisions": [
            {
              "segment_id": 1,
              "decision_tick": 480000,
              "route_count": 3,
              "hint": "Accuracy"
            }
          ]
        }
      ]
    }
  ],
  "warnings": [
    "skip broken/song.tja: ..."
  ]
}
```

### 2) `GET /v1/charts/{id}`

回傳原始 `.tja` bytes。Client 會以既有 `TjaImporter` 解析，並在選課程時才 lazy-load。

### 3) `GET /v1/audio/{id}`

回傳音訊 bytes（`ogg/mp3/wav/flac/opus` 皆可，實際可解碼格式由 audio backend 決定）。

## Server Indexing Rules

`taiko server`（或獨立二進位 `taiko-resource-server`）啟動時會掃描 `--songdir`：

- 遞迴尋找 `.tja`
- 解析 metadata/courses，計算課程摘要
- 產生 `source_id` / `audio_id`（由 `kind + 相對路徑` 做 SHA-256）
- 收集不可用檔案為 `warnings`

## Client Behavior

當 `taiko-game` 設定 `--resource-endpoint`：

- 啟動時改抓 `GET /v1/library`
- 需要譜面時抓 `GET /v1/charts/{id}`
- 需要音訊時抓 `GET /v1/audio/{id}`
- 預設會使用 app data 磁碟快取 + 記憶體快取，且 chart/audio 都以內容 SHA-256 當 cache key
- 若要改成純記憶體快取，可加 `--resource-cache-memory-only`
- 可用 `taiko cache` 子命令查看或清理快取：
  - `taiko cache path`
  - `taiko cache list`
  - `taiko cache clear --endpoint <URL>`
  - `taiko cache clear --all`

若未設定 `--resource-endpoint`，仍使用原本本地資料夾模式。

## Example

```bash
# 1) 啟動 resource server
cargo run -p taiko-game --release -- server \
  --songdir ./taiko-game/songs --host 127.0.0.1 --port 4150

# 2) 啟動遊戲（remote mode）
cargo run -p taiko-game --release -- \
  --resource-endpoint http://127.0.0.1:4150/
```
