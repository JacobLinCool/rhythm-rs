# taiko-game 玩家遊玩與測試指南

一般玩家只需要啟動一次遊戲；單人、同機雙人與連線遊玩都從遊戲畫面內選擇。

## 1. 啟動

在專案根目錄執行：

```bash
cargo run -p taiko-game --release -- --songdir ./taiko-game/songs
```

啟動後會看到三種模式：

1. `Single Player`：單人遊玩。
2. `Local Two Player`：兩人共用同一個終端機與鍵盤。
3. `Online Multiplayer`：本機主持、在既有 server 開房、加入或觀戰。

`Esc` 回上一層，`Ctrl+C` 隨時結束遊戲。

在模式選單按 `S` 可進入玩家設定。這裡可直接選擇 English／繁體中文／
日本語、調整音樂與鼓聲音量、單一輸入校正值、捲動速度、歌曲預覽、線上
名稱，以及兩位玩家各自的四個按鍵。語言會立即預覽；移到
`Save & Return` 按 `Enter` 才會以原子方式儲存，`Esc` 則捨棄本次修改。

## 2. 單人遊玩

1. 在模式選單選 `Single Player`。
2. 在歌曲選單用方向鍵選歌，按 `Enter`。
3. 在難度選單用上下鍵選難度，按 `Enter` 開始。
4. 預設按鍵為 `A=左 Kat`、`S=左 Don`、`D=右 Don`、`F=右 Kat`。
   `P` 暫停；`Esc` 會先詢問，再按一次才放棄這一局。

難度選單可用 `Tab`／`Shift+Tab` 切換設定，以左右鍵調整自動演奏、音樂
音量、鼓聲音量、單一輸入校正與捲動速度。校正單位為 5 ms，範圍
-500 至 +500 ms；它代表「實際按鍵時間對譜面時間」的單一關係，不再拆成
容易互相抵銷的音符偏移與音樂偏移。

歌曲選單可直接輸入文字搜尋，也支援 `oni=9`、`hard=4-7`、
`branch`、`bpm>=180` 等條件。畫面只呈現玩家需要的歌曲、難度、分歧與
音訊狀態；檔案路徑和 runtime 診斷不再佔據主要流程。

## 3. 同機雙人

1. 在模式選單選 `Local Two Player`。
2. 兩人共用歌曲選單選一首歌。
3. 兩位玩家可各自選不同難度：
   - P1：`W/S` 選難度，`F` 鎖定／取消鎖定。
   - P2：`↑/↓` 選難度，`J` 或 `Enter` 鎖定／取消鎖定。
4. 兩人都顯示 `READY` 後，遊戲會完成譜面／音訊準備再開始。
5. 遊玩按鍵互不重疊：
   - P1：`A=左 Kat`、`S=左 Don`、`D=右 Don`、`F=右 Kat`
   - P2：`J=左 Kat`、`K=左 Don`、`L=右 Don`、`;=右 Kat`
   - 共用：`P` 暫停／繼續；`Esc` 需再次確認才放棄並回難度選擇

遊玩畫面採上下排列：P1 在上、P2 在下。兩條音符軌都使用完整終端機寬度，
不會因雙人模式縮短橫向預讀距離。

每位玩家的四個按鍵分別代表鼓面的左／右 Kat 與左／右 Don；左右位置在
輸入層中保持獨立，送入譜面判定時才依音色歸類為 Don 或 Kat。

雙人難度頁可用 `Tab`／`Shift+Tab` 選擇共用的音量、偏移與捲動設定，
再用左右鍵調整；自動演奏不適用於雙人對戰。

同機雙人只播放一次音樂並共用同一個音樂時鐘，但兩位玩家有獨立譜面、
判定引擎、分數、分歧路線與 replay hash；其中一位的輸入不會進入另一位
玩家的 runtime。

## 4. 連線遊玩

在模式選單選 `Online Multiplayer`，再用左右鍵選擇：

- `Host here`：遊戲在本機 `127.0.0.1` 的隨機空閒 port 啟動權威 server，
  自動建立房間，適合在同一台電腦的不同終端機測試。離開房間或關閉遊戲
  時，這個 server 會一起正常關閉。
- `Create`：輸入既有 server URL 與暱稱，在該 server 建立房間。
- `Join`：貼上完整邀請並輸入暱稱，以玩家身分加入。
- `Spectate`：貼上完整邀請並輸入暱稱，只觀看房間。

用上下鍵／`Tab` 移動欄位，文字直接輸入，`Backspace` 刪除，`Enter`
確認。邀請格式類似：

