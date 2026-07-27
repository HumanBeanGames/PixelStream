//! Tiny local image viewer used while authoring palette and IPSI assets.

use std::{
    env, fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

const ADDR: &str = "127.0.0.1:8091";

fn main() {
    let path = env::args().nth(1).map(PathBuf::from);
    let Some(path) = path else {
        eprintln!("Usage: cargo run --bin ipsc_image_viewer -- <image.ipsi>");
        return;
    };

    let image = match fs::read(&path).and_then(validate_image) {
        Ok(image) => image,
        Err(err) => {
            eprintln!("Could not load {}: {err}", path.display());
            eprintln!("Usage: cargo run --bin ipsc_image_viewer -- <image.ipsi>");
            return;
        }
    };

    let listener = match TcpListener::bind(ADDR) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("Could not bind IPSI image viewer at http://{ADDR}: {err}");
            return;
        }
    };
    let _ = listener.set_nonblocking(true);

    eprintln!("IPSI image viewer: http://{ADDR}");
    eprintln!("Image: {}", path.display());

    let mut last_request_at = Instant::now();
    let mut served_image = false;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                last_request_at = Instant::now();
                served_image |= handle_request(stream, &image);
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                if served_image && last_request_at.elapsed() > Duration::from_millis(750) {
                    break;
                }
                if !served_image && last_request_at.elapsed() > Duration::from_secs(60) {
                    eprintln!("No image request received after 60 seconds; exiting.");
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(err) => eprintln!("IPSI image viewer connection failed: {err}"),
        }
    }
}

fn validate_image(bytes: Vec<u8>) -> std::io::Result<Vec<u8>> {
    if bytes.len() < 11 || &bytes[0..4] != b"IPSI" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "image does not start with an IPSI header",
        ));
    }

    let width = read_u16_le(&bytes, 5) as usize;
    let height = read_u16_le(&bytes, 7) as usize;
    let palette_len = read_u16_le(&bytes, 9) as usize;
    let expected = 11 + palette_len * 4 + width * height;
    if bytes.len() < expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "image is shorter than the IPSI header declares",
        ));
    }

    Ok(bytes)
}

fn handle_request(mut stream: TcpStream, image: &[u8]) -> bool {
    let mut request = [0; 1024];
    let bytes_read = stream.read(&mut request).unwrap_or(0);
    let request = String::from_utf8_lossy(&request[..bytes_read]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    if path.starts_with("/image.ipsi") {
        serve_image(stream, image);
        true
    } else {
        serve_page(stream);
        false
    }
}

fn serve_page(mut stream: TcpStream) {
    let body = viewer_html();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn serve_image(mut stream: TcpStream, image: &[u8]) {
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        image.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(image);
}

fn viewer_html() -> String {
    r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>IPSI Image Viewer</title>
  <style>
    :root { color-scheme: dark; font-family: Arial, sans-serif; background: #111318; color: #eef3f8; }
    body { margin: 0; min-height: 100vh; display: grid; grid-template-rows: auto 1fr; }
    header { padding: 12px 16px; background: #1b2029; border-bottom: 1px solid #303847; display: flex; gap: 14px; align-items: center; flex-wrap: wrap; }
    main { display: grid; place-items: center; padding: 16px; }
    canvas { width: min(100%, 720px); height: auto; max-height: calc(100vh - 96px); image-rendering: pixelated; image-rendering: crisp-edges; background: #050608; border: 1px solid #303847; }
    span { color: #b8c5d6; }
  </style>
</head>
<body>
  <header>
    <strong>IPSI Image Viewer</strong>
    <span id="status">loading</span>
  </header>
  <main>
    <canvas id="screen" width="16" height="16"></canvas>
  </main>
  <script>
    const canvas = document.getElementById("screen");
    const ctx = canvas.getContext("2d");
    const status = document.getElementById("status");
    ctx.imageSmoothingEnabled = false;

    function readU16LE(bytes, offset) {
      return bytes[offset] | (bytes[offset + 1] << 8);
    }

    function drawImage(bytes) {
      if (bytes[0] !== 0x49 || bytes[1] !== 0x50 || bytes[2] !== 0x53 || bytes[3] !== 0x49) {
        throw new Error("Not an IPSI image");
      }

      const width = readU16LE(bytes, 5);
      const height = readU16LE(bytes, 7);
      const paletteLength = readU16LE(bytes, 9);
      let cursor = 11;
      const palette = [];
      for (let i = 0; i < paletteLength; i++) {
        palette.push([bytes[cursor], bytes[cursor + 1], bytes[cursor + 2], bytes[cursor + 3]]);
        cursor += 4;
      }

      canvas.width = width;
      canvas.height = height;
      const image = ctx.createImageData(width, height);
      for (let i = 0; i < width * height; i++) {
        const color = palette[bytes[cursor + i]] || palette[0] || [0, 0, 0, 255];
        const out = i * 4;
        image.data[out] = color[0];
        image.data[out + 1] = color[1];
        image.data[out + 2] = color[2];
        image.data[out + 3] = color[3];
      }
      ctx.putImageData(image, 0, 0);
      status.textContent = `${width}x${height} - ${paletteLength} colors`;
    }

    fetch("/image.ipsi", { cache: "no-store" })
      .then(response => response.arrayBuffer())
      .then(buffer => drawImage(new Uint8Array(buffer)))
      .catch(error => {
        console.error(error);
        status.textContent = error.toString();
      });
  </script>
</body>
</html>"#
    .to_owned()
}

fn read_u16_le(bytes: &[u8], offset: usize) -> u16 {
    bytes[offset] as u16 | ((bytes[offset + 1] as u16) << 8)
}
