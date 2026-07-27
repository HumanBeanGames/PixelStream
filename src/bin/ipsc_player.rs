//! Standalone local player for IPSC palette stream debugging.

use std::{
    env, fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
};

const ADDR: &str = "127.0.0.1:8090";

fn main() {
    let Some(path) = env::args().nth(1).map(PathBuf::from) else {
        eprintln!("Usage: cargo run --bin ipsc_player -- <recording.ipsc> [fps]");
        return;
    };

    let fps = env::args()
        .nth(2)
        .and_then(|fps| fps.parse::<u32>().ok())
        .filter(|fps| (1..=60).contains(fps))
        .unwrap_or(5);

    let recording = match fs::read(&path).and_then(validate_recording) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("Could not load {}: {err}", path.display());
            return;
        }
    };

    let listener = match TcpListener::bind(ADDR) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("Could not bind IPSC player at http://{ADDR}: {err}");
            return;
        }
    };

    eprintln!("IPSC player: http://{ADDR}");
    eprintln!("Recording: {}", path.display());

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => handle_request(stream, &recording, fps),
            Err(err) => eprintln!("IPSC player connection failed: {err}"),
        }
    }
}

fn validate_recording(bytes: Vec<u8>) -> std::io::Result<Vec<u8>> {
    if bytes.len() < 12 || &bytes[0..4] != b"IPSC" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "recording does not start with an IPSC header",
        ));
    }
    Ok(bytes)
}

