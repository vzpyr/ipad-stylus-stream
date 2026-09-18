const canvas = document.getElementById("stylus-canvas");
const ctx = canvas.getContext("2d");
const areaContainer = document.getElementById("area-container");
const areaSlider = document.getElementById("area-width-slider");
const areaVal = document.getElementById("area-val");
const btnMinus = document.getElementById("btn-minus");
const btnPlus = document.getElementById("btn-plus");
const smoothingToggle = document.getElementById("smoothing-toggle");
const rotateButton = document.getElementById("rotate-button");
const latencyVal = document.getElementById("latency-val");
const rateVal = document.getElementById("rate-val");
const clearButton = document.getElementById("clear-button");

let socket = null;
let eventCount = 0;
let lastCountReset = performance.now();
let lastPoint = null;
let smoothingActive = true;
let rotation = 0;
let nextPingId = 1;
const pendingPings = new Map();

let displayAspect = 16 / 9;

function applyAreaSize() {
  const percentage = parseInt(areaSlider.value, 10);
  areaVal.textContent = `${percentage}%`;

  const wrapper = areaContainer.parentElement;
  const maxW = Math.max(100, wrapper.clientWidth - 16);
  const maxH = Math.max(100, wrapper.clientHeight - 16);

  const maxDimension = Math.min(maxW, maxH);
  const scale = percentage / 100;

  let baseW;
  let baseH;

  if (displayAspect >= 1) {
    baseW = maxDimension;
    baseH = maxDimension / displayAspect;
  } else {
    baseH = maxDimension;
    baseW = maxDimension * displayAspect;
  }

  const isRotated = rotation === 90 || rotation === 270;
  const targetW = isRotated ? baseH : baseW;
  const targetH = isRotated ? baseW : baseH;

  const finalW = Math.max(50, Math.round(targetW * scale));
  const finalH = Math.max(50, Math.round(targetH * scale));

  areaContainer.style.width = `${finalW}px`;
  areaContainer.style.height = `${finalH}px`;

  const dpr = window.devicePixelRatio || 1;
  canvas.width = Math.round(finalW * dpr);
  canvas.height = Math.round(finalH * dpr);

  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.scale(dpr, dpr);
  lastPoint = null;
}

btnMinus.addEventListener("click", () => {
  const current = parseInt(areaSlider.value, 10);
  areaSlider.value = Math.max(15, current - 5);
  applyAreaSize();
});

btnPlus.addEventListener("click", () => {
  const current = parseInt(areaSlider.value, 10);
  areaSlider.value = Math.min(100, current + 5);
  applyAreaSize();
});

smoothingToggle.addEventListener("click", () => {
  smoothingActive = !smoothingActive;
  smoothingToggle.classList.toggle("active", smoothingActive);
  smoothingToggle.textContent = smoothingActive ? "on" : "off";
  sendSmoothingConfig();
});

rotateButton.addEventListener("click", () => {
  rotation = (rotation + 90) % 360;
  rotateButton.textContent = `${rotation}°`;
  rotateButton.classList.toggle("on", rotation !== 0);
  applyAreaSize();
});

function sendSmoothingConfig() {
  if (socket && socket.readyState === WebSocket.OPEN) {
    socket.send(
      JSON.stringify({
        kind: "config",
        smoothing: smoothingActive,
      }),
    );
  }
}

areaSlider.addEventListener("input", applyAreaSize);
areaSlider.addEventListener("change", applyAreaSize);
window.addEventListener("resize", applyAreaSize);
applyAreaSize();

clearButton.addEventListener("click", () => {
  const dpr = window.devicePixelRatio || 1;
  ctx.save();
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  ctx.restore();
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.scale(dpr, dpr);
  lastPoint = null;
});

function connectWebSocket() {
  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  const wsUrl = `${protocol}//${window.location.host}/ws`;

  socket = new WebSocket(wsUrl);

  socket.addEventListener("open", sendSmoothingConfig);

  socket.addEventListener("message", (msg) => {
    if (typeof msg.data !== "string") return;

    try {
      const data = JSON.parse(msg.data);

      if (data.kind === "display") {
        if (data.width > 0 && data.height > 0) {
          displayAspect = data.width / data.height;
          applyAreaSize();
        }
      } else if (data.kind === "pong" && pendingPings.has(data.id)) {
        const sentAt = pendingPings.get(data.id);
        pendingPings.delete(data.id);
        latencyVal.textContent = `${(performance.now() - sentAt).toFixed(1)} ms`;
      }
    } catch (_e) {}
  });

  socket.addEventListener("close", () => {
    latencyVal.textContent = "-- ms";
    setTimeout(connectWebSocket, 1000);
  });

  socket.addEventListener("error", () => socket.close());
}

function transformCoordinates(u, v, tiltX, tiltY) {
  if (rotation === 90) {
    return {
      x: v,
      y: 1 - u,
      tx: tiltY,
      ty: -tiltX,
    };
  }

  if (rotation === 180) {
    return {
      x: 1 - u,
      y: 1 - v,
      tx: -tiltX,
      ty: -tiltY,
    };
  }

  if (rotation === 270) {
    return {
      x: 1 - v,
      y: u,
      tx: -tiltY,
      ty: tiltX,
    };
  }

  return {
    x: u,
    y: v,
    tx: tiltX,
    ty: tiltY,
  };
}

