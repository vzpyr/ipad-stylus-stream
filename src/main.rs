use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, InputEvent, InputId, KeyCode, PropType,
    SynchronizationCode, UinputAbsSetup,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::interval;
use tower_http::services::ServeDir;

const MAX_TILT: f64 = 90.0;
const SMOOTH_ALPHA: f64 = 0.42;

#[derive(Debug, Clone, Copy)]
struct DisplayInfo {
    width: i32,
    height: i32,
    refresh_hz: f64,
}

fn detect_display() -> Result<DisplayInfo, Box<dyn std::error::Error>> {
    let out = std::process::Command::new("xrandr")
        .arg("--current")
        .output()?;
    if !out.status.success() {
        return Err("xrandr failed".into());
    }
    let text = String::from_utf8(out.stdout)?;

    for line in text.lines() {
        if !line.contains(" connected") {
            continue;
        }
        for tok in line.split_whitespace() {
            let Some((res, _)) = tok.split_once('+') else {
                continue;
            };
            let Some((w, h)) = res.split_once('x') else {
                continue;
            };
            let (Ok(w), Ok(h)) = (w.parse::<i32>(), h.parse::<i32>()) else {
                continue;
            };
            if w <= 0 || h <= 0 {
                continue;
            }

            let mut hz = 60.0;
            for mode in text.lines().filter(|l| {
                l.starts_with(' ') && l.contains('*') && l.contains(&format!("{}x{}", w, h))
            }) {
                let hzs: Vec<f64> = mode
                    .split_whitespace()
                    .skip(1)
                    .filter_map(|t| t.trim_end_matches("*+").parse().ok())
                    .collect();
                if let Some(&first) = hzs.first() {
                    hz = first;
                }
            }

            return Ok(DisplayInfo {
                width: w,
                height: h,
                refresh_hz: hz,
            });
        }
    }

    Err("no display found".into())
}

fn parse_display_override(spec: &str) -> Result<DisplayInfo, Box<dyn std::error::Error>> {
    let (res, hz) = spec.split_once('@').ok_or("expected WIDTHxHEIGHT@HZ")?;
    let (w, h) = res.split_once('x').ok_or("expected WIDTHxHEIGHT@HZ")?;
    let width: i32 = w.parse()?;
    let height: i32 = h.parse()?;
    let refresh_hz: f64 = hz.parse()?;
    if width <= 0 || height <= 0 || refresh_hz <= 0.0 {
        return Err("values must be positive".into());
    }
    Ok(DisplayInfo {
        width,
        height,
        refresh_hz,
    })
}

fn resolve_display() -> Result<DisplayInfo, Box<dyn std::error::Error>> {
    match std::env::var("APP_DISPLAY") {
        Ok(spec) => parse_display_override(&spec),
        Err(_) => detect_display(),
    }
}

#[derive(Debug, Deserialize, Clone)]
struct PointerPayload {
    t: String,
    x: f64,
    y: f64,
    tx: f64,
    ty: f64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
enum ClientMessage {
    #[serde(rename = "pointer")]
    Pointer(PointerPayload),
    #[serde(rename = "config")]
    Config { smoothing: bool },
    #[serde(rename = "ping")]
    Ping { id: u64 },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind")]
enum ServerMessage {
    #[serde(rename = "display")]
    Display {
        width: i32,
        height: i32,
        refresh_hz: f64,
    },
    #[serde(rename = "pong")]
    Pong { id: u64 },
}

async fn send_message(socket: &mut WebSocket, msg: &ServerMessage) {
    if let Ok(encoded) = serde_json::to_string(msg) {
        let _ = socket.send(Message::Text(encoded.into())).await;
    }
}

#[derive(Debug, Clone, Copy)]
struct PointState {
    x: f64,
    y: f64,
    tx: f64,
    ty: f64,
    is_in_range: bool,
}

struct InterpolatorState {
    current: Option<PointState>,
    target: Option<PointState>,
    dirty: bool,
    tool_in_proximity: bool,
    smoothing_enabled: bool,
}

impl InterpolatorState {
    fn reset(&mut self) {
        self.current = None;
        self.target = None;
        self.dirty = false;
        self.tool_in_proximity = false;
    }
}

struct InputHub {
    device: Mutex<Option<VirtualDevice>>,
    state: Mutex<InterpolatorState>,
    display: DisplayInfo,
}

struct Hub {
    input: InputHub,
    clients: AtomicUsize,
}

impl Hub {
    fn new(display: DisplayInfo) -> Self {
        Self {
            input: InputHub::new(display),
            clients: AtomicUsize::new(0),
        }
    }