fn handle_request(mut stream: TcpStream, recording: &[u8], fps: u32) {
    let mut request = [0; 1024];
    let bytes_read = stream.read(&mut request).unwrap_or(0);
    let request = String::from_utf8_lossy(&request[..bytes_read]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    if path.starts_with("/recording.ipsc") {
        serve_recording(stream, recording);
    } else {
        serve_page(stream, fps);
    }
}

fn serve_page(mut stream: TcpStream, fps: u32) {
    let body = player_html(fps);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn serve_recording(mut stream: TcpStream, recording: &[u8]) {
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        recording.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(recording);
}

fn player_html(fps: u32) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>IPSC Player</title>
  <style>
    :root {{ color-scheme: dark; font-family: Arial, sans-serif; background: #111318; color: #eef3f8; }}
    body {{ margin: 0; min-height: 100vh; display: grid; grid-template-rows: auto 1fr; }}
    header {{ padding: 12px 16px; background: #1b2029; border-bottom: 1px solid #303847; display: flex; gap: 14px; align-items: center; flex-wrap: wrap; }}
    main {{ display: grid; place-items: center; padding: 16px; }}
    canvas {{ width: min(100%, 960px); height: auto; max-height: calc(100vh - 96px); image-rendering: pixelated; image-rendering: crisp-edges; background: #050608; border: 1px solid #303847; }}
    button {{ background: #273449; color: #eef3f8; border: 1px solid #647280; padding: 6px 10px; }}
    span {{ color: #b8c5d6; }}
  </style>
</head>
<body>
  <header>
    <strong>IPSC Player</strong>
    <button id="toggle">pause</button>
    <span id="status">loading</span>
  </header>
  <main>
    <canvas id="screen" width="128" height="128"></canvas>
  </main>
  <script>
    const FPS = {fps};
    const canvas = document.getElementById("screen");
    const ctx = canvas.getContext("2d");
    const status = document.getElementById("status");
    const toggle = document.getElementById("toggle");
    ctx.imageSmoothingEnabled = false;

    let width = 0;
    let height = 0;
    let tileSize = 8;
    let palette = [];
    let frames = [];
    let frameIndex = 0;
    let playing = true;
    let framebuffer = new Uint8Array(0);
    let image = ctx.createImageData(1, 1);

    function readU16LE(bytes, offset) {{
      return bytes[offset] | (bytes[offset + 1] << 8);
    }}

    function readU32LE(bytes, offset) {{
      return bytes[offset] | (bytes[offset + 1] << 8) | (bytes[offset + 2] << 16) | (bytes[offset + 3] << 24);
    }}

    function parseRecording(bytes) {{
      if (bytes[0] !== 0x49 || bytes[1] !== 0x50 || bytes[2] !== 0x53 || bytes[3] !== 0x43) {{
        throw new Error("Not an IPSC recording");
      }}

      width = readU16LE(bytes, 5);
      height = readU16LE(bytes, 7);
      tileSize = bytes[9];
      const paletteLength = readU16LE(bytes, 10);
      let cursor = 12;

      palette = [];
      for (let i = 0; i < paletteLength; i++) {{
        palette.push([bytes[cursor], bytes[cursor + 1], bytes[cursor + 2], bytes[cursor + 3]]);
        cursor += 4;
      }}

      frames = [];
      while (cursor + 4 <= bytes.length) {{
        const frameLength = readU32LE(bytes, cursor);
        cursor += 4;
        if (cursor + frameLength > bytes.length) break;
        frames.push(bytes.slice(cursor, cursor + frameLength));
        cursor += frameLength;
      }}

      canvas.width = width;
      canvas.height = height;
      framebuffer = new Uint8Array(width * height);
      image = ctx.createImageData(width, height);
    }}

    function drawFrame(frame) {{
      if (frame.length < 9) return;
      const frameType = frame[0];
      const payloadLength = readU32LE(frame, 5);
      const payload = frame.slice(9, 9 + payloadLength);

      if (frameType === 0) {{
        framebuffer.set(payload.slice(0, framebuffer.length));
      }} else if (frameType === 1) {{
        applyDelta(payload);
      }} else {{
        return;
      }}
      renderFramebuffer();
    }}

    function applyDelta(payload) {{
      const tilesX = width / tileSize;
      const tilesY = height / tileSize;
      const tileCount = tilesX * tilesY;
      const maskLength = Math.ceil(tileCount / 8);
      let cursor = maskLength;

      for (let tileIndex = 0; tileIndex < tileCount; tileIndex++) {{
        if ((payload[Math.floor(tileIndex / 8)] & (1 << (tileIndex % 8))) === 0) continue;
        const tileX = tileIndex % tilesX;
        const tileY = Math.floor(tileIndex / tilesX);
        const decoded = decodeTile(payload, cursor, tileX, tileY);
        cursor = decoded.cursor;
        writeTile(tileX, tileY, decoded.tile);
      }}
    }}

    function decodeTile(bytes, cursor, tileX, tileY) {{
      const mode = bytes[cursor++];
      const tile = readTile(tileX, tileY);

      if (mode === 0) {{
        tile.set(bytes.slice(cursor, cursor + 64));
        cursor += 64;
      }} else if (mode === 1) {{
        tile.fill(bytes[cursor++]);
      }} else if (mode === 2) {{
        cursor = decodeRle(tile, bytes, cursor, false);
      }} else if (mode === 3) {{
        const spanCount = bytes[cursor++];
        let out = 0;
        for (let span = 0; span < spanCount; span++) {{
          out += bytes[cursor++];
          const len = bytes[cursor++];
          tile.set(bytes.slice(cursor, cursor + len), out);
          cursor += len;
          out += len;
        }}
      }} else if (mode === 4) {{
        cursor = decodeRle(tile, bytes, cursor, true);
      }}

      return {{ tile, cursor }};
    }}

    function decodeRle(tile, bytes, cursor, xor) {{
      const runCount = bytes[cursor++];
      let out = 0;
      for (let run = 0; run < runCount; run++) {{
        const value = bytes[cursor++];
        const len = bytes[cursor++];
        for (let i = 0; i < len; i++) {{
          tile[out] = xor ? tile[out] ^ value : value;
          out++;
        }}
      }}
      return cursor;
    }}

    function readTile(tileX, tileY) {{
      const tile = new Uint8Array(64);
      for (let y = 0; y < tileSize; y++) {{
        for (let x = 0; x < tileSize; x++) {{
          tile[y * tileSize + x] = framebuffer[(tileY * tileSize + y) * width + tileX * tileSize + x];
        }}
      }}
      return tile;
    }}

    function writeTile(tileX, tileY, tile) {{
      for (let y = 0; y < tileSize; y++) {{
        for (let x = 0; x < tileSize; x++) {{
          framebuffer[(tileY * tileSize + y) * width + tileX * tileSize + x] = tile[y * tileSize + x];
        }}
      }}
    }}

    function renderFramebuffer() {{
      for (let i = 0; i < framebuffer.length; i++) {{
        const color = palette[framebuffer[i]] || palette[0] || [0, 0, 0, 255];
        const out = i * 4;
        image.data[out] = color[0];
        image.data[out + 1] = color[1];
        image.data[out + 2] = color[2];
        image.data[out + 3] = color[3];
      }}
      ctx.putImageData(image, 0, 0);
    }}

    function tick() {{
      if (!playing || frames.length === 0) return;
      drawFrame(frames[frameIndex]);
      frameIndex = (frameIndex + 1) % frames.length;
      status.textContent = `${{width}}x${{height}} - frame ${{frameIndex + 1}} / ${{frames.length}}`;
    }}

    toggle.addEventListener("click", () => {{
      playing = !playing;
      toggle.textContent = playing ? "pause" : "play";
    }});

    fetch("/recording.ipsc", {{ cache: "no-store" }})
      .then(response => response.arrayBuffer())
      .then(buffer => {{
        parseRecording(new Uint8Array(buffer));
        status.textContent = `${{width}}x${{height}} - ${{frames.length}} frames`;
        tick();
        setInterval(tick, 1000 / FPS);
      }})
      .catch(error => {{
        console.error(error);
        status.textContent = error.toString();
      }});
  </script>
</body>
</html>"#
    )
}
