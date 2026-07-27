//! Local HTTP launcher for the combined IPSC palette/converter lab.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
};

const ADDR: &str = "127.0.0.1:8092";
const OKLCH_MAX_CHROMA: f32 = 0.2576833;

fn main() {
    let listener = match TcpListener::bind(ADDR) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("Could not bind IPSC lab at http://{ADDR}: {err}");
            return;
        }
    };

    eprintln!("IPSC lab: http://{ADDR}");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                thread::spawn(move || handle_request(stream));
            }
            Err(err) => eprintln!("IPSC lab connection failed: {err}"),
        }
    }
}

fn handle_request(mut stream: TcpStream) {
    let mut request_bytes = Vec::new();
    let mut buffer = [0; 4096];
    let mut header_end = None;

    while header_end.is_none() {
        let bytes_read = stream.read(&mut buffer).unwrap_or(0);
        if bytes_read == 0 {
            break;
        }
        request_bytes.extend_from_slice(&buffer[..bytes_read]);
        header_end = request_bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|position| position + 4);
    }

    let header_end = header_end.unwrap_or(request_bytes.len());
    let headers = String::from_utf8_lossy(&request_bytes[..header_end]);
    let content_length = headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find_map(|(name, value)| {
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);

    while request_bytes.len().saturating_sub(header_end) < content_length {
        let bytes_read = stream.read(&mut buffer).unwrap_or(0);
        if bytes_read == 0 {
            break;
        }
        request_bytes.extend_from_slice(&buffer[..bytes_read]);
    }

    let request = String::from_utf8_lossy(&request_bytes[..header_end]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    match path {
        "/" => serve_html(stream, &lab_shell_html()),
        "/palette" => serve_html(stream, &palette_html()),
        "/converter" => serve_html(stream, &converter_html()),
        _ => serve_not_found(stream),
    }
}

fn serve_html(stream: TcpStream, body: &str) {
    serve_text(stream, body, "text/html; charset=utf-8");
}

fn serve_text(mut stream: TcpStream, body: &str, content_type: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn serve_not_found(mut stream: TcpStream) {
    let body = "not found";
    let response = format!(
        "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn lab_shell_html() -> String {
    r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>IPSC Lab</title>
  <style>
    :root { color-scheme: dark; font-family: Arial, sans-serif; background: #101217; color: #edf2f7; }
    body { margin: 0; min-height: 100vh; display: grid; grid-template-rows: auto 1fr; }
    nav { display: flex; gap: 8px; padding: 10px 12px; border-bottom: 1px solid #2d3441; background: #171b23; }
    button { border: 1px solid #43516a; border-radius: 5px; background: #263245; color: #f0f5ff; padding: 8px 12px; font: inherit; cursor: pointer; }
    button.active { background: #d8e8ff; color: #06101f; border-color: #d8e8ff; font-weight: 700; }
    iframe { width: 100%; height: 100%; border: 0; display: none; }
    iframe.active { display: block; }
  </style>
</head>
<body>
  <nav>
    <button id="paletteTab" class="active" type="button">Palette</button>
    <button id="converterTab" type="button">Converter</button>
  </nav>
  <iframe id="paletteFrame" class="active" src="/palette"></iframe>
  <iframe id="converterFrame" src="/converter"></iframe>
  <script>
    const tabs = [
      [document.getElementById("paletteTab"), document.getElementById("paletteFrame")],
      [document.getElementById("converterTab"), document.getElementById("converterFrame")],
    ];
    for (const [button, frame] of tabs) {
      button.addEventListener("click", () => {
        for (const [otherButton, otherFrame] of tabs) {
          otherButton.classList.toggle("active", otherButton === button);
          otherFrame.classList.toggle("active", otherFrame === frame);
        }
      });
    }
  </script>
</body>
</html>"#
        .to_owned()
}

fn palette_html() -> String {
    extract_raw_html(include_str!("ipsc_palette_lab.rs"), "r##\"", "\"##")
        .replace("__OKLCH_MAX_CHROMA__", &OKLCH_MAX_CHROMA.to_string())
}

fn converter_html() -> String {
    extract_raw_html(include_str!("ipsc_png_converter_lab.rs"), "r#\"", "\"#")
}

fn extract_raw_html(source: &str, start_marker: &str, end_marker: &str) -> String {
    let start = source
        .find(start_marker)
        .map(|index| index + start_marker.len())
        .expect("embedded lab HTML start marker exists");
    let end = source[start..]
        .find(end_marker)
        .map(|index| start + index)
        .expect("embedded lab HTML end marker exists");
    source[start..end].to_owned()
}