    fn client_joined(&self) {
        if self.clients.fetch_add(1, Ordering::Relaxed) == 0
            && let Err(e) = self.input.attach()
        {
            eprintln!("failed to attach input device: {e}");
        }
    }

    fn client_left(&self) {
        if self.clients.fetch_sub(1, Ordering::Relaxed) == 1 {
            self.input.release();
        }
    }
}

fn abs_event(axis: AbsoluteAxisCode, value: i32) -> InputEvent {
    InputEvent::new(evdev::EventType::ABSOLUTE.0, axis.0, value)
}

fn key_event(key: KeyCode, pressed: bool) -> InputEvent {
    InputEvent::new(evdev::EventType::KEY.0, key.0, pressed as i32)
}

fn syn_report() -> InputEvent {
    InputEvent::new(
        evdev::EventType::SYNCHRONIZATION.0,
        SynchronizationCode::SYN_REPORT.0,
        0,
    )
}

impl InputHub {
    fn new(display: DisplayInfo) -> Self {
        Self {
            device: Mutex::new(None),
            state: Mutex::new(InterpolatorState {
                current: None,
                target: None,
                dirty: false,
                tool_in_proximity: false,
                smoothing_enabled: true,
            }),
            display,
        }
    }

    fn attach(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut keys = AttributeSet::<KeyCode>::new();
        keys.insert(KeyCode::BTN_TOOL_PEN);

        let mut props = AttributeSet::<PropType>::new();
        props.insert(PropType::DIRECT);

        let x_setup = UinputAbsSetup::new(
            AbsoluteAxisCode::ABS_X,
            AbsInfo::new(0, 0, self.display.width, 0, 0, 1),
        );
        let y_setup = UinputAbsSetup::new(
            AbsoluteAxisCode::ABS_Y,
            AbsInfo::new(0, 0, self.display.height, 0, 0, 1),
        );
        let tilt_x_setup = UinputAbsSetup::new(
            AbsoluteAxisCode::ABS_TILT_X,
            AbsInfo::new(0, -MAX_TILT as i32, MAX_TILT as i32, 0, 0, 1),
        );
        let tilt_y_setup = UinputAbsSetup::new(
            AbsoluteAxisCode::ABS_TILT_Y,
            AbsInfo::new(0, -MAX_TILT as i32, MAX_TILT as i32, 0, 0, 1),
        );

        let device = VirtualDevice::builder()?
            .name("iPad Apple Pencil Stylus")
            .input_id(InputId::new(BusType::BUS_VIRTUAL, 0x1234, 0x5678, 1))
            .with_keys(&keys)?
            .with_absolute_axis(&x_setup)?
            .with_absolute_axis(&y_setup)?
            .with_absolute_axis(&tilt_x_setup)?
            .with_absolute_axis(&tilt_y_setup)?
            .with_properties(&props)?
            .build()?;

        let mut dev = self.device.lock().unwrap();
        *dev = Some(device);
        let mut st = self.state.lock().unwrap();
        st.reset();
        Ok(())
    }

    fn release(&self) {
        let mut dev = self.device.lock().unwrap();
        if let Some(device) = dev.as_mut() {
            let up = vec![key_event(KeyCode::BTN_TOOL_PEN, false), syn_report()];
            let _ = device.emit(&up);
        }
        *dev = None;
        self.state.lock().unwrap().reset();
    }

    fn set_smoothing(&self, enabled: bool) {
        self.state.lock().unwrap().smoothing_enabled = enabled;
    }

    fn push_target(&self, payload: &PointerPayload) {
        let is_in_range = payload.t != "up";

        let new_point = PointState {
            x: payload.x.clamp(0.0, 1.0),
            y: payload.y.clamp(0.0, 1.0),
            tx: payload.tx.clamp(-MAX_TILT, MAX_TILT),
            ty: payload.ty.clamp(-MAX_TILT, MAX_TILT),
            is_in_range,
        };

        let mut st = self.state.lock().unwrap();

        if !st.smoothing_enabled || payload.t == "down" || st.current.is_none() {
            st.current = Some(new_point);
        }
        st.target = Some(new_point);
        st.dirty = true;
    }

