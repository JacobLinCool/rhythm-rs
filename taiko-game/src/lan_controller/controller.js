"use strict";

(() => {
  const protocolVersion = 1;
  const protocol = `taiko-controller-v${protocolVersion}`;
  const tokenPattern = /^[0-9a-f]{64}$/;
  const maximumInboundMessageLength = 4096;
  const messageCatalog = Object.freeze({
    en: Object.freeze({
      title: "Taiko Controller",
      eyebrow: "TAIKO CONTROLLER",
      pairing: "Pairing…",
      connecting: "Connecting to the game",
      drumControls: "Taiko drum controls",
      leftKat: "Left Kat",
      leftDon: "Left Don",
      rightDon: "Right Don",
      rightKat: "Right Kat",
      left: "LEFT",
      right: "RIGHT",
      kat: "KAT",
      don: "DON",
      rim: "RIM",
      center: "CENTER",
      trustedLan: "Keep this page open on the same trusted LAN as the game.",
      completeLink: "Open the complete pairing link shown by the game.",
      reconnectFailed: "Could not reconnect to the game.",
      reconnectingIn: "Reconnecting in {seconds}s",
      reconnecting: "Reconnecting",
      connectionInterrupted: "Connection interrupted",
      invalidResponse: "The game sent an invalid response.",
      completingPairing: "Completing pairing",
      player1: "Player 1",
      player2: "Player 2",
      ready: "Ready",
      waitingGameplay: "Waiting for gameplay",
      sessionExpired: "Session expired. Re-pairing",
      pairingExpired:
        "Pairing expired. Ask the game to rotate this player link.",
      handshakeFailed: "The controller handshake failed. Reload this page.",
      serverStopped: "The controller server stopped.",
      rateLimited: "Too many hits. Reconnecting shortly.",
      queueFull: "The game is catching up. Reconnecting shortly.",
      rejected: "The controller connection was rejected.",
      waitingGame: "Waiting for the game",
      paused: "Paused",
    }),
    "zh-Hant": Object.freeze({
      title: "太鼓控制器",
      eyebrow: "太鼓控制器",
      pairing: "配對中…",
      connecting: "正在連線至遊戲",
      drumControls: "太鼓鼓面控制",
      leftKat: "左側鼓邊",
      leftDon: "左側鼓面",
      rightDon: "右側鼓面",
      rightKat: "右側鼓邊",
      left: "左",
      right: "右",
      kat: "咖",
      don: "咚",
      rim: "鼓邊",
      center: "鼓面",
      trustedLan: "請讓本頁保持開啟，並確定裝置與遊戲位於相同的可信任區域網路。",
      completeLink: "請開啟遊戲顯示的完整配對連結。",
      reconnectFailed: "無法重新連線至遊戲。",
      reconnectingIn: "{seconds} 秒後重新連線",
      reconnecting: "正在重新連線",
      connectionInterrupted: "連線中斷",
      invalidResponse: "遊戲傳回了無效回應。",
      completingPairing: "正在完成配對",
      player1: "玩家 1",
      player2: "玩家 2",
      ready: "準備完成",
      waitingGameplay: "等待遊戲開始",
      sessionExpired: "工作階段已過期，正在重新配對",
      pairingExpired: "配對已過期，請在遊戲中更新此玩家的配對連結。",
      handshakeFailed: "控制器交握失敗，請重新載入此頁。",
      serverStopped: "控制器伺服器已停止。",
      rateLimited: "輸入過於頻繁，稍後將重新連線。",
      queueFull: "遊戲正在追上輸入，稍後將重新連線。",
      rejected: "控制器連線遭到拒絕。",
      waitingGame: "等待遊戲",
      paused: "已暫停",
    }),
    ja: Object.freeze({
      title: "太鼓コントローラー",
      eyebrow: "太鼓コントローラー",
      pairing: "ペアリング中…",
      connecting: "ゲームに接続中",
      drumControls: "太鼓の操作",
      leftKat: "左のカッ",
      leftDon: "左のドン",
      rightDon: "右のドン",
      rightKat: "右のカッ",
      left: "左",
      right: "右",
      kat: "カッ",
      don: "ドン",
      rim: "ふち",
      center: "面",
      trustedLan:
        "このページを開いたまま、ゲームと同じ信頼できるLANに接続してください。",
      completeLink:
        "ゲームに表示された完全なペアリングリンクを開いてください。",
      reconnectFailed: "ゲームに再接続できませんでした。",
      reconnectingIn: "{seconds}秒後に再接続",
      reconnecting: "再接続中",
      connectionInterrupted: "接続が中断されました",
      invalidResponse: "ゲームから無効な応答を受信しました。",
      completingPairing: "ペアリングを完了しています",
      player1: "プレイヤー 1",
      player2: "プレイヤー 2",
      ready: "準備完了",
      waitingGameplay: "ゲーム開始を待っています",
      sessionExpired: "セッションの期限が切れました。再ペアリングしています",
      pairingExpired:
        "ペアリングの期限が切れました。ゲームでこのプレイヤーのリンクを更新してください。",
      handshakeFailed:
        "コントローラーのハンドシェイクに失敗しました。このページを再読み込みしてください。",
      serverStopped: "コントローラーサーバーが停止しました。",
      rateLimited: "入力が多すぎます。まもなく再接続します。",
      queueFull: "ゲームが入力を処理しています。まもなく再接続します。",
      rejected: "コントローラー接続が拒否されました。",
      waitingGame: "ゲームを待っています",
      paused: "一時停止中",
    }),
  });
  const languageParams = new URLSearchParams(window.location.search);
  const requestedLanguage = languageParams.get("lang");
  const language = Object.prototype.hasOwnProperty.call(
    messageCatalog,
    requestedLanguage,
  )
    ? requestedLanguage
    : "en";
  const messages = messageCatalog[language];
  const params = new URLSearchParams(window.location.hash.slice(1));
  const pairingToken = params.get("token");
  const status = document.getElementById("status");
  const player = document.getElementById("player");
  const pads = Array.from(document.querySelectorAll(".pad"));
  const pointers = new Map();

  const text = (key, values = {}) =>
    messages[key].replace(/\{([a-z]+)\}/g, (placeholder, name) =>
      Object.prototype.hasOwnProperty.call(values, name)
        ? String(values[name])
        : placeholder,
    );

  document.documentElement.lang = language;
  document.title = text("title");
  for (const element of document.querySelectorAll("[data-i18n]")) {
    element.textContent = text(element.dataset.i18n);
  }
  for (const element of document.querySelectorAll("[data-i18n-aria-label]")) {
    element.setAttribute(
      "aria-label",
      text(element.dataset.i18nAriaLabel),
    );
  }

  let socket = null;
  let ready = false;
  let nextSequence = 1;
  let reconnectAttempt = 0;
  let reconnectTimer = null;
  let permanentFailure = false;
  let suspended = false;
  let connectionAttempted = false;
  let sessionToken = null;
  let nextAcknowledgement = 2;
  const storageKey = pairingToken
    ? `taiko-controller-session:${pairingToken}`
    : null;

  const setStatus = (key, values) => {
    status.textContent = text(key, values);
  };

  const clearPressed = () => {
    pointers.clear();
    for (const pad of pads) {
      pad.classList.remove("pressed");
      pad.setAttribute("aria-pressed", "false");
    }
  };

  const fail = (key) => {
    permanentFailure = true;
    ready = false;
    clearPressed();
    if (reconnectTimer !== null) {
      window.clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
    setStatus(key);
  };

  const loadStoredSessionToken = () => {
    try {
      const stored = window.sessionStorage.getItem(storageKey);
      if (typeof stored === "string" && tokenPattern.test(stored)) {
        return stored;
      }
      if (stored !== null) {
        window.sessionStorage.removeItem(storageKey);
      }
    } catch {
      // Private browsing and embedded browsers may deny storage entirely.
    }
    return null;
  };

  const rememberSessionToken = (token) => {
    sessionToken = token;
    try {
      window.sessionStorage.setItem(storageKey, token);
    } catch {
      // The in-memory token remains authoritative for this page lifetime.
    }
  };

  const forgetSessionToken = () => {
    sessionToken = null;
    try {
      window.sessionStorage.removeItem(storageKey);
    } catch {
      // Revocation must still take effect in memory when storage is unavailable.
    }
  };

  const closeConnection = (connection, reason) => {
    try {
      connection.close(1000, reason);
    } catch {
      // A concurrently closed socket needs no further cleanup.
    }
  };

  const sendJson = (connection, message) => {
    if (
      connection !== socket ||
      connection.readyState !== WebSocket.OPEN
    ) {
      return false;
    }
    try {
      connection.send(JSON.stringify(message));
      return true;
    } catch {
      return false;
    }
  };

  const parseInboundMessage = (event) => {
    if (
      typeof event.data !== "string" ||
      event.data.length > maximumInboundMessageLength
    ) {
      return null;
    }
    try {
      const message = JSON.parse(event.data);
      if (
        message === null ||
        typeof message !== "object" ||
        Array.isArray(message)
      ) {
        return null;
      }
      return message;
    } catch {
      return null;
    }
  };

  const createWebSocket = () => {
    try {
      const connection = new WebSocket(
        `ws://${window.location.host}/controller/ws`,
        protocol,
      );
      return connection;
    } catch {
      return null;
    }
  };

  if (!pairingToken || !tokenPattern.test(pairingToken)) {
    fail("completeLink");
    return;
  }

  sessionToken = loadStoredSessionToken();

  const scheduleReconnect = () => {
    if (
      permanentFailure ||
      suspended ||
      reconnectTimer !== null ||
      socket !== null
    ) {
      return;
    }
    reconnectAttempt += 1;
    if (reconnectAttempt > 20) {
      fail("reconnectFailed");
      return;
    }
    const delay = Math.min(
      250 * 2 ** Math.min(reconnectAttempt - 1, 5),
      5000,
    );
    setStatus("reconnectingIn", {
      seconds: Math.ceil(delay / 1000),
    });
    reconnectTimer = window.setTimeout(() => {
      reconnectTimer = null;
      connect();
    }, delay);
  };

  const connect = () => {
    if (
      permanentFailure ||
      suspended ||
      socket !== null ||
      reconnectTimer !== null
    ) {
      return;
    }

    const connection = createWebSocket();
    if (connection === null) {
      scheduleReconnect();
      return;
    }

    const reconnecting = connectionAttempted;
    connectionAttempted = true;
    let handshakePhase = "awaiting_ready";
    let pendingConnectionId = null;
    let pendingSlot = null;
    let authenticationAttempt = null;
    let retryPairOnClose = false;
    let acceptingMessages = true;
    socket = connection;
    setStatus(reconnecting ? "reconnecting" : "connecting");

    connection.addEventListener("open", () => {
      if (connection !== socket) {
        closeConnection(connection, "superseded");
        return;
      }
      authenticationAttempt = sessionToken === null ? "pair" : "resume";
      const message = authenticationAttempt === "resume"
        ? {
            type: "resume",
            protocol: protocolVersion,
            session_token: sessionToken,
          }
        : { type: "pair", protocol: protocolVersion, token: pairingToken };
      if (!sendJson(connection, message)) {
        setStatus("connectionInterrupted");
        closeConnection(connection, "handshake send failed");
      }
    });

    connection.addEventListener("message", (event) => {
      if (connection !== socket || !acceptingMessages) {
        return;
      }

      const message = parseInboundMessage(event);
      if (message === null) {
        fail("invalidResponse");
        closeConnection(connection, "invalid response");
        return;
      }

      if (
        message.type === "ready" &&
        message.protocol === protocolVersion &&
        (message.slot === "p1" || message.slot === "p2") &&
        Number.isSafeInteger(message.connection_id) &&
        message.connection_id > 0 &&
        typeof message.commit_required === "boolean" &&
        (message.session_token === undefined ||
          (typeof message.session_token === "string" &&
            tokenPattern.test(message.session_token)))
      ) {
        const candidateSessionToken =
          typeof message.session_token === "string"
            ? message.session_token
            : sessionToken;
        if (candidateSessionToken === null || handshakePhase === "ready") {
          fail("invalidResponse");
          closeConnection(connection, "invalid ready response");
          return;
        }

        if (message.commit_required) {
          if (handshakePhase !== "awaiting_ready") {
            fail("invalidResponse");
            closeConnection(connection, "invalid pairing phase");
            return;
          }
          handshakePhase = "awaiting_commit";
          pendingConnectionId = message.connection_id;
          pendingSlot = message.slot;
          if (typeof message.session_token === "string") {
            rememberSessionToken(message.session_token);
          }
          player.textContent = text(message.slot === "p2" ? "player2" : "player1");
          setStatus("completingPairing");
          if (
            !sendJson(connection, {
              type: "ready_ack",
              connection_id: message.connection_id,
            })
          ) {
            setStatus("connectionInterrupted");
            closeConnection(connection, "pairing acknowledgement failed");
          }
          return;
        }

        if (
          handshakePhase === "awaiting_commit" &&
          (pendingConnectionId !== message.connection_id ||
            pendingSlot !== message.slot)
        ) {
          fail("invalidResponse");
          closeConnection(connection, "pairing connection changed");
          return;
        }
        if (
          handshakePhase !== "awaiting_ready" &&
          handshakePhase !== "awaiting_commit"
        ) {
          fail("invalidResponse");
          closeConnection(connection, "invalid ready phase");
          return;
        }

        if (typeof message.session_token === "string") {
          rememberSessionToken(message.session_token);
        }
        player.textContent = text(message.slot === "p2" ? "player2" : "player1");
        handshakePhase = "ready";
        nextSequence = 1;
        nextAcknowledgement = 2;
        reconnectAttempt = 0;
        ready = true;
        setStatus("ready");
        return;
      }

      if (
        message.type === "ack" &&
        handshakePhase === "ready" &&
        message.next_seq === nextAcknowledgement &&
        typeof message.accepted === "boolean"
      ) {
        nextAcknowledgement += 1;
        if (!message.accepted) {
          setStatus("waitingGameplay");
        } else {
          setStatus("ready");
        }
        return;
      }

      if (message.type === "error") {
        acceptingMessages = false;
        if (
          message.code === "unauthorized" &&
          authenticationAttempt === "resume"
        ) {
          retryPairOnClose = true;
          forgetSessionToken();
          ready = false;
          clearPressed();
          setStatus("sessionExpired");
        } else if (
          message.code === "unauthorized" &&
          authenticationAttempt === "pair"
        ) {
          forgetSessionToken();
          fail("pairingExpired");
        } else if (message.code === "invalid_handshake") {
          fail("handshakeFailed");
        } else if (message.code === "server_stopping") {
          fail("serverStopped");
        } else if (message.code === "rate_limited") {
          ready = false;
          clearPressed();
          setStatus("rateLimited");
        } else if (message.code === "queue_full") {
          ready = false;
          clearPressed();
          setStatus("queueFull");
        } else {
          fail("rejected");
        }
        closeConnection(connection, "server error");
        return;
      }

      fail("invalidResponse");
      closeConnection(connection, "invalid message");
    });

    connection.addEventListener("close", () => {
      if (connection !== socket) {
        return;
      }
      socket = null;
      ready = false;
      clearPressed();
      if (permanentFailure) {
        return;
      }
      if (retryPairOnClose) {
        reconnectAttempt = 0;
        connect();
        return;
      }
      scheduleReconnect();
    });

    connection.addEventListener("error", () => {
      if (connection !== socket) {
        return;
      }
      setStatus("connectionInterrupted");
    });
  };

  const releasePointer = (event) => {
    const pad = pointers.get(event.pointerId);
    if (!pad) {
      return;
    }
    pointers.delete(event.pointerId);
    const stillPressed = Array.from(pointers.values()).includes(pad);
    if (!stillPressed) {
      pad.classList.remove("pressed");
      pad.setAttribute("aria-pressed", "false");
    }
  };

  for (const pad of pads) {
    pad.addEventListener("pointerdown", (event) => {
      event.preventDefault();
      pointers.set(event.pointerId, pad);
      pad.classList.add("pressed");
      pad.setAttribute("aria-pressed", "true");
      if (typeof pad.setPointerCapture === "function") {
        pad.setPointerCapture(event.pointerId);
      }

      if (!ready || !socket || socket.readyState !== WebSocket.OPEN) {
        setStatus("waitingGame");
        return;
      }

      const connection = socket;
      if (
        !sendJson(connection, {
          type: "hit",
          seq: nextSequence,
          action: pad.dataset.action,
        })
      ) {
        ready = false;
        clearPressed();
        setStatus("connectionInterrupted");
        closeConnection(connection, "hit send failed");
        return;
      }
      nextSequence += 1;
      if (navigator.vibrate) {
        navigator.vibrate(8);
      }
    });
    pad.addEventListener("pointerup", releasePointer);
    pad.addEventListener("pointercancel", releasePointer);
    pad.addEventListener("lostpointercapture", releasePointer);
    pad.addEventListener("contextmenu", (event) => event.preventDefault());
  }

  window.addEventListener("pagehide", () => {
    suspended = true;
    ready = false;
    clearPressed();
    if (reconnectTimer !== null) {
      window.clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
    const connection = socket;
    socket = null;
    if (!permanentFailure) {
      setStatus("paused");
    }
    if (connection) {
      closeConnection(connection, "page hidden");
    }
  });

  window.addEventListener("pageshow", () => {
    suspended = false;
    if (!permanentFailure && socket === null && reconnectTimer === null) {
      reconnectAttempt = 0;
      connect();
    }
  });

  connect();
})();