```text
taiko://join?server=http%3A%2F%2F127.0.0.1%3A12345%2F&room=ABCD&token=<64-hex-token>
```

邀請中的 server、room、token 缺一不可；它是私密的入房能力憑證，不要
公開張貼。

### 同一台電腦、兩個終端機的測試

終端機 A：

1. 啟動遊戲。
2. 選 `Online Multiplayer` → `Host here`。
3. 輸入房主名稱並建立房間。
4. 複製大廳顯示的完整邀請。

終端機 B：

1. 再啟動一份遊戲。
2. 選 `Online Multiplayer` → `Join`。
3. 貼上邀請、輸入另一個名稱並連線。

房主選歌；每位玩家各自選難度並完成內容驗證與時鐘同步。全員準備後，
由房主開始。

### 獨立 server

只有獨立 server 需要 CLI 參數：

```bash
cargo run -p taiko-game --release -- server \
  --songdir ./taiko-game/songs \
  --host 0.0.0.0 \
  --port 4150
```

玩家仍然正常啟動遊戲，在 `Online Multiplayer` → `Create` 中輸入
`http://<server-address>:4150`；其他玩家用 `Join` 貼上房主分享的邀請。

正式跨網路部署時，server、反向代理、TLS、防火牆與 NAT 是營運邊界。
遊戲不會自行做 matchmaking、relay、UPnP 或 NAT 穿透。完整限制請看
[多人連線指南](../docs/multiplayer.md)。

本專案刻意不實作 relay、TLS 終止、NAT 穿透、matchmaking 或高可用；
私人連線模式只負責直接連到你指定的權威 server。

## 5. 無聲譜面與音訊裝置

`.tja` 的 `WAVE` 缺少或留空時，代表這是一張無聲譜面；它仍可用穩定的
遊戲時鐘觀看與遊玩。程式不會猜測同名音訊檔，以免實際播放內容與資源
hash 不一致。

如果作業系統沒有可用的音訊輸出，遊戲仍會進入模式選單並顯示一次說明。
無聲譜面可以正常遊玩；需要音樂的歌曲會得到明確、可返回的錯誤，而不會
讓整個 APP 在啟動前結束。

## 6. 結算、重試與錯誤復原

- 單人結算：`Enter` 立即重試、`Esc` 回歌曲、`D` 顯示／隱藏 replay 與
  效能詳情。
- 同機雙人結算：`Enter` 再戰、`Esc` 回歌曲、`D` 顯示／隱藏詳情。
- 線上結算：房主可再戰或回大廳；其他玩家會看到正在等待房主的狀態。
- 載入或準備失敗：錯誤頁會保留可返回的目的地；可重試的錯誤可直接按
  `Enter` 重試，`D` 才展開技術細節。
- 背景載入採「最新選擇優先」；較舊的完成結果不會覆蓋玩家後來選的歌曲
  或房間。

## 7. 遠端歌曲資源與快取

若一般遊戲也要從遠端 endpoint 讀取歌曲：

```bash
cargo run -p taiko-game --release -- \
  --resource-endpoint http://127.0.0.1:4150/
```

遠端內容預設使用 app-data 磁碟快取；維護指令：

```bash
cargo run -p taiko-game -- cache path
cargo run -p taiko-game -- cache list
cargo run -p taiko-game -- cache clear --endpoint http://127.0.0.1:4150/
cargo run -p taiko-game -- cache clear --all
```

若要在一次測試中停用磁碟快取，可在正常啟動時加
`--resource-cache-memory-only`。

## 8. 建議驗收清單

- 啟動：有／沒有音訊裝置都能進入模式選單；三種語言在 80×24 仍可讀。
- 單人：選歌、四鍵打擊、校正、暫停、離場確認、結算、Retry。
- 同機雙人：兩人選不同難度、同時打擊、各自分數變化、暫停、雙人結算。
- 本機連線：兩個終端機 Host/Join、邀請、選歌、不同難度、準備、開始、
  結算、正常離房。
- 獨立 server：Create/Join/Spectate，以及 server 關閉時客戶端的明確錯誤。
- 恢復：遊玩前與遊玩中短暫中斷網路，確認重連期間控制被鎖定，恢復後
  身分、指令序號與 match epoch 沒有重複。
- 內容：41 首本機歌曲全部載入且沒有警告；另以空 `WAVE` 的譜面確認無聲
  時鐘路徑。
- 硬體：在實際共用鍵盤同時按 P1 與 P2 的八個鍵，確認鍵盤本身沒有
  rollover／ghosting 限制。
