# taiko-game 玩家遊玩說明書

這份文件是給玩家的「怎麼開始玩」指南。

## 1. 啟動方式

在專案根目錄執行：

```bash
cargo run -p taiko-game --release -- --songdir ./taiko-game/songs
```

若要改用遠端資源（HTTP endpoint）：

```bash
cargo run -p taiko-game --release -- server \
  --songdir ./taiko-game/songs --host 127.0.0.1 --port 4150

cargo run -p taiko-game --release -- \
  --resource-endpoint http://127.0.0.1:4150/
```

如果你想看所有參數：

```bash
cargo run -p taiko-game -- --help
```

## 2. 遊戲流程

畫面流程：

1. Song Menu（選曲）
2. Course Menu（選難度/設定）
3. Game（遊玩）
4. Result（結算）
5. 回到 Song Menu

若發生可恢復錯誤，會進 Error 頁面，可返回 Song Menu。

## 3. 操作鍵位

## 共通操作

- `Ctrl + C`：離開遊戲
- `Esc`：返回上一層（在 Song Menu 會直接離開）
- `Enter`：確認

## 打擊鍵（遊戲中）

### Don 鍵組

- `Space f g h j c v b n m`

### Kat 鍵組

- 左側：`d s a t r e w q x z`
- 右側：`k l ; ' y u i o p , . /`

## 選單操作

- Song Menu：
- 打字即時搜尋（可輸入一般文字與 magic words）
- `Backspace` / `Delete`：編輯或清空搜尋字串
- `Esc`：先清空搜尋；若搜尋已空則離開遊戲
- `Arrow Up/Down`：移動選曲
- `Enter`：進入 Course Menu
- `Ctrl+W`：開啟 Load Warnings 詳細清單
- Load Warnings 頁：`Up/Down` 捲動、`Left/Right` 快速捲動、`Esc`/`Enter`/`Ctrl+W` 返回 Song Menu
- Course Menu：
- 上/下移動：方向鍵，或 Kat 鍵組
- `Tab / Shift+Tab`：切換設定焦點
- 左/右：調整目前焦點設定（Auto Play / Volume / Note Offset / Music Offset / Scroll）
- `Scroll Speed` 為循環選項，包含 `V-Sync (S)` 檔位；`S` 會依譜面自動計算，且固定在 `1.0 <= S <= 2.0`
- 確認：`Enter` 或 Don 鍵組

## 4. Song Menu 搜尋與 Filter（Magic Words）

搜尋是空白分詞，條件之間採 AND（都要符合）。

範例：

- `oni=9`
- `oni=8,9,10`
- `hard=4-7`
- `lvl=8-10`
- `branch`
- `nobranch`
- `bpm>=180`
- `bpm=120-180`
- `alice oni=9 bpm>=180`

已支援 magic words：

- 難度星等：
- `easy=...` / `normal=...` / `hard=...` / `oni=...` / `ura=...`
- 右側可用單值、逗號清單、區間：
- `oni=9`
- `oni=8,9,10`
- `hard=4-7`
- `oni=*`（該難度任意星等）
- 任意難度星等：
- `lvl=...` 或 `level=...`（例如 `lvl=8-10`）
- 分歧：
- `branch`（有分歧）
- `nobranch` / `no-branch`（無分歧）
- BPM：
- `bpm=180`
- `bpm=120-180`
- `bpm>=180` / `bpm<=160` / `bpm>150` / `bpm<200`

若 magic words 語法錯誤，Song Info 會顯示 `Filter error`，且結果清單會暫時為空（strict 模式，不做隱性 fallback）。

## 5. 音量與效能參數

- `--songvol 0..100`：歌曲音量
- `--sevol 0..100`：打擊音效音量
- `--tps N`：邏輯更新頻率（預設 240）
- `--demo true|false`：Song/Course 頁是否播放 demo
- `--resource-endpoint URL`：改由遠端 resource server 讀取歌單/譜面/音訊（設定後 client 端不再讀本地 `--songdir`）
- `--resource-cache-memory-only`：遠端模式改成只用記憶體快取（預設會使用 app data 磁碟快取）
- `cache` 子命令：檢查/清除遠端快取（`taiko cache path|list|clear --endpoint <URL>|--all`）

建議：

- 一般玩家：`--tps 240`
- 若機器較慢：可降到 `--tps 180`

## 6. 顏色模式（Color Policy）

預設會使用彩色主題。

若你想關閉顏色，設定 `NO_COLOR`：

```bash
NO_COLOR=1 cargo run -p taiko-game --release -- --songdir ./taiko-game/songs
```

規則是：

- `NO_COLOR` 不存在或為空字串：顯示彩色
- `NO_COLOR` 存在且非空：關閉彩色（保留粗體/反白）

## 7. 結果頁會看到什麼

Result 頁會顯示：

- Score / Max Combo / Gauge Bar / Pass-Fail
- Great / OK / Miss / Roll Hits
- Replay Hash
- Branch Controls 次數
- Timing Distribution（橫向 violin-like 圖，顯示判定早/晚 ms 分佈）
- 效能統計（tick/frame 平均與 p95）

## 8. 判定與打擊回饋顏色

打擊區視覺分成三層：

1. 底層：打擊區底色（只由判定驅動）
2. 中層：音符（小音符 `o`、大音符 `O`，含底色）
3. 上層：打擊標記（`|` / `◎` / `|`）

判定底色規則：

- `Great(良)`：黃
- `OK(可)`：白
- `Miss(不可)`：藍
- `RollHit`：不會觸發底色閃爍

輸入回饋規則：

- 只閃打擊標記（`|` / `◎` / `|`）
- Don 輸入閃紅、Kat 輸入閃青
- 閃爍時間固定約 `200ms`

## 9. 魂條（Gauge Bar）怎麼看

HUD 與 Result 都會顯示動態寬度魂條：

- 條內有 `PASS` 線（依該譜面難度/星數動態計算）與 `FULL` 線（100%）
- 旁邊顯示百分比與狀態（`FAIL / PASS / FULL`）
- 達到滿條時會切換為滿條色

你可以直接看條的位置與狀態字樣判斷是否過關，不用只看百分比。

## 10. Auto 連打速度

Auto 模式對連打（roll/hold）的預設輸入頻率是：

- `16 * (bpm / 120)` hits/s

例如：

- BPM 120 -> 16 hits/s
- BPM 150 -> 20 hits/s
- BPM 180 -> 24 hits/s

## 11. 常見問題

### Q1: 開不起來，說某些譜面解析失敗？

遊戲會跳過壞檔，能玩的曲子照常載入。你可先確認 `Song Menu` 右側的 `Load warnings` 數量，並按 `Ctrl+W` 開啟詳細清單。

### Q2: 覺得音樂和判定線對不起來？

可用 `--track-offset` 設定初始值，或在 Course Menu 即時微調：

- 太早判：增加 `Note Offset` 或 `Music Offset`
- 太晚判：減少 `Note Offset` 或 `Music Offset`

### Q3: 我只想看演出不想自己打？

在 Course Menu 把 `Auto Play` 切到 `ON`，即可自動打完整場。
