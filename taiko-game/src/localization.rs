use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::online_session::OnlinePhase;
use crate::preferences::{BindingSlot, UiLanguage};

macro_rules! define_ui_text {
    (
        $(
            $variant:ident => {
                en: $english:literal,
                zh_hant: $traditional_chinese:literal,
                ja: $japanese:literal
            }
        ),+ $(,)?
    ) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(crate) enum UiText {
            $($variant),+
        }

        impl UiText {
            const fn resolve(self, language: UiLanguage) -> &'static str {
                match language {
                    UiLanguage::English => match self {
                        $(Self::$variant => $english),+
                    },
                    UiLanguage::TraditionalChinese => match self {
                        $(Self::$variant => $traditional_chinese),+
                    },
                    UiLanguage::Japanese => match self {
                        $(Self::$variant => $japanese),+
                    },
                }
            }
        }
    };
}

define_ui_text! {
    Language => {
        en: "Language",
        zh_hant: "語言",
        ja: "言語"
    },
    On => {
        en: "ON",
        zh_hant: "開",
        ja: "オン"
    },
    Off => {
        en: "OFF",
        zh_hant: "關",
        ja: "オフ"
    },
    Enter => {
        en: "ENTER",
        zh_hant: "ENTER",
        ja: "ENTER"
    },
    Waiting => {
        en: "WAITING",
        zh_hant: "等待中",
        ja: "待機中"
    },
    Result => {
        en: "RESULT",
        zh_hant: "結果",
        ja: "リザルト"
    },
    Paused => {
        en: "Ⅱ  PAUSED",
        zh_hant: "Ⅱ  已暫停",
        ja: "Ⅱ  一時停止"
    },
    AutoPlay => {
        en: "AUTO PLAY",
        zh_hant: "自動演奏",
        ja: "オート演奏"
    },
    ManualPlay => {
        en: "MANUAL PLAY",
        zh_hant: "手動演奏",
        ja: "手動演奏"
    },
    Pass => {
        en: "PASS",
        zh_hant: "通過",
        ja: "クリア"
    },
    Fail => {
        en: "FAIL",
        zh_hant: "失敗",
        ja: "失敗"
    },
    Full => {
        en: "FULL",
        zh_hant: "全滿",
        ja: "満タン"
    },
    Great => {
        en: "GREAT",
        zh_hant: "良",
        ja: "良"
    },
    Ok => {
        en: "OK",
        zh_hant: "可",
        ja: "可"
    },
    Miss => {
        en: "MISS",
        zh_hant: "不可",
        ja: "不可"
    },
    Score => {
        en: "SCORE",
        zh_hant: "分數",
        ja: "スコア"
    },
    Combo => {
        en: "COMBO",
        zh_hant: "連擊",
        ja: "コンボ"
    },
    Soul => {
        en: "SOUL",
        zh_hant: "魂",
        ja: "魂"
    },
    TerminalTooSmall => {
        en: "Terminal too small",
        zh_hant: "終端機尺寸太小",
        ja: "ターミナルが小さすぎます"
    },
    ResizeTerminal => {
        en: "Resize the terminal to continue.",
        zh_hant: "請調整終端機尺寸以繼續。",
        ja: "続行するにはターミナルのサイズを変更してください。"
    },
    LeaveThisMatch => {
        en: "Leave this match?",
        zh_hant: "要離開這場遊戲嗎？",
        ja: "この対戦から退出しますか？"
    },
    Destination => {
        en: "Destination",
        zh_hant: "返回位置",
        ja: "移動先"
    },
    LeaveConfirmHint => {
        en: "Esc again: leave  •  Any other key: stay",
        zh_hant: "再按 Esc：離開  •  其他按鍵：留下",
        ja: "もう一度 Esc：退出  •  その他のキー：残る"
    },
    ConfirmLeave => {
        en: " Confirm Leave ",
        zh_hant: " 確認離開 ",
        ja: " 退出の確認 "
    },
    PlayModeSelection => {
        en: "play mode selection",
        zh_hant: "遊玩模式選擇",
        ja: "プレイモード選択"
    },
    SongSelection => {
        en: "song selection",
        zh_hant: "歌曲選擇",
        ja: "曲選択"
    },
    CourseSelection => {
        en: "course selection",
        zh_hant: "難度選擇",
        ja: "コース選択"
    },
    LocalCourseSelection => {
        en: "local course selection",
        zh_hant: "本機難度選擇",
        ja: "ローカルコース選択"
    },
    OnlineConnection => {
        en: "online connection",
        zh_hant: "線上連線",
        ja: "オンライン接続"
    },
    OnlineDisconnectDestination => {
        en: "play mode selection (disconnect)",
        zh_hant: "遊玩模式選擇（中斷連線）",
        ja: "プレイモード選択（切断）"
    },
    RetrySinglePreparation => {
        en: "retry single-player preparation",
        zh_hant: "重試單人遊戲準備",
        ja: "ひとりプレイの準備を再試行"
    },
    RetryLocalPreparation => {
        en: "retry local match preparation",
        zh_hant: "重試本機對戰準備",
        ja: "ローカル対戦の準備を再試行"
    },
    TopModeSelect => {
        en: "TAIKO // SELECT PLAY MODE",
        zh_hant: "太鼓 // 選擇遊玩模式",
        ja: "太鼓 // プレイモード選択"
    },
    TopModeSelectHelp => {
        en: "↑↓ SELECT  •  ENTER CONFIRM  •  C CONTROLLERS  •  S SETTINGS",
        zh_hant: "↑↓ 選擇  •  ENTER 確認  •  C 控制器  •  S 設定",
        ja: "↑↓ 選択  •  ENTER 決定  •  C コントローラー  •  S 設定"
    },
    TopControllers => {
        en: "TAIKO // CONTROLLERS",
        zh_hant: "太鼓 // 控制器",
        ja: "太鼓 // コントローラー"
    },
    TopControllersHelp => {
        en: "↑↓ FIELD  •  ENTER ACTION  •  C COPY  •  R RESET  •  ESC BACK",
        zh_hant: "↑↓ 欄位  •  ENTER 操作  •  C 複製  •  R 重設  •  ESC 返回",
        ja: "↑↓ 項目  •  ENTER 操作  •  C コピー  •  R リセット  •  ESC 戻る"
    },
    ControllerSetup => {
        en: "Controller Setup",
        zh_hant: "控制器設定",
        ja: "コントローラー設定"
    },
    ControllerSetupDescription => {
        en: "Keyboard, Mac trackpad contact, terminal click, and phone controllers share one four-pad input path.",
        zh_hant: "鍵盤、Mac 觸控板接觸、終端機點擊與手機控制器共用同一條四鼓面輸入路徑。",
        ja: "キーボード、Macトラックパッド接触、端末クリック、スマホは同じ4打面の入力経路を使います。"
    },
    TrustedLanOnly => {
        en: "Trusted LAN only",
        zh_hant: "僅限可信任區域網路",
        ja: "信頼できるLAN専用"
    },
    TrustedLanWarning => {
        en: "The phone link is unencrypted. Use it only on a network whose other users you trust.",
        zh_hant: "手機連線未加密；只能在你信任其他使用者的網路上啟用。",
        ja: "スマホ接続は暗号化されません。他の利用者を信頼できるネットワークだけで使ってください。"
    },
    ControllerBindAddress => {
        en: "LAN bind address",
        zh_hant: "區網綁定位址",
        ja: "LANバインドアドレス"
    },
    ControllerLanServer => {
        en: "Phone controller server",
        zh_hant: "手機控制器伺服器",
        ja: "スマホコントローラーサーバー"
    },
    ControllerMacTrackpad => {
        en: "Mac trackpad contact",
        zh_hant: "Mac 觸控板接觸",
        ja: "Macトラックパッド接触"
    },
    ControllerTerminalPointer => {
        en: "Terminal mouse click",
        zh_hant: "終端機滑鼠點擊",
        ja: "端末マウスクリック"
    },
    ControllerPlayerOne => {
        en: "P1 phone",
        zh_hant: "P1 手機",
        ja: "P1 スマホ"
    },
    ControllerPlayerTwo => {
        en: "P2 phone",
        zh_hant: "P2 手機",
        ja: "P2 スマホ"
    },
    ControllerBack => {
        en: "Back to play modes",
        zh_hant: "返回遊玩模式",
        ja: "プレイモードへ戻る"
    },
    ControllerRunning => {
        en: "RUNNING",
        zh_hant: "運作中",
        ja: "稼働中"
    },
    ControllerStopped => {
        en: "STOPPED",
        zh_hant: "已停止",
        ja: "停止中"
    },
    ControllerLocalPlayerOne => {
        en: "LOCAL P1",
        zh_hant: "本機 P1",
        ja: "ローカル P1"
    },
    ControllerLocalPlayerTwo => {
        en: "LOCAL P2",
        zh_hant: "本機 P2",
        ja: "ローカル P2"
    },
    ControllerPointerOff => {
        en: "OFF",
        zh_hant: "關",
        ja: "オフ"
    },
    ControllerUnavailable => {
        en: "UNAVAILABLE",
        zh_hant: "無法使用",
        ja: "利用不可"
    },
    ControllerMacTrackpadUnavailable => {
        en: "Native Mac trackpad contact is unavailable",
        zh_hant: "無法使用 Mac 觸控板原生接觸輸入",
        ja: "Macトラックパッドのネイティブ接触入力を利用できません"
    },
    ControllerMacTrackpadUnavailableDetail => {
        en: "This Mac or macOS version did not provide a compatible raw-contact device.",
        zh_hant: "這台 Mac 或目前的 macOS 版本未提供相容的原始接觸裝置。",
        ja: "このMacまたはmacOSでは互換性のある接触デバイスを利用できません。"
    },
    ControllerPaired => {
        en: "PAIRED",
        zh_hant: "已配對",
        ja: "ペア済み"
    },
    ControllerWaitingPair => {
        en: "READY TO PAIR",
        zh_hant: "等待配對",
        ja: "ペアリング待ち"
    },
    ControllerConnected => {
        en: "CONNECTED",
        zh_hant: "已連線",
        ja: "接続中"
    },
    ControllerDisconnected => {
        en: "DISCONNECTED",
        zh_hant: "未連線",
        ja: "未接続"
    },
    ControllerEndpoint => {
        en: "Phone page",
        zh_hant: "手機頁面",
        ja: "スマホページ"
    },
    ControllerPairingLink => {
        en: "Pairing link",
        zh_hant: "配對連結",
        ja: "ペアリングリンク"
    },
    ControllerPairingQr => {
        en: "Scan this one-time pairing QR",
        zh_hant: "掃描此一次性配對 QR Code",
        ja: "この一度限りのペアリングQRを読み取ってください"
    },
    ControllerPairingQrHelp => {
        en: "Phone on same LAN  •  Enter/Esc hide  •  C copy  •  R revoke",
        zh_hant: "手機須在同一區網  •  Enter／Esc 隱藏  •  C 複製  •  R 撤銷",
        ja: "同じLANのスマホ  •  Enter/Esc 非表示  •  C コピー  •  R 無効化"
    },
    ControllerPairingQrNeedsSpace => {
        en: "Enlarge the terminal to display the pairing QR.",
        zh_hant: "請放大終端機以顯示配對 QR Code。",
        ja: "ペアリングQRを表示するにはターミナルを大きくしてください。"
    },
    ControllerPairingQrFailed => {
        en: "The pairing QR could not be encoded.",
        zh_hant: "無法編碼配對 QR Code。",
        ja: "ペアリングQRを生成できませんでした。"
    },
    ControllerLinkMasked => {
        en: "[hidden — Enter to reveal]",
        zh_hant: "[已隱藏 — 按 Enter 顯示]",
        ja: "[非表示 — Enterで表示]"
    },
    ControllerNoUnusedInvite => {
        en: "Already paired. Press R to revoke it and create a new link.",
        zh_hant: "已經配對；按 R 撤銷並建立新的連結。",
        ja: "ペア済みです。Rで無効化して新しいリンクを作成します。"
    },
    ControllerHits => {
        en: "Accepted / rejected hits",
        zh_hant: "接受／拒絕敲擊",
        ja: "受付／拒否した入力"
    },
    ControllerNavigationHelp => {
        en: "Enter starts/stops the server, changes a local controller's P1/P2 assignment, or reveals a selected link.",
        zh_hant: "Enter 可啟停伺服器、切換本機控制器的 P1／P2 指派，或顯示選取的配對連結。",
        ja: "Enterでサーバーの起動・停止、ローカル入力のP1/P2割当、選択リンクの表示を行います。"
    },
    ControllerLocalSlotHelp => {
        en: "Single-player and online play use LOCAL P1. LOCAL P2 is used only by local two-player mode.",
        zh_hant: "單人與連線遊玩使用「本機 P1」；只有單機雙人模式會使用「本機 P2」。",
        ja: "1人プレイとオンラインは「ローカルP1」を使います。「ローカルP2」はローカル2人プレイ専用です。"
    },
    ControllerPairingHelp => {
        en: "On a P1/P2 row: C copies the one-time link; R revokes the controller and rotates the link.",
        zh_hant: "在 P1／P2 欄位：C 複製一次性連結；R 撤銷控制器並更換連結。",
        ja: "P1/P2項目では、Cで一度限りのリンクをコピーし、Rで接続を無効化してリンクを更新します。"
    },
    ControllerMacTrackpadHelp => {
        en: "Mac contact mode needs no click or pressure: touch one of four left-to-right trackpad zones. Holding or sliding does not repeat.",
        zh_hant: "Mac 接觸模式不需點擊或加壓：直接碰觸觸控板由左至右的四個分區；持續接觸或滑動不會重複敲擊。",
        ja: "Mac接触モードはクリックも加圧も不要です。トラックパッドを左から4分割した領域に触れます。長押しや移動では連打しません。"
    },
    ControllerPointerHelp => {
        en: "Terminal mouse mode requires left-clicking one of the four on-screen pads.",
        zh_hant: "終端機滑鼠模式需要左鍵點擊畫面上的四個鼓面之一。",
        ja: "端末マウスモードでは画面上の4打面を左クリックします。"
    },
    ControllerKeyboardRepeatLimited => {
        en: "This terminal cannot distinguish held-key repeat. Use Mac trackpad contact or a phone controller for repeat-safe play.",
        zh_hant: "此終端機無法分辨按住按鍵的重複事件；若要避免誤觸，請改用 Mac 觸控板接觸或手機控制器。",
        ja: "この端末ではキー長押しのリピートを判別できません。Macトラックパッド接触かスマホを使ってください。"
    },
    ControllerKeyboardRepeatGameplay => {
        en: "KEY HOLD-REPEAT IS UNSAFE HERE — USE MAC TRACKPAD CONTACT OR PHONE",
        zh_hant: "此終端機無法安全辨識按住重複 — 請用 MAC 觸控板接觸或手機",
        ja: "キー長押しの判別不可 — MACトラックパッド接触かスマホを使用"
    },
    ControllerTestHelp => {
        en: "Live test: touch a Mac trackpad zone, tap a phone pad, or enable terminal mouse click and click a pad below.",
        zh_hant: "即時測試：碰觸 Mac 觸控板分區、敲手機鼓面，或啟用終端機滑鼠點擊後按下方鼓面。",
        ja: "動作確認：Macトラックパッド領域に触れるか、スマホを叩くか、端末マウスを有効にして下の打面をクリックします。"
    },
    ControllerPhoneHelp => {
        en: "Open one P1/P2 link on a phone on the same LAN. Multi-touch uses Pointer Events.",
        zh_hant: "在同一區網的手機開啟 P1／P2 連結；多點觸控使用 Pointer Events。",
        ja: "同じLANのスマホでP1/P2リンクを開きます。マルチタッチはPointer Eventsを使います。"
    },
    ControllerStopBeforeEditing => {
        en: "Stop the phone controller server before editing its bind address.",
        zh_hant: "請先停止手機控制器伺服器，再編輯綁定位址。",
        ja: "バインドアドレスを編集する前にサーバーを停止してください。"
    },
    ControllerBindMustBeExact => {
        en: "Choose one exact local or LAN address; wildcard and multicast addresses are not allowed.",
        zh_hant: "請指定一個確切的本機或區網位址；不允許萬用或多播位址。",
        ja: "ローカルまたはLANのアドレスを1つ指定してください。ワイルドカードとマルチキャストは使えません。"
    },
    ControllerLoopbackOnly => {
        en: "127.0.0.1 is local-only. A phone cannot connect; enter this computer's Wi-Fi or Ethernet IPv4 address.",
        zh_hant: "127.0.0.1 只能在本機使用，手機無法連線；請輸入這台電腦的 Wi-Fi 或乙太網路 IPv4 位址。",
        ja: "127.0.0.1 はこの端末専用です。スマホからは接続できないため、Wi-Fi または Ethernet の IPv4 アドレスを入力してください。"
    },
    ControllerInvalidBindAddress => {
        en: "Invalid bind address",
        zh_hant: "無效的綁定位址",
        ja: "無効なバインドアドレス"
    },
    ControllerGenerationExhausted => {
        en: "Controller server generation counter is exhausted; restart the application.",
        zh_hant: "控制器伺服器世代計數已耗盡；請重新啟動程式。",
        ja: "コントローラーサーバーの世代番号を使い切りました。アプリを再起動してください。"
    },
    ControllerServerStarted => {
        en: "Phone controller server started.",
        zh_hant: "手機控制器伺服器已啟動。",
        ja: "スマホコントローラーサーバーを起動しました。"
    },
    ControllerServerStopped => {
        en: "Phone controller server stopped and all sessions were revoked.",
        zh_hant: "手機控制器伺服器已停止，所有工作階段均已撤銷。",
        ja: "スマホコントローラーサーバーを停止し、全セッションを無効化しました。"
    },
    ControllerServerStartFailed => {
        en: "Could not start the phone controller server",
        zh_hant: "無法啟動手機控制器伺服器",
        ja: "スマホコントローラーサーバーを起動できません"
    },
    ControllerServerStopFailed => {
        en: "Controller server cleanup failed",
        zh_hant: "控制器伺服器清理失敗",
        ja: "コントローラーサーバーの終了処理に失敗しました"
    },
    ControllerStartServerFirst => {
        en: "Start the phone controller server first.",
        zh_hant: "請先啟動手機控制器伺服器。",
        ja: "先にスマホコントローラーサーバーを起動してください。"
    },
    ControllerInviteUnavailable => {
        en: "No unused pairing link is available; press R to rotate it.",
        zh_hant: "沒有可用的一次性配對連結；請按 R 更換。",
        ja: "未使用のペアリングリンクがありません。Rで更新してください。"
    },
    ControllerInviteCopied => {
        en: "One-time pairing link copied.",
        zh_hant: "已複製一次性配對連結。",
        ja: "一度限りのペアリングリンクをコピーしました。"
    },
    ControllerInviteCopyFailed => {
        en: "Could not copy the pairing link",
        zh_hant: "無法複製配對連結",
        ja: "ペアリングリンクをコピーできません"
    },
    ControllerPairingRotated => {
        en: "Controller revoked and a new one-time pairing link was created.",
        zh_hant: "已撤銷控制器，並建立新的單次配對連結。",
        ja: "コントローラーを無効化し、新しい一度限りのリンクを作成しました。"
    },
    ControllerPairingRotateFailed => {
        en: "Could not rotate the pairing link",
        zh_hant: "無法更換配對連結",
        ja: "ペアリングリンクを更新できません"
    },
    TopSettings => {
        en: "TAIKO // PLAYER SETTINGS",
        zh_hant: "太鼓 // 玩家設定",
        ja: "太鼓 // プレイヤー設定"
    },
    TopSettingsHelp => {
        en: "↑↓ FIELD  •  ←→ ADJUST  •  ESC DISCARD",
        zh_hant: "↑↓ 欄位  •  ←→ 調整  •  ESC 放棄",
        ja: "↑↓ 項目  •  ←→ 変更  •  ESC 破棄"
    },
    TopLoadWarnings => {
        en: "TAIKO // LOAD WARNINGS",
        zh_hant: "太鼓 // 載入警告",
        ja: "太鼓 // 読み込み警告"
    },
    EscBack => {
        en: "ESC BACK",
        zh_hant: "ESC 返回",
        ja: "ESC 戻る"
    },
    SelectCourse => {
        en: "SELECT COURSE",
        zh_hant: "選擇難度",
        ja: "コース選択"
    },
    TopPreparingMatch => {
        en: "TAIKO // PREPARING MATCH",
        zh_hant: "太鼓 // 準備遊戲",
        ja: "太鼓 // 対戦準備"
    },
    PreparingMatchHelp => {
        en: "VERIFYING CHART & AUDIO  •  ESC CANCEL",
        zh_hant: "驗證譜面與音訊  •  ESC 取消",
        ja: "譜面と音声を検証中  •  ESC キャンセル"
    },
    TopPlay => {
        en: "TAIKO // PLAY",
        zh_hant: "太鼓 // 演奏",
        ja: "太鼓 // 演奏"
    },
    GameHelp => {
        en: "P PAUSE  •  ESC LEAVE",
        zh_hant: "P 暫停  •  ESC 離開",
        ja: "P 一時停止  •  ESC 退出"
    },
    TopResult => {
        en: "TAIKO // RESULT",
        zh_hant: "太鼓 // 結果",
        ja: "太鼓 // リザルト"
    },
    ResultHelp => {
        en: "ENTER RETRY  •  ESC SONGS  •  D DETAILS",
        zh_hant: "ENTER 重試  •  ESC 歌曲  •  D 詳情",
        ja: "ENTER リトライ  •  ESC 曲選択  •  D 詳細"
    },
    TopLocalCourses => {
        en: "TAIKO // LOCAL VERSUS // COURSES",
        zh_hant: "太鼓 // 本機對戰 // 難度",
        ja: "太鼓 // ローカル対戦 // コース"
    },
    TopLocalVersus => {
        en: "TAIKO // LOCAL VERSUS",
        zh_hant: "太鼓 // 本機對戰",
        ja: "太鼓 // ローカル対戦"
    },
    TopLocalResult => {
        en: "TAIKO // LOCAL VERSUS // RESULT",
        zh_hant: "太鼓 // 本機對戰 // 結果",
        ja: "太鼓 // ローカル対戦 // リザルト"
    },
    LocalResultHelp => {
        en: "ENTER REMATCH  •  ESC SONGS  •  D DETAILS",
        zh_hant: "ENTER 再戰  •  ESC 歌曲  •  D 詳情",
        ja: "ENTER 再戦  •  ESC 曲選択  •  D 詳細"
    },
    TopError => {
        en: "TAIKO // ERROR",
        zh_hant: "太鼓 // 錯誤",
        ja: "太鼓 // エラー"
    },
    ErrorDefaultHelp => {
        en: "ENTER / ESC: PLAY MODE  •  D DETAILS",
        zh_hant: "ENTER / ESC：遊玩模式  •  D 詳情",
        ja: "ENTER / ESC：モード選択  •  D 詳細"
    },
    TopOnline => {
        en: "TAIKO // ONLINE",
        zh_hant: "太鼓 // 線上遊玩",
        ja: "太鼓 // オンライン"
    },
    OnlineModes => {
        en: "HOST  •  CREATE  •  JOIN  •  SPECTATE",
        zh_hant: "本機主持  •  建立  •  加入  •  觀戰",
        ja: "ホスト  •  作成  •  参加  •  観戦"
    },
    TopOnlineLobby => {
        en: "TAIKO // ONLINE LOBBY",
        zh_hant: "太鼓 // 線上大廳",
        ja: "太鼓 // オンラインロビー"
    },
    TopOnlineCourse => {
        en: "TAIKO // ONLINE // COURSE SELECT",
        zh_hant: "太鼓 // 線上遊玩 // 選擇難度",
        ja: "太鼓 // オンライン // コース選択"
    },
    TopOnlineMatch => {
        en: "TAIKO // ONLINE MATCH",
        zh_hant: "太鼓 // 線上遊戲",
        ja: "太鼓 // オンライン対戦"
    },
    TopOnlineResult => {
        en: "TAIKO // ONLINE // RESULT",
        zh_hant: "太鼓 // 線上遊玩 // 結果",
        ja: "太鼓 // オンライン // リザルト"
    },
    ModeSingle => {
        en: "Single Player",
        zh_hant: "單人遊玩",
        ja: "ひとりで遊ぶ"
    },
    ModeLocal => {
        en: "Local Two Player",
        zh_hant: "本機雙人",
        ja: "ローカル2人対戦"
    },
    ModeOnline => {
        en: "Online Multiplayer",
        zh_hant: "線上多人",
        ja: "オンライン対戦"
    },
    ModeSingleDescription => {
        en: "Play one chart locally with full course settings and result analysis.",
        zh_hant: "在本機遊玩一份譜面，使用完整難度設定與結果分析。",
        ja: "ローカルで1つの譜面を、詳細設定とリザルト分析付きで遊びます。"
    },
    ModeLocalDescription => {
        en: "Two players share this terminal and song while keeping independent courses and scores.",
        zh_hant: "兩位玩家共用此終端機與歌曲，各自選擇難度並獨立計分。",
        ja: "2人で端末と曲を共有し、別々のコースとスコアで対戦します。"
    },
    ModeOnlineDescription => {
        en: "Host here, create or join a room, or spectate an authoritative online match.",
        zh_hant: "在本機主持、建立或加入房間，也可觀戰權威式線上對戰。",
        ja: "この端末でホストするか、ルームの作成・参加・観戦を行います。"
    },
    ModeSingleControls => {
        en: "Choose songs and courses with the arrow keys and Enter.",
        zh_hant: "使用方向鍵與 Enter 選擇歌曲和難度。",
        ja: "矢印キーと Enter で曲とコースを選びます。"
    },
    ModeLocalControls => {
        en: "Each player uses the four drum keys configured in Settings.",
        zh_hant: "每位玩家使用「設定」中指定的四個太鼓按鍵。",
        ja: "各プレイヤーは「設定」で指定した4つの太鼓キーを使います。"
    },
    ModeOnlineControls => {
        en: "All connection choices are configured inside the game.",
        zh_hant: "所有連線選項都可在遊戲內設定。",
        ja: "接続方法はすべてゲーム内で設定できます。"
    },
    ChoosePlayMode => {
        en: "Choose Play Mode",
        zh_hant: "選擇遊玩模式",
        ja: "プレイモードを選択"
    },
    ModeSelectControls => {
        en: "Up/Down: select  •  Enter: confirm  •  C: controllers  •  S: settings  •  Esc: quit",
        zh_hant: "上/下：選擇  •  Enter：確認  •  C：控制器  •  S：設定  •  Esc：離開",
        ja: "上/下：選択  •  Enter：決定  •  C：コントローラー  •  S：設定  •  Esc：終了"
    },
    OfflineLibrary => {
        en: "Offline library",
        zh_hant: "離線曲庫",
        ja: "オフライン曲ライブラリ"
    },
    HowItWorks => {
        en: "How It Works",
        zh_hant: "操作說明",
        ja: "遊び方"
    },
    PlayerPreferences => {
        en: "Player Preferences",
        zh_hant: "玩家偏好設定",
        ja: "プレイヤー設定"
    },
    PersistentSettings => {
        en: "Persistent settings",
        zh_hant: "持久化設定",
        ja: "保存される設定"
    },
    SettingsAdjustHelp => {
        en: "Up/Down chooses a field. Left/Right adjusts values.",
        zh_hant: "上/下選擇欄位；左/右調整數值。",
        ja: "上/下で項目を選び、左/右で値を変更します。"
    },
    SettingsBindingHelp => {
        en: "Select a binding and press Enter, then press its new key.",
        zh_hant: "選擇按鍵綁定並按 Enter，再按下新的按鍵。",
        ja: "キー設定を選んで Enter を押し、新しいキーを入力します。"
    },
    SettingsTestHelp => {
        en: "Outside capture, press any bound key to test it and its drum sound.",
        zh_hant: "非擷取狀態下，按任一已綁定按鍵即可測試輸入與鼓聲。",
        ja: "キー入力待ち以外では、設定済みキーで入力と音を確認できます。"
    },
    SettingsSaveHelp => {
        en: "Save & Return writes one validated, atomic preferences file.",
        zh_hant: "「儲存並返回」會原子寫入一份通過驗證的偏好設定檔。",
        ja: "「保存して戻る」は検証済み設定をアトミックに保存します。"
    },
    SettingsDiscardHelp => {
        en: "Esc discards every unsaved change.",
        zh_hant: "Esc 會捨棄所有未儲存的變更。",
        ja: "Esc で未保存の変更をすべて破棄します。"
    },
    ControlsAndKeyTest => {
        en: "Controls & Key Test",
        zh_hant: "操作與按鍵測試",
        ja: "操作とキーテスト"
    },
    MusicVolume => {
        en: "Music Volume",
        zh_hant: "音樂音量",
        ja: "楽曲音量"
    },
    DrumVolume => {
        en: "Drum Volume",
        zh_hant: "鼓聲音量",
        ja: "太鼓音量"
    },
    Calibration => {
        en: "Calibration",
        zh_hant: "延遲校正",
        ja: "タイミング調整"
    },
    ScrollSpeed => {
        en: "Scroll Speed",
        zh_hant: "捲動速度",
        ja: "スクロール速度"
    },
    VelocitySync => {
        en: "Velocity Sync",
        zh_hant: "速度同步",
        ja: "速度同期"
    },
    SongPreview => {
        en: "Song Preview",
        zh_hant: "歌曲預覽",
        ja: "曲プレビュー"
    },
    OnlineName => {
        en: "Online Name",
        zh_hant: "線上名稱",
        ja: "オンライン名"
    },
    SaveAndReturn => {
        en: "Save & Return",
        zh_hant: "儲存並返回",
        ja: "保存して戻る"
    },
    BindingLeftKat => {
        en: "Left Kat",
        zh_hant: "左緣",
        ja: "左カッ"
    },
    BindingLeftDon => {
        en: "Left Don",
        zh_hant: "左咚",
        ja: "左ドン"
    },
    BindingRightDon => {
        en: "Right Don",
        zh_hant: "右咚",
        ja: "右ドン"
    },
    BindingRightKat => {
        en: "Right Kat",
        zh_hant: "右緣",
        ja: "右カッ"
    },
    Offline => {
        en: "OFFLINE",
        zh_hant: "離線",
        ja: "オフライン"
    },
    SongSelect => {
        en: "SONG SELECT",
        zh_hant: "選擇歌曲",
        ja: "曲選択"
    },
    Room => {
        en: "ROOM",
        zh_hant: "房間",
        ja: "ルーム"
    },
    None => {
        en: "<none>",
        zh_hant: "<無>",
        ja: "<なし>"
    },
    UnknownSong => {
        en: "<unknown song>",
        zh_hant: "<未知歌曲>",
        ja: "<不明な曲>"
    },
    AudioUnavailable => {
        en: "Audio unavailable — silent charts only",
        zh_hant: "音訊不可用 — 僅能遊玩無聲譜面",
        ja: "音声を利用できません — 無音譜面のみプレイ可能"
    },
    SoundEffectsDisabled => {
        en: "Drum sound effects unavailable",
        zh_hant: "鼓聲效果不可用",
        ja: "太鼓の効果音を利用できません"
    },
    KeyCaptureCancelled => {
        en: "Key capture cancelled",
        zh_hant: "已取消按鍵擷取",
        ja: "キー入力をキャンセルしました"
    },
    VisibleKeyOrCancel => {
        en: "Press one visible character, or Esc to cancel",
        zh_hant: "請按一個可見字元，或按 Esc 取消",
        ja: "表示可能な文字キーを1つ押すか、Esc でキャンセルしてください"
    },
    BindingVisibleAsciiRequired => {
        en: "Drum bindings must use one visible ASCII character.",
        zh_hant: "太鼓按鍵必須是一個可見的 ASCII 字元。",
        ja: "太鼓キーには表示可能な ASCII 文字を1つ指定してください。"
    },
    PauseKeyReserved => {
        en: "P is reserved for pause.",
        zh_hant: "P 保留作為暫停鍵。",
        ja: "P は一時停止用に予約されています。"
    },
    ErrorTitle => {
        en: "Error",
        zh_hant: "錯誤",
        ja: "エラー"
    },
    TaikoCouldNotContinue => {
        en: "Taiko could not continue this action.",
        zh_hant: "太鼓無法繼續執行這項操作。",
        ja: "この操作を続行できませんでした。"
    },
    RecoverableErrorOccurred => {
        en: "A recoverable error occurred.",
        zh_hant: "發生可復原的錯誤。",
        ja: "復旧可能なエラーが発生しました。"
    },
    PressEnter => {
        en: "Press Enter ",
        zh_hant: "按 Enter ",
        ja: "Enter を押して"
    },
    PressEnterOrEsc => {
        en: "Press Enter/Esc ",
        zh_hant: "按 Enter/Esc ",
        ja: "Enter/Esc を押して"
    },
    PressEsc => {
        en: "Press Esc ",
        zh_hant: "按 Esc ",
        ja: "Esc を押して"
    },
    PressD => {
        en: "Press D ",
        zh_hant: "按 D ",
        ja: "D を押して"
    },
    ToggleTechnicalDetails => {
        en: "to show or hide technical details.",
        zh_hant: "顯示或隱藏技術詳情。",
        ja: "技術情報の表示／非表示を切り替えます。"
    },
    PressCtrlC => {
        en: "Press Ctrl+C ",
        zh_hant: "按 Ctrl+C ",
        ja: "Ctrl+C を押して"
    },
    QuitSentence => {
        en: "to quit.",
        zh_hant: "離開遊戲。",
        ja: "終了します。"
    },
    NoMatchingSongs => {
        en: "(no matching songs)",
        zh_hant: "（沒有符合的歌曲）",
        ja: "（一致する曲がありません）"
    },
    SongMenuTitle => {
        en: "Song Menu (Type to filter, arrows to move, Enter to select)",
        zh_hant: "歌曲選單（輸入文字篩選、方向鍵移動、Enter 選擇）",
        ja: "曲メニュー（文字で絞り込み、矢印で移動、Enter で選択）"
    },
    Empty => {
        en: "<empty>",
        zh_hant: "<空白>",
        ja: "<空>"
    },
    Search => {
        en: "Search",
        zh_hant: "搜尋",
        ja: "検索"
    },
    Matches => {
        en: "Matches",
        zh_hant: "符合數",
        ja: "一致"
    },
    Preview => {
        en: "Preview",
        zh_hant: "預覽",
        ja: "プレビュー"
    },
    OnlineStillAvailable => {
        en: "Online play remains available from the play-mode menu.",
        zh_hant: "仍可從遊玩模式選單進入線上遊玩。",
        ja: "プレイモードメニューからオンラインプレイを利用できます。"
    },
    FilterError => {
        en: "Filter error",
        zh_hant: "篩選錯誤",
        ja: "絞り込みエラー"
    },
    Title => {
        en: "Title",
        zh_hant: "曲名",
        ja: "曲名"
    },
    Subtitle => {
        en: "Subtitle",
        zh_hant: "副標題",
        ja: "サブタイトル"
    },
    Artist => {
        en: "Artist",
        zh_hant: "演出者",
        ja: "アーティスト"
    },
    Courses => {
        en: "Courses",
        zh_hant: "難度數",
        ja: "コース数"
    },
    Branching => {
        en: "Branching",
        zh_hant: "譜面分歧",
        ja: "譜面分岐"
    },
    Yes => {
        en: "Yes",
        zh_hant: "有",
        ja: "あり"
    },
    No => {
        en: "No",
        zh_hant: "無",
        ja: "なし"
    },
    Audio => {
        en: "Audio",
        zh_hant: "音訊",
        ja: "音声"
    },
    Available => {
        en: "Available",
        zh_hant: "可用",
        ja: "あり"
    },
    Silent => {
        en: "Silent",
        zh_hant: "無聲",
        ja: "無音"
    },
    LoadWarnings => {
        en: "Load warnings",
        zh_hant: "載入警告",
        ja: "読み込み警告"
    },
    Keys => {
        en: "Keys",
        zh_hant: "按鍵",
        ja: "キー"
    },
    SongFilterHelp => {
        en: "Filter: type text, Backspace/Delete edit, Esc clear",
        zh_hant: "篩選：輸入文字；Backspace/Delete 編輯；Esc 清除",
        ja: "絞り込み：文字入力、Backspace/Delete で編集、Esc でクリア"
    },
    ConfirmEnterHelp => {
        en: "Confirm: Enter",
        zh_hant: "確認：Enter",
        ja: "決定：Enter"
    },
    NavigateArrowsHelp => {
        en: "Navigate: arrow keys",
        zh_hant: "移動：方向鍵",
        ja: "移動：矢印キー"
    },
    LoadWarningsHelp => {
        en: "Load warnings: Ctrl+W",
        zh_hant: "載入警告：Ctrl+W",
        ja: "読み込み警告：Ctrl+W"
    },
    BackToModesHelp => {
        en: "Back to play modes when filter is empty: Esc",
        zh_hant: "篩選為空時返回遊玩模式：Esc",
        ja: "絞り込みが空ならモード選択へ戻る：Esc"
    },
    QuitHelp => {
        en: "Quit: Ctrl+C",
        zh_hant: "離開：Ctrl+C",
        ja: "終了：Ctrl+C"
    },
    FilterExamples => {
        en: "Filter examples",
        zh_hant: "篩選範例",
        ja: "絞り込み例"
    },
    NoPlayableSong => {
        en: "No playable offline song is selected.",
        zh_hant: "目前未選擇可遊玩的離線歌曲。",
        ja: "プレイ可能なオフライン曲が選択されていません。"
    },
    SongInfo => {
        en: "Song Info",
        zh_hant: "歌曲資訊",
        ja: "曲情報"
    },
    NoSelectedSong => {
        en: "No selected song",
        zh_hant: "未選擇歌曲",
        ja: "曲が選択されていません"
    },
    CourseMenu => {
        en: "Course Menu",
        zh_hant: "難度選單",
        ja: "コースメニュー"
    },
    CourseMenuTitle => {
        en: "Course Menu (Enter/Don to start, Esc to go back)",
        zh_hant: "難度選單（Enter／咚開始，Esc 返回）",
        ja: "コースメニュー（Enter／ドンで開始、Esc で戻る）"
    },
    Song => {
        en: "Song",
        zh_hant: "歌曲",
        ja: "曲"
    },
    CourseSettingsHelp => {
        en: "Settings (Tab/Shift+Tab focuses, Left/Right adjusts)",
        zh_hant: "設定（Tab/Shift+Tab 切換焦點，左/右調整）",
        ja: "設定（Tab/Shift+Tab で選択、左/右で変更）"
    },
    AutoPlaySetting => {
        en: "Auto Play",
        zh_hant: "自動演奏",
        ja: "オート演奏"
    },
    SeVolume => {
        en: "SE Volume",
        zh_hant: "效果音量",
        ja: "効果音量"
    },
    Tip => {
        en: "Tip",
        zh_hant: "提示",
        ja: "ヒント"
    },
    CourseSettingsTip => {
        en: "Tab focuses a setting; Left/Right adjusts it; Up/Down selects a course.",
        zh_hant: "Tab 切換設定；左/右調整；上/下選擇難度。",
        ja: "Tab で設定を選択、左/右で変更、上/下でコースを選びます。"
    },
    SelectedCourse => {
        en: "Selected course",
        zh_hant: "目前難度",
        ja: "選択中のコース"
    },
    Stars => {
        en: "Stars",
        zh_hant: "星等",
        ja: "星"
    },
    NoteObjectCount => {
        en: "Notes",
        zh_hant: "譜面音符數",
        ja: "音符数"
    },
    CourseInfo => {
        en: "Course Info",
        zh_hant: "難度資訊",
        ja: "コース情報"
    },
    GameSessionMissing => {
        en: "Game session missing",
        zh_hant: "找不到遊戲階段",
        ja: "ゲームセッションがありません"
    },
    Game => {
        en: "Game",
        zh_hant: "遊戲",
        ja: "ゲーム"
    },
    BestCombo => {
        en: "BEST",
        zh_hant: "最高連擊",
        ja: "最大コンボ"
    },
    LiveScore => {
        en: " LIVE SCORE ",
        zh_hant: " 即時分數 ",
        ja: " ライブスコア "
    },
    Progress => {
        en: "PROGRESS",
        zh_hant: "進度",
        ja: "進行"
    },
    Remaining => {
        en: "REMAINING",
        zh_hant: "剩餘",
        ja: "残り"
    },
    GoGoTime => {
        en: "GO-GO TIME",
        zh_hant: "燃燒段",
        ja: "ゴーゴータイム"
    },
    PausedResumeFeedback => {
        en: "Ⅱ  PAUSED — PRESS P TO RESUME",
        zh_hant: "Ⅱ  已暫停 — 按 P 繼續",
        ja: "Ⅱ  一時停止 — P で再開"
    },
    GreatFeedback => {
        en: "● GREAT!",
        zh_hant: "● 良！",
        ja: "● 良！"
    },
    GoodFeedback => {
        en: "● GOOD",
        zh_hant: "● 可",
        ja: "● 可"
    },
    MissFeedback => {
        en: "× MISS",
        zh_hant: "× 不可",
        ja: "× 不可"
    },
    DrumrollFeedback => {
        en: "● DRUMROLL!",
        zh_hant: "● 連打！",
        ja: "● 連打！"
    },
    KeepRhythm => {
        en: "KEEP THE RHYTHM",
        zh_hant: "保持節奏",
        ja: "リズムをキープ"
    },
    PauseLane => {
        en: "PAUSED",
        zh_hant: "已暫停",
        ja: "一時停止"
    },
    GameControlsHelp => {
        en: "•  P PAUSE  •  ESC LEAVE",
        zh_hant: "•  P 暫停  •  ESC 離開",
        ja: "•  P 一時停止  •  ESC 退出"
    },
    OnlineGameControlsHelp => {
        en: "ESC LEAVE",
        zh_hant: "ESC 離開",
        ja: "ESC 退出"
    },
    LocalSessionMissing => {
        en: "Local multiplayer session missing",
        zh_hant: "找不到本機雙人遊戲階段",
        ja: "ローカル対戦セッションがありません"
    },
    LocalTwoPlayer => {
        en: "Local Two Player",
        zh_hant: "本機雙人",
        ja: "ローカル2人対戦"
    },
    NoResult => {
        en: "No result",
        zh_hant: "沒有結果",
        ja: "リザルトがありません"
    },
    Clear => {
        en: "CLEAR",
        zh_hant: "過關",
        ja: "クリア"
    },
    FullCombo => {
        en: "FULL COMBO",
        zh_hant: "全連擊",
        ja: "フルコンボ"
    },
    Grade => {
        en: "Grade",
        zh_hant: "評級",
        ja: "ランク"
    },
    Accuracy => {
        en: "Accuracy",
        zh_hant: "準確率",
        ja: "精度"
    },
    MaxCombo => {
        en: "Max Combo",
        zh_hant: "最高連擊",
        ja: "最大コンボ"
    },
    ResultSummary => {
        en: "Result Summary",
        zh_hant: "結果摘要",
        ja: "リザルト概要"
    },
    Early => {
        en: "Early",
        zh_hant: "偏早",
        ja: "早い"
    },
    Late => {
        en: "Late",
        zh_hant: "偏晚",
        ja: "遅い"
    },
    Centered => {
        en: "Centered",
        zh_hant: "正中",
        ja: "中央"
    },
    RollHits => {
        en: "Roll Hits",
        zh_hant: "連打數",
        ja: "連打数"
    },
    Judgement => {
        en: "Judgement",
        zh_hant: "判定",
        ja: "判定"
    },
    TimingDistribution => {
        en: "Timing Distribution",
        zh_hant: "打擊時間分布",
        ja: "タイミング分布"
    },
    Replay => {
        en: "Replay",
        zh_hant: "重播",
        ja: "リプレイ"
    },
    BranchControls => {
        en: "Branch controls",
        zh_hant: "分歧控制數",
        ja: "分岐制御数"
    },
    Details => {
        en: "Details",
        zh_hant: "詳情",
        ja: "詳細"
    },
    Next => {
        en: "Next",
        zh_hant: "下一步",
        ja: "次へ"
    },
    ResultHideDetails => {
        en: "Enter Retry  •  Esc Songs  •  D Hide Details",
        zh_hant: "Enter 重試  •  Esc 歌曲  •  D 隱藏詳情",
        ja: "Enter リトライ  •  Esc 曲選択  •  D 詳細を隠す"
    },
    ResultShowDetails => {
        en: "Enter Retry  •  Esc Songs  •  D Details",
        zh_hant: "Enter 重試  •  Esc 歌曲  •  D 詳情",
        ja: "Enter リトライ  •  Esc 曲選択  •  D 詳細"
    },
    NewPersonalBest => {
        en: "NEW PERSONAL BEST",
        zh_hant: "刷新個人最佳",
        ja: "自己ベスト更新"
    },
    PersonalBestMatched => {
        en: "PERSONAL BEST MATCHED",
        zh_hant: "追平個人最佳",
        ja: "自己ベストタイ"
    },
    PersonalBestDelta => {
        en: "PB DELTA",
        zh_hant: "與個人最佳差距",
        ja: "自己ベスト差"
    },
    NoLocalResult => {
        en: "No local multiplayer result",
        zh_hant: "沒有本機雙人結果",
        ja: "ローカル対戦のリザルトがありません"
    },
    LocalResultTitle => {
        en: "Local Result",
        zh_hant: "本機對戰結果",
        ja: "ローカル対戦リザルト"
    },
    LocalTwoPlayerResult => {
        en: "Local Two Player Result",
        zh_hant: "本機雙人結果",
        ja: "ローカル2人対戦リザルト"
    },
    LocalResultHideDetails => {
        en: "Enter Rematch  •  Esc Songs  •  D Hide Details",
        zh_hant: "Enter 再戰  •  Esc 歌曲  •  D 隱藏詳情",
        ja: "Enter 再戦  •  Esc 曲選択  •  D 詳細を隠す"
    },
    LocalResultShowDetails => {
        en: "Enter Rematch  •  Esc Songs  •  D Details",
        zh_hant: "Enter 再戰  •  Esc 歌曲  •  D 詳情",
        ja: "Enter 再戦  •  Esc 曲選択  •  D 詳細"
    },
    LocalChooseCourses => {
        en: "Local Two Player — Choose Courses",
        zh_hant: "本機雙人 — 選擇難度",
        ja: "ローカル2人対戦 — コース選択"
    },
    LocalReadyControls => {
        en: "P1: W/S choose, F ready  •  P2: ↑/↓ choose, J or Enter ready",
        zh_hant: "P1：W/S 選擇、F 準備  •  P2：↑/↓ 選擇、J 或 Enter 準備",
        ja: "P1：W/S 選択、F 準備  •  P2：↑/↓ 選択、J または Enter 準備"
    },
    LocalReadyExplanation => {
        en: "Ready players are locked. Press the same ready key to unlock. Esc returns to songs.",
        zh_hant: "準備完成後會鎖定；再按一次準備鍵可解除。Esc 返回歌曲選擇。",
        ja: "準備完了後は固定されます。同じ準備キーで解除、Esc で曲選択へ戻ります。"
    },
    Music => {
        en: "Music",
        zh_hant: "音樂",
        ja: "楽曲"
    },
    Scroll => {
        en: "Scroll",
        zh_hant: "捲動",
        ja: "スクロール"
    },
    SharedSettingsHelp => {
        en: "Tab/Shift+Tab selects a shared setting; Left/Right adjusts it.",
        zh_hant: "Tab/Shift+Tab 選擇共用設定；左/右調整。",
        ja: "Tab/Shift+Tab で共通設定を選び、左/右で変更します。"
    },
    Controls => {
        en: "Controls",
        zh_hant: "操作",
        ja: "操作"
    },
    Ready => {
        en: "READY",
        zh_hant: "準備完成",
        ja: "準備完了"
    },
    Choosing => {
        en: "CHOOSING",
        zh_hant: "選擇中",
        ja: "選択中"
    },
    PhaseConnecting => {
        en: "Connecting",
        zh_hant: "連線中",
        ja: "接続中"
    },
    PhaseJoining => {
        en: "Joining room",
        zh_hant: "加入房間中",
        ja: "ルーム参加中"
    },
    PhaseLobby => {
        en: "Lobby",
        zh_hant: "大廳",
        ja: "ロビー"
    },
    PhaseSpectating => {
        en: "Spectating (waiting for players)",
        zh_hant: "觀戰中（等待玩家）",
        ja: "観戦中（プレイヤー待ち）"
    },
    PhaseSelectingCourse => {
        en: "Selecting course",
        zh_hant: "選擇難度中",
        ja: "コース選択中"
    },
    PhaseDownloading => {
        en: "Downloading",
        zh_hant: "下載中",
        ja: "ダウンロード中"
    },
    PhaseVerifying => {
        en: "Verifying content",
        zh_hant: "驗證內容中",
        ja: "コンテンツ検証中"
    },
    PhaseLoading => {
        en: "Loading chart and audio",
        zh_hant: "載入譜面與音訊中",
        ja: "譜面と音声を読み込み中"
    },
    PhasePrepared => {
        en: "Prepared (not ready)",
        zh_hant: "準備完成（尚未就緒）",
        ja: "準備済み（未確定）"
    },
    PhaseReady => {
        en: "Ready",
        zh_hant: "已就緒",
        ja: "準備完了"
    },
    PhaseCountdown => {
        en: "Countdown",
        zh_hant: "倒數",
        ja: "カウントダウン"
    },
    PhasePlaying => {
        en: "Playing",
        zh_hant: "遊戲中",
        ja: "プレイ中"
    },
    PhaseFinalizing => {
        en: "Finalizing authoritative result",
        zh_hant: "確認權威結果中",
        ja: "確定リザルトを処理中"
    },
    PhaseResults => {
        en: "Results",
        zh_hant: "結果",
        ja: "リザルト"
    },
    PhaseReconnecting => {
        en: "Reconnecting",
        zh_hant: "重新連線中",
        ja: "再接続中"
    },
    PhaseFailed => {
        en: "Connection failed",
        zh_hant: "連線失敗",
        ja: "接続失敗"
    },
    ConnectModesHost => {
        en: "[ Host here ]   Create     Join     Spectate",
        zh_hant: "[ 本機主持 ]   建立     加入     觀戰",
        ja: "[ ホスト ]   作成     参加     観戦"
    },
    ConnectModesCreate => {
        en: "  Host here   [ Create ]   Join     Spectate",
        zh_hant: "  本機主持   [ 建立 ]   加入     觀戰",
        ja: "  ホスト   [ 作成 ]   参加     観戦"
    },
    ConnectModesJoin => {
        en: "  Host here     Create   [ Join ]   Spectate",
        zh_hant: "  本機主持     建立   [ 加入 ]   觀戰",
        ja: "  ホスト     作成   [ 参加 ]   観戦"
    },
    ConnectModesSpectate => {
        en: "  Host here     Create     Join   [ Spectate ]",
        zh_hant: "  本機主持     建立     加入   [ 觀戰 ]",
        ja: "  ホスト     作成     参加   [ 観戦 ]"
    },
    Mode => {
        en: "Mode",
        zh_hant: "模式",
        ja: "モード"
    },
    HostDescription => {
        en: "Starts a private authoritative server on this device.",
        zh_hant: "在此裝置啟動私人權威伺服器。",
        ja: "この端末でプライベートな確定サーバーを起動します。"
    },
    Server => {
        en: "Server",
        zh_hant: "伺服器",
        ja: "サーバー"
    },
    Invite => {
        en: "Invite",
        zh_hant: "邀請",
        ja: "招待"
    },
    Name => {
        en: "Name",
        zh_hant: "名稱",
        ja: "名前"
    },
    HideInviteSecret => {
        en: "F2 hides the invite secret",
        zh_hant: "F2 隱藏邀請密鑰",
        ja: "F2 で招待シークレットを隠す"
    },
    RevealInviteSecret => {
        en: "F2 reveals the invite secret",
        zh_hant: "F2 顯示邀請密鑰",
        ja: "F2 で招待シークレットを表示"
    },
    ConnectControls => {
        en: "Left/Right mode  •  Up/Down fields  •  Enter confirm  •  Esc back",
        zh_hant: "左/右切換模式  •  上/下切換欄位  •  Enter 確認  •  Esc 返回",
        ja: "左/右 モード  •  上/下 項目  •  Enter 決定  •  Esc 戻る"
    },
    ConnectingButton => {
        en: "[ Connecting… ]",
        zh_hant: "[ 連線中… ]",
        ja: "[ 接続中… ]"
    },
    HostButton => {
        en: "[ Start Server & Create Room ]",
        zh_hant: "[ 啟動伺服器並建立房間 ]",
        ja: "[ サーバーを起動してルーム作成 ]"
    },
    ConnectButton => {
        en: "[ Connect ]",
        zh_hant: "[ 連線 ]",
        ja: "[ 接続 ]"
    },
    StartingPrivateServer => {
        en: "Preparing songs and starting a private server… Esc cancels",
        zh_hant: "正在準備歌曲並啟動私人伺服器… Esc 取消",
        ja: "曲を準備してプライベートサーバーを起動中… Esc でキャンセル"
    },
    LoadingAuthoritativeLibrary => {
        en: "Loading the authoritative song library… Esc cancels",
        zh_hant: "正在載入權威歌曲庫… Esc 取消",
        ja: "確定サーバーの曲ライブラリを読み込み中… Esc でキャンセル"
    },
    PreparingSpectatorConnection => {
        en: "Preparing the spectator connection… Esc cancels",
        zh_hant: "正在準備觀戰連線… Esc 取消",
        ja: "観戦接続を準備中… Esc でキャンセル"
    },
    NameRequired => {
        en: "Name is required.",
        zh_hant: "請輸入名稱。",
        ja: "名前を入力してください。"
    },
    ServerRequired => {
        en: "Server is required.",
        zh_hant: "請輸入伺服器位址。",
        ja: "サーバーを入力してください。"
    },
    InviteRequired => {
        en: "Invite is required.",
        zh_hant: "請輸入邀請。",
        ja: "招待を入力してください。"
    },
    LocalHostingCancelled => {
        en: "Local hosting was cancelled.",
        zh_hant: "已取消本機主持。",
        ja: "ローカルホストをキャンセルしました。"
    },
    OnlineConnectionCancelled => {
        en: "Online connection was cancelled.",
        zh_hant: "已取消線上連線。",
        ja: "オンライン接続をキャンセルしました。"
    },
    OnlineMultiplayerTitle => {
        en: " Online Multiplayer ",
        zh_hant: " 線上多人 ",
        ja: " オンライン対戦 "
    },
    Locked => {
        en: "LOCKED",
        zh_hant: "已鎖定",
        ja: "ロック済み"
    },
    LobbySongsLeader => {
        en: "Songs (↑↓ select, Enter lock)",
        zh_hant: "歌曲（↑↓ 選擇，Enter 鎖定）",
        ja: "曲（↑↓ 選択、Enter でロック）"
    },
    LobbySongsWaiting => {
        en: "Songs (waiting for host)",
        zh_hant: "歌曲（等待主持人）",
        ja: "曲（ホスト待ち）"
    },
    Phase => {
        en: "Phase",
        zh_hant: "階段",
        ja: "フェーズ"
    },
    Status => {
        en: "Status",
        zh_hant: "狀態",
        ja: "状態"
    },
    InviteSecretNotice => {
        en: "Room invite (contains an access secret):",
        zh_hant: "房間邀請（包含存取密鑰）：",
        ja: "ルーム招待（アクセス用シークレットを含みます）："
    },
    HideAndCopyInvite => {
        en: "F2 hide  •  F3 copy invite",
        zh_hant: "F2 隱藏  •  F3 複製邀請",
        ja: "F2 隠す  •  F3 招待をコピー"
    },
    HiddenInvite => {
        en: "••••••••  hidden",
        zh_hant: "••••••••  已隱藏",
        ja: "••••••••  非表示"
    },
    RevealAndCopyInvite => {
        en: "F2 reveal  •  F3 copy invite",
        zh_hant: "F2 顯示  •  F3 複製邀請",
        ja: "F2 表示  •  F3 招待をコピー"
    },
    InviteCopied => {
        en: "Invite copied to clipboard",
        zh_hant: "已將邀請複製到剪貼簿",
        ja: "招待をクリップボードにコピーしました"
    },
    CopyFailed => {
        en: "Copy failed",
        zh_hant: "複製失敗",
        ja: "コピー失敗"
    },
    Players => {
        en: "Players",
        zh_hant: "玩家",
        ja: "プレイヤー"
    },
    Dnf => {
        en: "DNF",
        zh_hant: "未完成",
        ja: "未完走"
    },
    Reconnecting => {
        en: "RECONNECTING",
        zh_hant: "重新連線中",
        ja: "再接続中"
    },
    Leader => {
        en: "leader",
        zh_hant: "主持人",
        ja: "リーダー"
    },
    Filter => {
        en: "Filter",
        zh_hant: "篩選",
        ja: "絞り込み"
    },
    RoomInfo => {
        en: "Room Info",
        zh_hant: "房間資訊",
        ja: "ルーム情報"
    },
    CourseControlsPaused => {
        en: "Reconnecting — room controls paused",
        zh_hant: "重新連線中 — 房間操作已暫停",
        ja: "再接続中 — ルーム操作は一時停止中"
    },
    AllReadyStart => {
        en: "All online and ready — Enter starts match, Esc unready",
        zh_hant: "所有玩家皆在線且就緒 — Enter 開始，Esc 取消就緒",
        ja: "全員オンライン・準備完了 — Enter で開始、Esc で準備解除"
    },
    ReadyWaitingOnline => {
        en: "Ready — waiting for online players, Esc unready",
        zh_hant: "已就緒 — 等待線上玩家，Esc 取消就緒",
        ja: "準備完了 — 他のプレイヤー待ち、Esc で準備解除"
    },
    SelectCoursePrepare => {
        en: "Select Course (↑↓ select, Enter prepare)",
        zh_hant: "選擇難度（↑↓ 選擇，Enter 準備）",
        ja: "コース選択（↑↓ 選択、Enter で準備）"
    },
    LocalPreparationFailed => {
        en: "Local preparation failed",
        zh_hant: "本機準備失敗",
        ja: "ローカル準備に失敗"
    },
    FixContentRetry => {
        en: "Fix the audio/content, then press Enter on this course to retry.",
        zh_hant: "修正音訊或內容後，在此難度按 Enter 重試。",
        ja: "音声または内容を修正し、このコースで Enter を押して再試行してください。"
    },
    Clock => {
        en: "Clock",
        zh_hant: "時鐘",
        ja: "クロック"
    },
    ClockReady => {
        en: "Ready",
        zh_hant: "已同步",
        ja: "同期完了"
    },
    ClockChecking => {
        en: "Checking synchronization…",
        zh_hant: "正在檢查同步…",
        ja: "同期を確認中…"
    },
    ClockCheckHelp => {
        en: "If checking does not finish, verify latency/jitter and retry on a steadier connection.",
        zh_hant: "若檢查長時間未完成，請確認延遲與抖動，並改用較穩定的連線重試。",
        ja: "確認が終わらない場合は遅延とジッターを確認し、安定した接続で再試行してください。"
    },
    Selecting => {
        en: "selecting…",
        zh_hant: "選擇中…",
        ja: "選択中…"
    },
    VerifyingHashes => {
        en: "verifying hashes…",
        zh_hant: "驗證雜湊中…",
        ja: "ハッシュ検証中…"
    },
    LoadingChartAudio => {
        en: "loading chart/audio…",
        zh_hant: "載入譜面／音訊中…",
        ja: "譜面／音声を読み込み中…"
    },
    PreparedCheckingClock => {
        en: "prepared (checking clock)",
        zh_hant: "已準備（檢查時鐘中）",
        ja: "準備済み（クロック確認中）"
    },
    Failed => {
        en: "FAILED",
        zh_hant: "失敗",
        ja: "失敗"
    },
    GetReady => {
        en: "GET READY",
        zh_hant: "準備",
        ja: "構えて"
    },
    Start => {
        en: "START!",
        zh_hant: "開始！",
        ja: "スタート！"
    },
    WaitingForRoomSnapshot => {
        en: "Waiting for authoritative room snapshot…",
        zh_hant: "等待權威房間快照…",
        ja: "確定ルームスナップショット待ち…"
    },
    Match => {
        en: "MATCH",
        zh_hant: "遊戲",
        ja: "対戦"
    },
    SongManifestPending => {
        en: "Song manifest pending…",
        zh_hant: "等待歌曲清單…",
        ja: "曲マニフェスト待ち…"
    },
    WaitingOtherPlayers => {
        en: "Waiting for other players…",
        zh_hant: "等待其他玩家…",
        ja: "他のプレイヤーを待っています…"
    },
    OtherPlayers => {
        en: "Other Players",
        zh_hant: "其他玩家",
        ja: "他のプレイヤー"
    },
    WaitingPlayers => {
        en: "Waiting for players…",
        zh_hant: "等待玩家…",
        ja: "プレイヤー待ち…"
    },
    WaitingLiveScore => {
        en: "Waiting for live score…",
        zh_hant: "等待即時分數…",
        ja: "ライブスコア待ち…"
    },
    You => {
        en: "YOU",
        zh_hant: "你",
        ja: "あなた"
    },
    MatchFinished => {
        en: "Match Finished!",
        zh_hant: "遊戲結束！",
        ja: "対戦終了！"
    },
    OnlineResultTitle => {
        en: "Online Result",
        zh_hant: "線上結果",
        ja: "オンラインリザルト"
    },
    ResultControlsPaused => {
        en: "Reconnecting — result controls paused  Ctrl+C: disconnect",
        zh_hant: "重新連線中 — 結果操作已暫停  Ctrl+C：中斷連線",
        ja: "再接続中 — リザルト操作は一時停止中  Ctrl+C：切断"
    },
    LeaderResultControls => {
        en: "Enter: rematch  Esc: return to lobby  Ctrl+C: disconnect",
        zh_hant: "Enter：再戰  Esc：返回大廳  Ctrl+C：中斷連線",
        ja: "Enter：再戦  Esc：ロビーへ戻る  Ctrl+C：切断"
    },
    WaitingLeaderResultControls => {
        en: "Waiting for the leader…  Ctrl+C: disconnect",
        zh_hant: "等待主持人…  Ctrl+C：中斷連線",
        ja: "リーダー待ち…  Ctrl+C：切断"
    },
    Warnings => {
        en: "Warnings",
        zh_hant: "警告",
        ja: "警告"
    },
    LoadWarningsControls => {
        en: "↑/↓: scroll  ←/→: page  Esc/Enter/Ctrl+W: back  Ctrl+C: quit",
        zh_hant: "↑/↓：捲動  ←/→：翻頁  Esc/Enter/Ctrl+W：返回  Ctrl+C：結束",
        ja: "↑/↓：スクロール  ←/→：ページ  Esc/Enter/Ctrl+W：戻る  Ctrl+C：終了"
    },
    NoLoadWarnings => {
        en: "No load warnings.",
        zh_hant: "沒有載入警告。",
        ja: "読み込み警告はありません。"
    },
    Entries => {
        en: "Entries",
        zh_hant: "項目",
        ja: "項目"
    },
    Preparing => {
        en: "Preparing",
        zh_hant: "準備中",
        ja: "準備中"
    },
    PreparingSinglePlayerMatch => {
        en: "Preparing single-player match…",
        zh_hant: "正在準備單人遊戲…",
        ja: "1人プレイを準備中…"
    },
    PreparingLocalTwoPlayerMatch => {
        en: "Preparing local two-player match…",
        zh_hant: "正在準備本機雙人遊戲…",
        ja: "ローカル2人プレイを準備中…"
    },
    PreparingGenericMatch => {
        en: "Preparing match…",
        zh_hant: "正在準備遊戲…",
        ja: "対戦を準備中…"
    },
    LoadingAndValidatingChart => {
        en: "Loading and validating the selected chart data.",
        zh_hant: "正在載入並驗證所選譜面資料。",
        ja: "選択した譜面データを読み込み、検証しています。"
    },
    DecodingChartAudio => {
        en: "Decoding audio when the chart provides a soundtrack.",
        zh_hant: "若譜面含有音樂，將一併解碼音訊。",
        ja: "譜面に音源がある場合は音声をデコードします。"
    },
    CancelPreparationHelp => {
        en: "Esc cancels and returns to course selection.",
        zh_hant: "Esc：取消並返回難度選擇。",
        ja: "Esc：キャンセルしてコース選択へ戻る。"
    },
    TaikoBrand => {
        en: "TAIKO",
        zh_hant: "太鼓",
        ja: "太鼓"
    },
    Don => {
        en: "DON",
        zh_hant: "咚",
        ja: "ドン"
    },
    Kat => {
        en: "KAT",
        zh_hant: "緣",
        ja: "カッ"
    },
    NoTapTimingSamples => {
        en: "No tap timing samples.",
        zh_hant: "沒有打擊時間樣本。",
        ja: "打撃タイミングのサンプルがありません。"
    },
    RollExpiredMissExcluded => {
        en: "(roll and expired misses are excluded)",
        zh_hant: "（不包含連打與逾時未擊）",
        ja: "（連打と時間切れの不可は対象外）"
    },
    TimingZero => {
        en: "0ms",
        zh_hant: "0ms",
        ja: "0ms"
    },
    LanguageEnglishNative => {
        en: "English",
        zh_hant: "English",
        ja: "English"
    },
    LanguageTraditionalChineseNative => {
        en: "繁體中文",
        zh_hant: "繁體中文",
        ja: "繁體中文"
    },
    LanguageJapaneseNative => {
        en: "日本語",
        zh_hant: "日本語",
        ja: "日本語"
    },
    SelectedMatchCouldNotBePrepared => {
        en: "The selected match could not be prepared.",
        zh_hant: "無法準備所選遊戲。",
        ja: "選択した対戦を準備できませんでした。"
    },
    GameplayStoppedRequiredService => {
        en: "Gameplay stopped because a required service failed.",
        zh_hant: "必要服務發生錯誤，遊戲已停止。",
        ja: "必要なサービスでエラーが発生したため、プレイを停止しました。"
    },
    OnlineSessionInterrupted => {
        en: "The online session was interrupted.",
        zh_hant: "線上連線階段已中斷。",
        ja: "オンラインセッションが中断されました。"
    },
    PlayerSettingsCouldNotBeApplied => {
        en: "The player settings could not be applied.",
        zh_hant: "無法套用玩家設定。",
        ja: "プレイヤー設定を適用できませんでした。"
    },
    LoadingSongLibrary => {
        en: "Loading the song library…",
        zh_hant: "正在載入歌曲庫…",
        ja: "楽曲ライブラリを読み込み中…"
    },
    ResourceEndpointHasNoPlayableCharts => {
        en: "The configured resource endpoint has no playable charts.",
        zh_hant: "設定的資源端點沒有可遊玩的譜面。",
        ja: "設定されたリソースエンドポイントにプレイ可能な譜面がありません。"
    },
    OfflineDirectoryHasNoPlayableCharts => {
        en: "The offline song directory has no playable .tja charts.",
        zh_hant: "離線歌曲目錄沒有可遊玩的 .tja 譜面。",
        ja: "オフライン楽曲フォルダーにプレイ可能な .tja 譜面がありません。"
    },
    PreviousSongUnavailable => {
        en: "Your previous song is no longer available; selected the first matching song.",
        zh_hant: "先前的歌曲已無法使用；已選擇第一首相符歌曲。",
        ja: "前回の楽曲は利用できないため、最初に一致する楽曲を選びました。"
    },
    PreviousSongDoesNotMatchSearch => {
        en: "Your previous song no longer matches the saved search; selected the first matching song.",
        zh_hant: "先前的歌曲已不符合儲存的搜尋條件；已選擇第一首相符歌曲。",
        ja: "前回の楽曲は保存された検索条件に一致しないため、最初に一致する楽曲を選びました。"
    },
    PreviousCourseUnavailable => {
        en: "Your previous course is no longer available; selected the first course.",
        zh_hant: "先前的難度已無法使用；已選擇第一個難度。",
        ja: "前回のコースは利用できないため、最初のコースを選びました。"
    },
    LoadingPreview => {
        en: "Loading preview…",
        zh_hant: "正在載入試聽…",
        ja: "プレビューを読み込み中…"
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Localizer {
    language: UiLanguage,
}

impl Localizer {
    pub(crate) const fn new(language: UiLanguage) -> Self {
        Self { language }
    }

    pub(crate) const fn text(self, key: UiText) -> &'static str {
        key.resolve(self.language)
    }

    pub(crate) const fn binding_slot(self, slot: BindingSlot) -> &'static str {
        let key = match slot {
            BindingSlot::LeftKat => UiText::BindingLeftKat,
            BindingSlot::LeftDon => UiText::BindingLeftDon,
            BindingSlot::RightDon => UiText::BindingRightDon,
            BindingSlot::RightKat => UiText::BindingRightKat,
        };
        self.text(key)
    }

    pub(crate) const fn language_name(self, language: UiLanguage) -> &'static str {
        let key = match language {
            UiLanguage::English => UiText::LanguageEnglishNative,
            UiLanguage::TraditionalChinese => UiText::LanguageTraditionalChineseNative,
            UiLanguage::Japanese => UiText::LanguageJapaneseNative,
        };
        self.text(key)
    }

    pub(crate) const fn online_phase(self, phase: OnlinePhase) -> &'static str {
        let key = match phase {
            OnlinePhase::Connecting => UiText::PhaseConnecting,
            OnlinePhase::Joining => UiText::PhaseJoining,
            OnlinePhase::Lobby => UiText::PhaseLobby,
            OnlinePhase::Spectating => UiText::PhaseSpectating,
            OnlinePhase::SelectingCourse => UiText::PhaseSelectingCourse,
            OnlinePhase::Downloading => UiText::PhaseDownloading,
            OnlinePhase::Verifying => UiText::PhaseVerifying,
            OnlinePhase::Loading => UiText::PhaseLoading,
            OnlinePhase::Prepared => UiText::PhasePrepared,
            OnlinePhase::Ready => UiText::PhaseReady,
            OnlinePhase::Countdown => UiText::PhaseCountdown,
            OnlinePhase::Playing => UiText::PhasePlaying,
            OnlinePhase::Finalizing => UiText::PhaseFinalizing,
            OnlinePhase::Results => UiText::PhaseResults,
            OnlinePhase::Reconnecting => UiText::PhaseReconnecting,
            OnlinePhase::Failed => UiText::PhaseFailed,
        };
        self.text(key)
    }

    pub(crate) fn message(self, message: UiMessage<'_>) -> String {
        match message {
            UiMessage::CurrentTerminalSize { width, height } => match self.language {
                UiLanguage::English => format!("Current: {width} × {height}"),
                UiLanguage::TraditionalChinese => format!("目前：{width} × {height}"),
                UiLanguage::Japanese => format!("現在：{width} × {height}"),
            },
            UiMessage::RequiredTerminalSize { width, height } => match self.language {
                UiLanguage::English => format!("Required: at least {width} × {height}"),
                UiLanguage::TraditionalChinese => format!("需要：至少 {width} × {height}"),
                UiLanguage::Japanese => format!("必要：最低 {width} × {height}"),
            },
            UiMessage::SongCount { visible, total } => match self.language {
                UiLanguage::English => format!("{visible}/{total} SONGS"),
                UiLanguage::TraditionalChinese => format!("{visible}/{total} 首歌曲"),
                UiLanguage::Japanese => format!("{visible}/{total} 曲"),
            },
            UiMessage::RoomCode { room } => match self.language {
                UiLanguage::English => format!("ROOM {room}"),
                UiLanguage::TraditionalChinese => format!("房間 {room}"),
                UiLanguage::Japanese => format!("ルーム {room}"),
            },
            UiMessage::CapturingBinding { player, binding } => match self.language {
                UiLanguage::English => {
                    format!("CAPTURING P{player} {binding} — press one visible key")
                }
                UiLanguage::TraditionalChinese => {
                    format!("正在擷取 P{player} {binding} — 請按一個可見按鍵")
                }
                UiLanguage::Japanese => {
                    format!("P{player} {binding} を設定中 — 表示可能なキーを1つ押してください")
                }
            },
            UiMessage::BindingChanged {
                player,
                binding,
                key,
            } => match self.language {
                UiLanguage::English => format!("P{player} {binding} is now {key}"),
                UiLanguage::TraditionalChinese => {
                    format!("P{player} {binding} 已設為 {key}")
                }
                UiLanguage::Japanese => format!("P{player} {binding} を {key} に設定しました"),
            },
            UiMessage::BindingAlreadyAssigned { key } => match self.language {
                UiLanguage::English => format!("{key} is already assigned to another drum input."),
                UiLanguage::TraditionalChinese => {
                    format!("{key} 已指定給另一個太鼓輸入。")
                }
                UiLanguage::Japanese => {
                    format!("{key} は別の太鼓入力に割り当て済みです。")
                }
            },
            UiMessage::BindingChangeFailed { reason } => match self.language {
                UiLanguage::English => format!("The binding could not be changed: {reason}"),
                UiLanguage::TraditionalChinese => format!("無法變更按鍵：{reason}"),
                UiLanguage::Japanese => format!("キー設定を変更できませんでした：{reason}"),
            },
            UiMessage::BindingDetected { player, binding } => match self.language {
                UiLanguage::English => format!("P{player} {binding} input detected"),
                UiLanguage::TraditionalChinese => format!("偵測到 P{player} {binding} 輸入"),
                UiLanguage::Japanese => format!("P{player} {binding} の入力を検出しました"),
            },
            UiMessage::PressNewBinding { player, binding } => match self.language {
                UiLanguage::English => {
                    format!("Press the new key for P{player} {binding}, or Esc to cancel")
                }
                UiLanguage::TraditionalChinese => {
                    format!("請按 P{player} {binding} 的新按鍵，或按 Esc 取消")
                }
                UiLanguage::Japanese => {
                    format!("P{player} {binding} の新しいキーを押すか、Esc でキャンセル")
                }
            },
            UiMessage::OnlineNameByteLimit { max_bytes } => match self.language {
                UiLanguage::English => {
                    format!("Online names are limited to {max_bytes} UTF-8 bytes")
                }
                UiLanguage::TraditionalChinese => {
                    format!("線上名稱上限為 {max_bytes} 個 UTF-8 位元組")
                }
                UiLanguage::Japanese => {
                    format!("オンライン名は UTF-8 で {max_bytes} バイトまでです")
                }
            },
            UiMessage::Utf8ByteLimit { field, max_bytes } => match self.language {
                UiLanguage::English => {
                    format!("{field} is limited to {max_bytes} UTF-8 bytes.")
                }
                UiLanguage::TraditionalChinese => {
                    format!("{field}上限為 {max_bytes} 個 UTF-8 位元組。")
                }
                UiLanguage::Japanese => {
                    format!("{field}は UTF-8 で {max_bytes} バイトまでです。")
                }
            },
            UiMessage::InvalidField { field, reason } => match self.language {
                UiLanguage::English => format!("Invalid {field}: {reason}"),
                UiLanguage::TraditionalChinese => format!("{field}無效：{reason}"),
                UiLanguage::Japanese => format!("{field}が無効です：{reason}"),
            },
            UiMessage::SettingsNotSaved { details } => match self.language {
                UiLanguage::English => format!("Settings were not saved: {details}"),
                UiLanguage::TraditionalChinese => format!("設定未儲存：{details}"),
                UiLanguage::Japanese => format!("設定を保存できませんでした：{details}"),
            },
            UiMessage::ErrorTopbar {
                has_retry,
                destination,
            } => match (self.language, has_retry) {
                (UiLanguage::English, true) => {
                    format!("ENTER RETRY  •  ESC {destination}  •  D DETAILS")
                }
                (UiLanguage::English, false) => {
                    format!("ENTER / ESC {destination}  •  D DETAILS")
                }
                (UiLanguage::TraditionalChinese, true) => {
                    format!("ENTER 重試  •  ESC {destination}  •  D 詳情")
                }
                (UiLanguage::TraditionalChinese, false) => {
                    format!("ENTER / ESC {destination}  •  D 詳情")
                }
                (UiLanguage::Japanese, true) => {
                    format!("ENTER リトライ  •  ESC {destination}  •  D 詳細")
                }
                (UiLanguage::Japanese, false) => {
                    format!("ENTER / ESC {destination}  •  D 詳細")
                }
            },
            UiMessage::ReturnTo { destination } => match self.language {
                UiLanguage::English => format!("to return to {destination}."),
                UiLanguage::TraditionalChinese => format!("返回{destination}。"),
                UiLanguage::Japanese => format!("{destination}に戻ります。"),
            },
            UiMessage::RetryAction { action } => match self.language {
                UiLanguage::English => format!("to {action}."),
                UiLanguage::TraditionalChinese => format!("{action}。"),
                UiLanguage::Japanese => format!("{action}。"),
            },
            UiMessage::CourseListEntry {
                index,
                name,
                level,
                objects,
            } => match self.language {
                UiLanguage::English => {
                    format!("{index:>2}. {name}  Lv {level}  notes={objects}")
                }
                UiLanguage::TraditionalChinese => {
                    format!("{index:>2}. {name}  ★{level}  音符={objects}")
                }
                UiLanguage::Japanese => {
                    format!("{index:>2}. {name}  ★{level}  音符={objects}")
                }
            },
            UiMessage::LocalCourseListEntry { name, level, notes } => match self.language {
                UiLanguage::English => format!("{name}  Lv {level}  notes={notes}"),
                UiLanguage::TraditionalChinese => {
                    format!("{name}  ★{level}  音符={notes}")
                }
                UiLanguage::Japanese => format!("{name}  ★{level}  音符={notes}"),
            },
            UiMessage::LocalOutcome { outcome } => match (self.language, outcome) {
                (UiLanguage::English, LocalOutcome::WinnerP1) => "Winner: P1".to_owned(),
                (UiLanguage::English, LocalOutcome::WinnerP2) => "Winner: P2".to_owned(),
                (UiLanguage::English, LocalOutcome::Tie) => "Tie".to_owned(),
                (UiLanguage::English, LocalOutcome::HigherP1DifferentCourses) => {
                    "Higher score (different courses): P1".to_owned()
                }
                (UiLanguage::English, LocalOutcome::HigherP2DifferentCourses) => {
                    "Higher score (different courses): P2".to_owned()
                }
                (UiLanguage::English, LocalOutcome::EqualDifferentCourses) => {
                    "Equal score (different courses)".to_owned()
                }
                (UiLanguage::TraditionalChinese, LocalOutcome::WinnerP1) => "勝者：P1".to_owned(),
                (UiLanguage::TraditionalChinese, LocalOutcome::WinnerP2) => "勝者：P2".to_owned(),
                (UiLanguage::TraditionalChinese, LocalOutcome::Tie) => "平手".to_owned(),
                (UiLanguage::TraditionalChinese, LocalOutcome::HigherP1DifferentCourses) => {
                    "不同難度，P1 分數較高".to_owned()
                }
                (UiLanguage::TraditionalChinese, LocalOutcome::HigherP2DifferentCourses) => {
                    "不同難度，P2 分數較高".to_owned()
                }
                (UiLanguage::TraditionalChinese, LocalOutcome::EqualDifferentCourses) => {
                    "不同難度，分數相同".to_owned()
                }
                (UiLanguage::Japanese, LocalOutcome::WinnerP1) => "勝者：P1".to_owned(),
                (UiLanguage::Japanese, LocalOutcome::WinnerP2) => "勝者：P2".to_owned(),
                (UiLanguage::Japanese, LocalOutcome::Tie) => "引き分け".to_owned(),
                (UiLanguage::Japanese, LocalOutcome::HigherP1DifferentCourses) => {
                    "別コース・高得点：P1".to_owned()
                }
                (UiLanguage::Japanese, LocalOutcome::HigherP2DifferentCourses) => {
                    "別コース・高得点：P2".to_owned()
                }
                (UiLanguage::Japanese, LocalOutcome::EqualDifferentCourses) => {
                    "別コース・同点".to_owned()
                }
            },
            UiMessage::DownloadingProgress {
                whole_percent,
                tenths_percent,
            } => match self.language {
                UiLanguage::English => {
                    format!("downloading… {whole_percent}.{tenths_percent}%")
                }
                UiLanguage::TraditionalChinese => {
                    format!("下載中… {whole_percent}.{tenths_percent}%")
                }
                UiLanguage::Japanese => {
                    format!("ダウンロード中… {whole_percent}.{tenths_percent}%")
                }
            },
            UiMessage::CourseWithLevel { name, level } => match self.language {
                UiLanguage::English => format!("{name} (Lv {level})"),
                UiLanguage::TraditionalChinese | UiLanguage::Japanese => {
                    format!("{name}（★{level}）")
                }
            },
            UiMessage::TimingStats {
                count,
                average_ms,
                median_ms,
                p90_absolute_ms,
            } => match self.language {
                UiLanguage::English => format!(
                    "Samples={count}  Avg={average_ms:+.2}ms  Median={median_ms:+.2}ms  P90|delta|={p90_absolute_ms:.2}ms"
                ),
                UiLanguage::TraditionalChinese => format!(
                    "樣本={count}  平均={average_ms:+.2}ms  中位數={median_ms:+.2}ms  P90|偏差|={p90_absolute_ms:.2}ms"
                ),
                UiLanguage::Japanese => format!(
                    "標本={count}  平均={average_ms:+.2}ms  中央値={median_ms:+.2}ms  P90|ずれ|={p90_absolute_ms:.2}ms"
                ),
            },
            UiMessage::TimingAxisEarly { range_ms } => match self.language {
                UiLanguage::English => format!("Early <-{range_ms:.1}ms"),
                UiLanguage::TraditionalChinese => format!("偏早 <-{range_ms:.1}ms"),
                UiLanguage::Japanese => format!("早い <-{range_ms:.1}ms"),
            },
            UiMessage::TimingAxisLate { range_ms } => match self.language {
                UiLanguage::English => format!("+{range_ms:.1}ms Late"),
                UiLanguage::TraditionalChinese => format!("+{range_ms:.1}ms 偏晚"),
                UiLanguage::Japanese => format!("+{range_ms:.1}ms 遅い"),
            },
            UiMessage::NoPlayableOfflineCharts { reason } => match self.language {
                UiLanguage::English => {
                    format!("No playable offline charts were loaded: {reason}")
                }
                UiLanguage::TraditionalChinese => {
                    format!("未載入任何可遊玩的離線譜面：{reason}")
                }
                UiLanguage::Japanese => {
                    format!("プレイ可能なオフライン譜面を読み込めませんでした：{reason}")
                }
            },
            UiMessage::SongLibraryLoadFailed { reason } => match self.language {
                UiLanguage::English => {
                    format!("Song library load failed: {reason}. Press R to retry.")
                }
                UiLanguage::TraditionalChinese => {
                    format!("歌曲庫載入失敗：{reason}。按 R 重試。")
                }
                UiLanguage::Japanese => {
                    format!("楽曲ライブラリの読み込みに失敗しました：{reason}。R で再試行します。")
                }
            },
            UiMessage::PreviewUnavailable { reason } => match self.language {
                UiLanguage::English => format!("Preview unavailable: {reason}"),
                UiLanguage::TraditionalChinese => format!("無法試聽：{reason}"),
                UiLanguage::Japanese => format!("プレビューできません：{reason}"),
            },
            UiMessage::PreviewStopFailed { reason } => match self.language {
                UiLanguage::English => format!("Failed to stop preview: {reason}"),
                UiLanguage::TraditionalChinese => format!("停止試聽失敗：{reason}"),
                UiLanguage::Japanese => format!("プレビューの停止に失敗しました：{reason}"),
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum UiMessage<'a> {
    CurrentTerminalSize {
        width: u16,
        height: u16,
    },
    RequiredTerminalSize {
        width: u16,
        height: u16,
    },
    SongCount {
        visible: usize,
        total: usize,
    },
    RoomCode {
        room: &'a str,
    },
    CapturingBinding {
        player: usize,
        binding: &'a str,
    },
    BindingChanged {
        player: usize,
        binding: &'a str,
        key: char,
    },
    BindingAlreadyAssigned {
        key: char,
    },
    BindingChangeFailed {
        reason: &'a str,
    },
    BindingDetected {
        player: usize,
        binding: &'a str,
    },
    PressNewBinding {
        player: usize,
        binding: &'a str,
    },
    OnlineNameByteLimit {
        max_bytes: usize,
    },
    Utf8ByteLimit {
        field: &'a str,
        max_bytes: usize,
    },
    InvalidField {
        field: &'a str,
        reason: &'a str,
    },
    SettingsNotSaved {
        details: &'a str,
    },
    ErrorTopbar {
        has_retry: bool,
        destination: &'a str,
    },
    ReturnTo {
        destination: &'a str,
    },
    RetryAction {
        action: &'a str,
    },
    CourseListEntry {
        index: usize,
        name: &'a str,
        level: &'a str,
        objects: usize,
    },
    LocalCourseListEntry {
        name: &'a str,
        level: &'a str,
        notes: usize,
    },
    LocalOutcome {
        outcome: LocalOutcome,
    },
    DownloadingProgress {
        whole_percent: u32,
        tenths_percent: u32,
    },
    CourseWithLevel {
        name: &'a str,
        level: &'a str,
    },
    TimingStats {
        count: usize,
        average_ms: f64,
        median_ms: f64,
        p90_absolute_ms: f64,
    },
    TimingAxisEarly {
        range_ms: f64,
    },
    TimingAxisLate {
        range_ms: f64,
    },
    NoPlayableOfflineCharts {
        reason: &'a str,
    },
    SongLibraryLoadFailed {
        reason: &'a str,
    },
    PreviewUnavailable {
        reason: &'a str,
    },
    PreviewStopFailed {
        reason: &'a str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalOutcome {
    WinnerP1,
    WinnerP2,
    Tie,
    HigherP1DifferentCourses,
    HigherP2DifferentCourses,
    EqualDifferentCourses,
}

pub(crate) fn display_width(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}

pub(crate) fn pop_grapheme(value: &mut String) -> bool {
    let Some((start, _)) = value.grapheme_indices(true).next_back() else {
        return false;
    };
    value.truncate(start);
    true
}

pub(crate) fn truncate_to_width(value: &str, max_width: usize) -> String {
    if display_width(value) <= max_width {
        return value.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }

    const ELLIPSIS: &str = "…";
    let ellipsis_width = display_width(ELLIPSIS);
    if max_width <= ellipsis_width {
        return ELLIPSIS.to_owned();
    }

    let content_width = max_width - ellipsis_width;
    let mut result = String::new();
    let mut used = 0;
    for grapheme in value.graphemes(true) {
        let width = display_width(grapheme);
        if used + width > content_width {
            break;
        }
        result.push_str(grapheme);
        used += width;
    }
    result.push_str(ELLIPSIS);
    result
}

pub(crate) fn pad_or_truncate_to_width(value: &str, width: usize) -> String {
    let mut fitted = truncate_to_width(value, width);
    let padding = width.saturating_sub(display_width(&fitted));
    fitted.extend(std::iter::repeat_n(' ', padding));
    fitted
}

pub(crate) fn truncate_tail_to_width(value: &str, max_width: usize) -> String {
    if display_width(value) <= max_width {
        return value.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }

    const ELLIPSIS: &str = "…";
    let ellipsis_width = display_width(ELLIPSIS);
    if max_width <= ellipsis_width {
        return ELLIPSIS.to_owned();
    }

    let content_width = max_width - ellipsis_width;
    let mut kept = Vec::new();
    let mut used = 0;
    for grapheme in value.graphemes(true).rev() {
        let width = display_width(grapheme);
        if used + width > content_width {
            break;
        }
        kept.push(grapheme);
        used += width;
    }
    kept.reverse();
    format!("{ELLIPSIS}{}", kept.concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_resolves_without_a_fallback() {
        for language in UiLanguage::ALL {
            let localizer = Localizer::new(language);
            assert!(!localizer.text(UiText::ChoosePlayMode).is_empty());
            assert!(!localizer.text(UiText::TopOnlineMatch).is_empty());
            assert!(!localizer.text(UiText::StartingPrivateServer).is_empty());
            assert!(!localizer.text(UiText::OnlineConnectionCancelled).is_empty());
            assert!(!localizer.text(UiText::ClockChecking).is_empty());
            assert!(localizer
                .message(UiMessage::Utf8ByteLimit {
                    field: localizer.text(UiText::Invite),
                    max_bytes: 4_096,
                })
                .contains("4096"));
        }
    }

    #[test]
    fn truncation_uses_display_columns_and_preserves_graphemes() {
        assert_eq!(display_width("太鼓ABC"), 7);
        assert_eq!(truncate_to_width("太鼓ABC", 7), "太鼓ABC");
        assert_eq!(truncate_to_width("太鼓ABC", 6), "太鼓A…");
        assert_eq!(truncate_to_width("か\u{3099}きく", 5), "か\u{3099}き…");
        assert_eq!(display_width(&truncate_to_width("繁體中文", 5)), 5);
        assert_eq!(display_width(&pad_or_truncate_to_width("日本語", 10)), 10);
        assert_eq!(truncate_tail_to_width("設定プレイヤー名", 9), "…イヤー名");
        assert_eq!(
            display_width(&truncate_tail_to_width("設定プレイヤー名", 9)),
            9
        );
    }

    #[test]
    fn backspace_removes_one_user_perceived_character() {
        for (mut value, expected) in [
            ("Cafe\u{301}".to_owned(), "Caf"),
            ("A👨‍👩‍👧‍👦".to_owned(), "A"),
            ("A🇹🇼".to_owned(), "A"),
        ] {
            assert!(pop_grapheme(&mut value));
            assert_eq!(value, expected);
        }

        let mut empty = String::new();
        assert!(!pop_grapheme(&mut empty));
    }
}