    fn tick(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut guard = self.device.lock().unwrap();
        let device = match guard.as_mut() {
            Some(d) => d,
            None => return Ok(()),
        };

        let (pt, should_emit, in_proximity_changed) = {
            let mut st = self.state.lock().unwrap();

            let target = match st.target {
                Some(t) => t,
                None => return Ok(()),
            };

            let current = st.current.unwrap_or(target);

            let dx = target.x - current.x;
            let dy = target.y - current.y;
            let dist_sq = dx * dx + dy * dy;

            let next = if !target.is_in_range {
                st.target = None;
                st.current = None;
                target
            } else if !st.smoothing_enabled || dist_sq < 0.0000001 {
                st.current = Some(target);
                target
            } else {
                let blended = PointState {
                    x: current.x + dx * SMOOTH_ALPHA,
                    y: current.y + dy * SMOOTH_ALPHA,
                    tx: current.tx + (target.tx - current.tx) * SMOOTH_ALPHA,
                    ty: current.ty + (target.ty - current.ty) * SMOOTH_ALPHA,
                    is_in_range: target.is_in_range,
                };
                st.current = Some(blended);
                blended
            };

            let should_emit = st.dirty || dist_sq >= 0.0000001;
            st.dirty = false;

            let in_proximity_changed = if st.tool_in_proximity != next.is_in_range {
                st.tool_in_proximity = next.is_in_range;
                Some(next.is_in_range)
            } else {
                None
            };

            (next, should_emit, in_proximity_changed)
        };

        if !should_emit && in_proximity_changed.is_none() {
            return Ok(());
        }

        let abs_x = (pt.x * self.display.width as f64).round() as i32;
        let abs_y = (pt.y * self.display.height as f64).round() as i32;
        let tilt_x = pt.tx.round() as i32;
        let tilt_y = pt.ty.round() as i32;

        let mut events = vec![
            abs_event(AbsoluteAxisCode::ABS_X, abs_x),
            abs_event(AbsoluteAxisCode::ABS_Y, abs_y),
            abs_event(AbsoluteAxisCode::ABS_TILT_X, tilt_x),
            abs_event(AbsoluteAxisCode::ABS_TILT_Y, tilt_y),
        ];

        if let Some(in_range) = in_proximity_changed {
            events.push(key_event(KeyCode::BTN_TOOL_PEN, in_range));
        }

        events.push(syn_report());

        device.emit(&events)?;
        Ok(())
    }
}

async fn ws_handler(ws: WebSocketUpgrade, hub: Arc<Hub>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, hub))
}

async fn handle_socket(mut socket: WebSocket, hub: Arc<Hub>) {
    hub.client_joined();

    let display = hub.input.display;
    let _ = send_message(
        &mut socket,
        &ServerMessage::Display {
            width: display.width,
            height: display.height,
            refresh_hz: display.refresh_hz,
        },
    )
    .await;

    while let Some(Ok(message)) = socket.recv().await {
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) else {
            continue;
        };

        match client_msg {
            ClientMessage::Pointer(payload) => hub.input.push_target(&payload),
            ClientMessage::Config { smoothing } => hub.input.set_smoothing(smoothing),
            ClientMessage::Ping { id } => {
                let _ = send_message(&mut socket, &ServerMessage::Pong { id }).await;
            }
        }
    }

    hub.client_left();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let display = resolve_display()?;
    println!(
        "display: {}x{} @ {:.0} Hz",
        display.width, display.height, display.refresh_hz
    );

    let tick_hz = display.refresh_hz;
    let hub = Arc::new(Hub::new(display));

    let ticker_hub = Arc::clone(&hub);
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs_f64(1.0 / tick_hz));
        loop {
            ticker.tick().await;
            let _ = ticker_hub.input.tick();
        }
    });

    let app = Router::new()
        .route(
            "/ws",
            get({
                let hub = Arc::clone(&hub);
                move |ws| ws_handler(ws, Arc::clone(&hub))
            }),
        )
        .fallback_service(ServeDir::new("static"));

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    println!("listening on http://0.0.0.0:8080");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