function sendNormalizedPayload(type, absX, absY, pressure, tiltX, tiltY) {
  eventCount += 1;

  const rect = canvas.getBoundingClientRect();
  const x = absX - rect.left;
  const y = absY - rect.top;
  const u = Math.max(0, Math.min(1, x / rect.width));
  const v = Math.max(0, Math.min(1, y / rect.height));
  const coords = transformCoordinates(u, v, tiltX, tiltY);

  if (socket && socket.readyState === WebSocket.OPEN) {
    socket.send(
      JSON.stringify({
        kind: "pointer",
        t: type,
        x: coords.x,
        y: coords.y,
        p: pressure,
        tx: coords.tx,
        ty: coords.ty,
      }),
    );
  }

  drawFeedback(type, x, y, pressure);
}

function drawFeedback(type, x, y, pressure) {
  if (type === "down") {
    lastPoint = { x, y };
    return;
  }

  if (type === "up") {
    lastPoint = null;
    return;
  }

  if (pressure <= 0) {
    lastPoint = null;
    return;
  }

  if (!lastPoint) lastPoint = { x, y };

  ctx.beginPath();
  ctx.moveTo(lastPoint.x, lastPoint.y);
  ctx.lineTo(x, y);
  ctx.strokeStyle = "#eeeeee";
  ctx.lineWidth = Math.max(2, pressure * 8);
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  ctx.stroke();

  lastPoint = { x, y };
}

function sendPointerPayload(type, event) {
  if (event.pointerType !== "pen") return;

  const pressure = typeof event.pressure === "number" ? event.pressure : 0;
  const tiltX = typeof event.tiltX === "number" ? event.tiltX : 0;
  const tiltY = typeof event.tiltY === "number" ? event.tiltY : 0;

  sendNormalizedPayload(
    type,
    event.clientX,
    event.clientY,
    pressure,
    tiltX,
    tiltY,
  );
}

function handlePointerDown(event) {
  event.preventDefault();
  sendPointerPayload("down", event);
}

function handlePointerMove(event) {
  event.preventDefault();

  const coalesced =
    typeof event.getCoalescedEvents === "function"
      ? event.getCoalescedEvents()
      : null;

  if (coalesced && coalesced.length > 0) {
    for (const e of coalesced) {
      sendPointerPayload("move", e);
    }
  } else {
    sendPointerPayload("move", event);
  }
}

function handlePointerUp(event) {
  event.preventDefault();
  sendPointerPayload("up", event);
}

function sendTouchPayload(type, touch) {
  const pressure = typeof touch.force === "number" ? touch.force : 0;
  const altitude =
    typeof touch.altitudeAngle === "number" ? touch.altitudeAngle : 0;
  const azimuth =
    typeof touch.azimuthAngle === "number" ? touch.azimuthAngle : 0;

  const tiltX = Math.round(
    Math.cos(azimuth) * Math.cos(altitude) * (180 / Math.PI),
  );

  const tiltY = Math.round(
    Math.sin(azimuth) * Math.cos(altitude) * (180 / Math.PI),
  );

  sendNormalizedPayload(
    type,
    touch.clientX,
    touch.clientY,
    pressure,
    tiltX,
    tiltY,
  );
}

function makeTouchHandler(type) {
  return (event) => {
    event.preventDefault();

    for (const touch of event.changedTouches) {
      if (touch.touchType === "stylus") {
        sendTouchPayload(type, touch);
      }
    }
  };
}

canvas.addEventListener("pointerdown", handlePointerDown, {
  passive: false,
});
canvas.addEventListener("pointermove", handlePointerMove, {
  passive: false,
});
canvas.addEventListener("pointerup", handlePointerUp, {
  passive: false,
});
canvas.addEventListener("pointercancel", handlePointerUp, {
  passive: false,
});

canvas.addEventListener("touchstart", makeTouchHandler("down"), {
  passive: false,
});
canvas.addEventListener("touchmove", makeTouchHandler("move"), {
  passive: false,
});
canvas.addEventListener("touchend", makeTouchHandler("up"), { passive: false });
canvas.addEventListener("touchcancel", makeTouchHandler("up"), {
  passive: false,
});

document.addEventListener("contextmenu", (e) => e.preventDefault(), {
  passive: false,
});

document.addEventListener("dblclick", (e) => e.preventDefault(), {
  passive: false,
});

setInterval(() => {
  const elapsed = (performance.now() - lastCountReset) / 1000;

  if (elapsed >= 1) {
    rateVal.textContent = `${Math.round(eventCount / elapsed)}/s`;
    eventCount = 0;
    lastCountReset = performance.now();
  }
}, 1000);

setInterval(() => {
  if (socket && socket.readyState === WebSocket.OPEN) {
    const id = nextPingId;
    nextPingId = (nextPingId + 1) % 1000000;

    pendingPings.set(id, performance.now());

    socket.send(
      JSON.stringify({
        kind: "ping",
        id,
      }),
    );
  }
}, 1000);

connectWebSocket();
