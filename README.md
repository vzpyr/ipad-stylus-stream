# ipad-stylus-stream

Proof of concept iPad stylus to Linux uinput bridge

## Features

- Maps iPad pen input to a virtual uinput device
- Resizable canvas with dynamic display sizing
- Wireless over WebSocket (axum server)
- Position, pressure and tilt passthrough
- Optional smoothing and rotation

## How to use

Requires: Rust/Cargo

1. Build and start server:

```bash
cargo run --release
```

2. Open `http://<ip>:8080` in Safari on your iPad

## License

MIT
