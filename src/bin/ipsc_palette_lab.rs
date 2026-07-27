//! Local HTTP server for the interactive IPSMAP palette lab.

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
            eprintln!("Could not bind IPSC palette lab at http://{ADDR}: {err}");
            return;
        }
    };

    eprintln!("IPSC palette lab: http://{ADDR}");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                thread::spawn(move || handle_request(stream));
            }
            Err(err) => eprintln!("IPSC palette lab connection failed: {err}"),
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

    if path == "/" {
        serve_page(stream);
    } else {
        serve_not_found(stream);
    }
}

fn serve_page(mut stream: TcpStream) {
    let body = palette_lab_html();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",
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

fn palette_lab_html() -> String {
    r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>IPSC Palette Lab</title>
  <style>
    :root {
      color-scheme: dark;
      font-family: Arial, sans-serif;
      background: #101217;
      color: #edf2f7;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      height: 100vh;
      overflow: hidden;
      display: grid;
      grid-template-columns: minmax(320px, 390px) minmax(0, 1fr) minmax(320px, 420px);
    }
    aside {
      border-right: 1px solid #2d3441;
      background: #171b23;
      padding: 16px;
      overflow-y: auto;
      min-height: 0;
    }
    main {
      display: grid;
      min-width: 0;
      min-height: 0;
    }
    .debug-panel {
      border-left: 1px solid #2d3441;
      background: #131720;
      padding: 16px;
      display: grid;
      grid-template-rows: auto minmax(0, 1fr);
      gap: 12px;
      min-height: 0;
      overflow: hidden;
    }
    .debug-panel strong {
      font-size: 14px;
      color: #edf2f7;
    }
    h1 {
      font-size: 18px;
      margin: 0 0 16px;
    }
    fieldset {
      border: 1px solid #303847;
      border-radius: 6px;
      margin: 0 0 14px;
      padding: 12px;
    }
    legend {
      padding: 0 6px;
      color: #c4cede;
      font-weight: 700;
    }
    label {
      display: grid;
      grid-template-columns: 1fr 96px;
      gap: 10px;
      align-items: center;
      margin: 9px 0;
      color: #cbd5e1;
      font-size: 14px;
    }
    .bias-label {
      grid-template-columns: 92px minmax(0, 1fr) 76px;
    }
    input {
      width: 100%;
      background: #0d1016;
      color: #f8fafc;
      border: 1px solid #3a4353;
      border-radius: 4px;
      padding: 7px 8px;
      font: inherit;
    }
    input[type="range"] {
      padding: 0;
      accent-color: #87bfff;
    }
    .range-number {
      padding: 5px 6px;
      font-size: 13px;
      text-align: right;
    }
    button, a.button {
      appearance: none;
      border: 1px solid #4a5668;
      border-radius: 5px;
      background: #263142;
      color: #f8fafc;
      padding: 8px 10px;
      font: inherit;
      text-decoration: none;
      cursor: pointer;
      display: inline-flex;
      align-items: center;
      justify-content: center;
      min-height: 36px;
    }
    button.primary {
      background: #d7e8ff;
      border-color: #d7e8ff;
      color: #101217;
      font-weight: 700;
    }
    button:disabled, a.button[aria-disabled="true"] {
      opacity: 0.45;
      pointer-events: none;
    }
    .actions {
      display: grid;
      grid-template-columns: 86px minmax(120px, 1fr) minmax(0, 1fr);
      gap: 8px;
      margin-top: 12px;
    }
    .actions .primary,
    .actions .download-ipsi {
      grid-column: 1 / -1;
    }
    .actions .progress-wrap {
      min-width: 0;
    }
    .status {
      font-family: Consolas, monospace;
      white-space: pre-wrap;
      color: #cbd5e1;
      line-height: 1.45;
      display: block;
      min-width: 0;
      width: 100%;
      height: 100%;
      overflow: auto;
      scrollbar-gutter: stable;
    }
    .status.ok { color: #b6f5c7; }
    .status.bad { color: #ffb4a8; }
    .progress-wrap {
      display: grid;
      grid-template-columns: minmax(0, 1fr) 38px;
      gap: 6px;
      align-items: center;
    }
    .progress-wrap[hidden] {
      display: none;
    }
    progress {
      width: 100%;
      height: 8px;
      accent-color: #87bfff;
    }
    .progress-text {
      color: #cbd5e1;
      font-size: 12px;
      font-variant-numeric: tabular-nums;
      min-width: 38px;
      text-align: right;
    }
    .stage {
      display: grid;
      align-content: start;
      justify-items: center;
      gap: 18px;
      overflow: auto;
      padding: 18px;
      background: #0b0d12;
      min-height: 0;
    }
    .preview {
      width: min(100%, 1100px);
      display: grid;
      gap: 8px;
    }
    .preview h2 {
      margin: 0;
      font-size: 14px;
      color: #cbd5e1;
    }
    canvas {
      image-rendering: pixelated;
      image-rendering: crisp-edges;
      background: #000;
      border: 1px solid #303847;
      width: 100%;
      height: auto;
    }
    @media (max-width: 860px) {
      body { grid-template-columns: 1fr; }
      body { overflow: auto; height: auto; }
      aside { border-right: 0; border-bottom: 1px solid #2d3441; max-height: none; }
      .debug-panel { border-left: 0; border-top: 1px solid #2d3441; min-height: 280px; }
      .status { width: 100%; }
    }
  </style>
</head>
<body>
  <aside>
    <h1>IPSC Palette Lab</h1>
    <form id="form">
      <fieldset>
        <legend>Chroma</legend>
        <label>min <input id="chromaMin" type="number" step="0.001" value="0"></label>
        <label>max <input id="chromaMax" type="number" step="0.001" value="1"></label>
        <label>divisions <input id="chromaDivisions" type="number" min="1" step="1" value="4"></label>
      </fieldset>
      <fieldset>
        <legend>Value</legend>
        <label>min <input id="valueMin" type="number" step="0.001" value="0"></label>
        <label>max <input id="valueMax" type="number" step="0.001" value="1"></label>
        <label>divisions <input id="valueDivisions" type="number" min="1" step="1" value="16"></label>
        <label>add black <input id="addBlack" type="checkbox"></label>
        <label>add white <input id="addWhite" type="checkbox"></label>
      </fieldset>
      <fieldset>
        <legend>Hue</legend>
        <label>min <input id="hueMin" type="number" step="0.1" value="0"></label>
        <label>max <input id="hueMax" type="number" step="0.1" value="360"></label>
        <label>divisions <input id="hueDivisions" type="number" min="1" step="1" value="20"></label>
        <label>offset <input id="hueOffset" type="number" step="any" value="0"></label>
      </fieldset>
      <fieldset>
        <legend>Nearest Palette Priorities</legend>
        <label class="bias-label">Oklab L <input id="biasLightness" type="range" min="0" max="1" step="0.001" value="0.333"><input id="biasLightnessValue" class="range-number" type="number" min="0" max="1" step="0.001" value="0.333"></label>
        <label class="bias-label">Oklab a <input id="biasChroma" type="range" min="0" max="1" step="0.001" value="0.333"><input id="biasChromaValue" class="range-number" type="number" min="0" max="1" step="0.001" value="0.333"></label>
        <label class="bias-label">Oklab b <input id="biasHue" type="range" min="0" max="1" step="0.001" value="0.334"><input id="biasHueValue" class="range-number" type="number" min="0" max="1" step="0.001" value="0.334"></label>
      </fieldset>
      <fieldset>
        <legend>Input Biases Before Matching</legend>
        <label class="bias-label">value mult <input id="offsetLightnessMultiply" type="range" min="-1" max="1" step="0.001" value="0"><input id="offsetLightnessMultiplyValue" class="range-number" type="number" min="-1" max="1" step="0.001" value="0.000"></label>
        <label class="bias-label">value add <input id="offsetLightnessAdd" type="range" min="-1" max="1" step="0.001" value="0"><input id="offsetLightnessAddValue" class="range-number" type="number" min="-1" max="1" step="0.001" value="0.000"></label>
        <label class="bias-label">chroma mult <input id="offsetChromaMultiply" type="range" min="-1" max="1" step="0.001" value="0"><input id="offsetChromaMultiplyValue" class="range-number" type="number" min="-1" max="1" step="0.001" value="0.000"></label>
        <label class="bias-label">chroma add <input id="offsetChromaAdd" type="range" min="-1" max="1" step="0.001" value="0"><input id="offsetChromaAddValue" class="range-number" type="number" min="-1" max="1" step="0.001" value="0.000"></label>
        <label class="bias-label">hue add <input id="offsetHueAdd" type="range" min="-1" max="1" step="0.001" value="0"><input id="offsetHueAddValue" class="range-number" type="number" min="-1" max="1" step="0.001" value="0.000"></label>
      </fieldset>
      <fieldset>
        <legend>Neutral Protection</legend>
        <label class="bias-label">grey threshold <input id="greyChromaThreshold" type="range" min="0" max="1" step="0.001" value="0.001"><input id="greyChromaThresholdValue" class="range-number" type="number" min="0" max="1" step="0.001" value="0.001"></label>
      </fieldset>
      <fieldset>
        <legend>Export</legend>
        <label>filename <input id="filenameBase" type="text" value="palette"></label>
      </fieldset>
      <div class="actions">
        <button class="primary" type="submit">Generate</button>
        <a class="button download-ipsi" id="downloadIpsi" aria-disabled="true">palette.ipsi</a>
        <button class="secondary" id="bakeButton" type="button" disabled>Bake</button>
        <a class="button" id="downloadMap" aria-disabled="true">palette.ipsmap</a>
        <div id="bakeProgressWrap" class="progress-wrap" hidden>
          <progress id="bakeProgress" max="100" value="0"></progress>
          <span id="bakeProgressText" class="progress-text">0%</span>
        </div>
      </div>
    </form>
  </aside>
  <main>
    <section class="stage">
      <div class="preview">
        <h2>OKLCH strict-gamut palette</h2>
        <canvas id="canvas" width="1" height="1"></canvas>
      </div>
      <div class="preview">
        <h2>sRGB comparison palette</h2>
        <canvas id="srgbCanvas" width="1" height="1"></canvas>
      </div>
      <div class="preview">
        <h2>sRGB rounded to nearest OKLCH palette colour</h2>
        <canvas id="roundedSrgbCanvas" width="1" height="1"></canvas>
      </div>
    </section>
  </main>
  <section class="debug-panel">
    <strong>Palette Preview</strong>
    <span id="status" class="status">ready</span>
  </section>
  <script>
    const form = document.getElementById("form");
    const canvas = document.getElementById("canvas");
    const ctx = canvas.getContext("2d");
    const srgbCanvas = document.getElementById("srgbCanvas");
    const srgbCtx = srgbCanvas.getContext("2d");
    const roundedSrgbCanvas = document.getElementById("roundedSrgbCanvas");
    const roundedSrgbCtx = roundedSrgbCanvas.getContext("2d");
    const status = document.getElementById("status");
    const bakeProgressWrap = document.getElementById("bakeProgressWrap");
    const bakeProgress = document.getElementById("bakeProgress");
    const bakeProgressText = document.getElementById("bakeProgressText");
    const bakeButton = document.getElementById("bakeButton");
    const downloadIpsi = document.getElementById("downloadIpsi");
    const downloadMap = document.getElementById("downloadMap");
    const biasInputs = [
      document.getElementById("biasLightness"),
      document.getElementById("biasChroma"),
      document.getElementById("biasHue"),
    ];
    const offsetInputs = [
      document.getElementById("offsetLightnessMultiply"),
      document.getElementById("offsetLightnessAdd"),
      document.getElementById("offsetChromaMultiply"),
      document.getElementById("offsetChromaAdd"),
      document.getElementById("greyChromaThreshold"),
      document.getElementById("offsetHueAdd"),
    ];
    const sliderPairs = [
      ["biasLightness", "biasLightnessValue", 0, 1],
      ["biasChroma", "biasChromaValue", 0, 1],
      ["biasHue", "biasHueValue", 0, 1],
      ["offsetLightnessMultiply", "offsetLightnessMultiplyValue", -1, 1],
      ["offsetLightnessAdd", "offsetLightnessAddValue", -1, 1],
      ["offsetChromaMultiply", "offsetChromaMultiplyValue", -1, 1],
      ["offsetChromaAdd", "offsetChromaAddValue", -1, 1],
      ["greyChromaThreshold", "greyChromaThresholdValue", 0, 1],
      ["offsetHueAdd", "offsetHueAddValue", -1, 1],
    ];
    const hueOffsetZeroShift = -11;
    ctx.imageSmoothingEnabled = false;
    srgbCtx.imageSmoothingEnabled = false;
    roundedSrgbCtx.imageSmoothingEnabled = false;

    let ipsiUrl = null;
    let mapUrl = null;
    let currentPaletteArtifact = null;

    function numberValue(id) {
      const input = document.getElementById(id);
      const value = Number(input.value);
      if (!Number.isFinite(value)) {
        throw new Error(`${id} must be a number`);
      }
      return value;
    }

    function checkedValue(id) {
      return document.getElementById(id).checked;
    }

    function filenameBase() {
      const raw = document.getElementById("filenameBase").value.trim() || "palette";
      return raw
        .replace(/\.[^.\\\/]+$/, "")
        .replace(/[^A-Za-z0-9._-]+/g, "_")
        .replace(/^_+|_+$/g, "") || "palette";
    }

    function biasValues() {
      return {
        lightness: Number(document.getElementById("biasLightness").value),
        chroma: Number(document.getElementById("biasChroma").value),
        hue: Number(document.getElementById("biasHue").value),
      };
    }

    function offsetValues() {
      return {
        lightnessMultiply: Number(document.getElementById("offsetLightnessMultiply").value),
        lightnessAdd: Number(document.getElementById("offsetLightnessAdd").value),
        chromaMultiply: Number(document.getElementById("offsetChromaMultiply").value),
        chromaAdd: Number(document.getElementById("offsetChromaAdd").value),
        greyChromaThreshold: Number(document.getElementById("greyChromaThreshold").value),
        hueAdd: Number(document.getElementById("offsetHueAdd").value),
      };
    }

    function updateBiasLabels() {
      document.getElementById("biasLightnessValue").value = Number(document.getElementById("biasLightness").value).toFixed(3);
      document.getElementById("biasChromaValue").value = Number(document.getElementById("biasChroma").value).toFixed(3);
      document.getElementById("biasHueValue").value = Number(document.getElementById("biasHue").value).toFixed(3);
    }

    function updateOffsetLabels() {
      document.getElementById("offsetLightnessMultiplyValue").value = Number(document.getElementById("offsetLightnessMultiply").value).toFixed(3);
      document.getElementById("offsetLightnessAddValue").value = Number(document.getElementById("offsetLightnessAdd").value).toFixed(3);
      document.getElementById("offsetChromaMultiplyValue").value = Number(document.getElementById("offsetChromaMultiply").value).toFixed(3);
      document.getElementById("offsetChromaAddValue").value = Number(document.getElementById("offsetChromaAdd").value).toFixed(3);
      document.getElementById("greyChromaThresholdValue").value = Number(document.getElementById("greyChromaThreshold").value).toFixed(3);
      document.getElementById("offsetHueAddValue").value = Number(document.getElementById("offsetHueAdd").value).toFixed(3);
    }

    function normalizeBias(changedInput) {
      const next = Math.max(0, Math.min(1, Number(changedInput.value)));
      changedInput.value = next.toFixed(3);
      const others = biasInputs.filter(input => input !== changedInput);
      const remaining = 1 - next;
      const otherTotal = others.reduce((sum, input) => sum + Number(input.value), 0);

      if (otherTotal <= 0) {
        const split = remaining / others.length;
        for (const input of others) input.value = split.toFixed(3);
      } else {
        for (const input of others) {
          input.value = (Number(input.value) / otherTotal * remaining).toFixed(3);
        }
      }

      const total = biasInputs.reduce((sum, input) => sum + Number(input.value), 0);
      const correction = 1 - total;
      const correctionTarget = others[others.length - 1] || changedInput;
      correctionTarget.value = Math.max(0, Math.min(1, Number(correctionTarget.value) + correction)).toFixed(3);
      updateBiasLabels();
    }

    function clampSliderValue(value, min, max) {
      if (!Number.isFinite(value)) return min;
      return Math.max(min, Math.min(max, value));
    }

    function markSettingsChanged() {
      revokeDownloads();
      status.className = "status";
      status.textContent = "settings changed; press Generate";
    }

    function integerValue(id) {
      const value = Math.floor(numberValue(id));
      if (value < 1) {
        throw new Error(`${id} must be at least 1`);
      }
      return value;
    }

    function range(min, max, divisions) {
      if (divisions === 1) return [(min + max) * 0.5];
      return Array.from({ length: divisions }, (_, i) => min + (max - min) * i / (divisions - 1));
    }

    function uniqueSortedValues(values) {
      return [...values]
        .sort((a, b) => a - b)
        .filter((value, index, sorted) => index === 0 || Math.abs(value - sorted[index - 1]) > 0.000001);
    }

    function srgbByte(value) {
      value = Math.max(0, Math.min(1, value));
      const srgb = value <= 0.0031308 ? value * 12.92 : 1.055 * Math.pow(value, 1 / 2.4) - 0.055;
      return Math.round(Math.max(0, Math.min(255, srgb * 255)));
    }

    function oklchToLinearSrgbRadians(lightness, chroma, hue) {
      const a = Math.cos(hue) * chroma;
      const b = Math.sin(hue) * chroma;
      const l_ = lightness + 0.39633778 * a + 0.21580376 * b;
      const m_ = lightness - 0.105561346 * a - 0.06385417 * b;
      const s_ = lightness - 0.08948418 * a - 1.2914855 * b;
      const l = l_ * l_ * l_;
      const m = m_ * m_ * m_;
      const s = s_ * s_ * s_;
      return [
        4.0767417 * l - 3.3077116 * m + 0.23096994 * s,
        -1.268438 * l + 2.6097574 * m - 0.34131938 * s,
        -0.0041960863 * l - 0.7034186 * m + 1.7076147 * s,
      ];
    }

    function oklchToLinearSrgb(lightness, chroma, hueDegrees) {
      return oklchToLinearSrgbRadians(lightness, chroma, hueDegrees * Math.PI / 180);
    }

    function srgbToLinear(value) {
      value /= 255;
      return value <= 0.04045 ? value / 12.92 : Math.pow((value + 0.055) / 1.055, 2.4);
    }

    function rgbToOklab(r8, g8, b8) {
      const r = srgbToLinear(r8);
      const g = srgbToLinear(g8);
      const b = srgbToLinear(b8);
      const l = 0.41222146 * r + 0.53633255 * g + 0.051445995 * b;
      const m = 0.2119035 * r + 0.6806995 * g + 0.10739696 * b;
      const s = 0.08830246 * r + 0.28171884 * g + 0.6299787 * b;
      const l_ = Math.cbrt(l);
      const m_ = Math.cbrt(m);
      const s_ = Math.cbrt(s);
      return {
        l: 0.21045426 * l_ + 0.7936178 * m_ - 0.004072047 * s_,
        a: 1.9779985 * l_ - 2.4285922 * m_ + 0.4505937 * s_,
        b: 0.025904037 * l_ + 0.78277177 * m_ - 0.80867577 * s_,
      };
    }

    function oklabToOklch(color) {
      const chroma = Math.hypot(color.a, color.b);
      return {
        l: color.l,
        c: chroma,
        h: chroma <= 0.000001 ? 0 : Math.atan2(color.b, color.a),
      };
    }

    function oklchToOklab(color) {
      return {
        l: color.l,
        a: Math.cos(color.h) * color.c,
        b: Math.sin(color.h) * color.c,
      };
    }

    function clamp(value, min, max) {
      return Math.max(min, Math.min(max, value));
    }

    function smoothstep(edge0, edge1, value) {
      const t = clamp((value - edge0) / (edge1 - edge0), 0, 1);
      return t * t * (3 - 2 * t);
    }

    function deltaEOkDistanceSquared(a, b) {
      const dl = a.l - b.l;
      const da = a.a - b.a;
      const db = a.b - b.b;
      return dl * dl + da * da + db * db;
    }

    function offsetInputOklch(color, offset) {
      const greyChromaThreshold = Math.max(0, Math.min(1, offset.greyChromaThreshold));
      const chromaOffsetWeight = smoothstep(
        greyChromaThreshold,
        greyChromaThreshold + 0.04,
        color.c,
      );
      const adjustedChroma = Math.max(
        0,
        color.c + (color.c * offset.chromaMultiply + offset.chromaAdd) * chromaOffsetWeight,
      );
      const adjusted = {
        l: Math.max(0, Math.min(1, color.l * (1 + offset.lightnessMultiply) + offset.lightnessAdd)),
        c: adjustedChroma,
        h: color.h + offset.hueAdd * Math.PI * 2,
      };
      if (hasInputOffset(offset)) adjusted.c = clampChromaToSrgbGamut(adjusted);
      return adjusted;
    }

    function hasInputOffset(offset) {
      return Math.abs(offset.lightnessMultiply) > 0.000001
        || Math.abs(offset.lightnessAdd) > 0.000001
        || Math.abs(offset.chromaMultiply) > 0.000001
        || Math.abs(offset.chromaAdd) > 0.000001
        || Math.abs(offset.greyChromaThreshold - 0.001) > 0.000001
        || Math.abs(offset.hueAdd) > 0.000001;
    }

    function clampChromaToSrgbGamut(color) {
      if (color.c <= 0 || !Number.isFinite(color.l) || !Number.isFinite(color.c) || !Number.isFinite(color.h)) return 0;
      if (oklchInSrgbGamut(color)) return color.c;

      let low = 0;
      let high = color.c;
      for (let i = 0; i < 16; i++) {
        const mid = (low + high) * 0.5;
        if (oklchInSrgbGamut({ l: color.l, c: mid, h: color.h })) low = mid;
        else high = mid;
      }
      return low;
    }

    function oklchInSrgbGamut(color) {
      return inGamut(oklchToLinearSrgbRadians(color.l, color.c, color.h));
    }

    function inGamut(rgb) {
      const epsilon = 0.000001;
      return rgb.every(value => Number.isFinite(value) && value >= -epsilon && value <= 1 + epsilon);
    }

    function oklchColor(lightness, chroma, hueDegrees) {
      const rgb = oklchToLinearSrgb(lightness, chroma, hueDegrees);
      if (!inGamut(rgb)) return null;
      return [srgbByte(rgb[0]), srgbByte(rgb[1]), srgbByte(rgb[2]), 255];
    }

    function hsvColor(hueDegrees, saturation, value) {
      const hue = ((hueDegrees % 360) + 360) % 360;
      const c = value * saturation;
      const x = c * (1 - Math.abs((hue / 60) % 2 - 1));
      const m = value - c;
      let r = 0;
      let g = 0;
      let b = 0;

      if (hue < 60) [r, g, b] = [c, x, 0];
      else if (hue < 120) [r, g, b] = [x, c, 0];
      else if (hue < 180) [r, g, b] = [0, c, x];
      else if (hue < 240) [r, g, b] = [0, x, c];
      else if (hue < 300) [r, g, b] = [x, 0, c];
      else [r, g, b] = [c, 0, x];

      return [
        Math.round((r + m) * 255),
        Math.round((g + m) * 255),
        Math.round((b + m) * 255),
        255,
      ];
    }

    function srgbHueToOklchHue(hueDegrees) {
      const [r, g, b] = hsvColor(hueDegrees, 1, 1);
      const lab = rgbToOklab(r, g, b);
      return (Math.atan2(lab.b, lab.a) * 180 / Math.PI + 360) % 360;
    }

    function hex(color) {
      if (!isColor(color)) {
        throw new Error(`invalid palette colour in export: ${JSON.stringify(color)}`);
      }
      return "#" + color.slice(0, 3).map(value => value.toString(16).padStart(2, "0").toUpperCase()).join("");
    }

    function isColor(color) {
      return Array.isArray(color)
        && color.length >= 3
        && color.slice(0, 3).every(value => Number.isInteger(value) && value >= 0 && value <= 255);
    }

    function colorCountFor(settings) {
      let count = settings.greys.length;
      for (const hue of settings.oklchHues) {
        for (const chroma of settings.chromas) {
          for (const value of settings.values) {
            if (value <= 0 || value >= 1) continue;
            if (oklchColor(value, chroma, hue)) count++;
          }
        }
      }
      return count;
    }

    function readSettings() {
      const chromaMin = numberValue("chromaMin");
      const chromaMax = numberValue("chromaMax");
      const chromaDivisions = integerValue("chromaDivisions");
      const valueMin = numberValue("valueMin");
      const valueMax = numberValue("valueMax");
      const valueDivisions = integerValue("valueDivisions");
      const addBlack = checkedValue("addBlack");
      const addWhite = checkedValue("addWhite");
      const hueMin = numberValue("hueMin");
      const hueMax = numberValue("hueMax");
      const hueDivisions = integerValue("hueDivisions");
      const hueOffset = numberValue("hueOffset");
      const shiftedHueOffset = hueOffset + hueOffsetZeroShift;

      const oklchMaxChroma = __OKLCH_MAX_CHROMA__;

      if (chromaMax > 1 || chromaMax <= chromaMin) {
        throw new Error("chroma max must be <= 1 and greater than chroma min");
      }
      if (valueMin < 0 || valueMax > 1 || valueMax <= valueMin) {
        throw new Error("value min/max must be a valid 0..1 range");
      }

      const chromaSamples = range(chromaMin, chromaMax, chromaDivisions)
        .filter(value => value > 0);
      const chromas = chromaSamples.map(value => Math.min(1, value) * oklchMaxChroma);
      const values = range(valueMin, valueMax, valueDivisions);
      const greyValues = uniqueSortedValues([
        ...(addBlack ? [0] : []),
        ...values,
        ...(addWhite ? [1] : []),
      ]);
      const greys = greyValues.map(greyColor);
      const hueSpan = hueMax - hueMin;
      const srgbHues = Array.from({ length: hueDivisions }, (_, i) => shiftedHueOffset + hueMin + hueSpan * i / hueDivisions);
      const oklchStart = srgbHueToOklchHue(shiftedHueOffset + hueMin);
      const oklchHues = Array.from({ length: hueDivisions }, (_, i) => oklchStart + hueSpan * i / hueDivisions);

      const srgbSaturations = chromaSamples.map(value => Math.max(0, Math.min(1, value)));
      const srgbValues = greyValues;

      return {
        chromas,
        chromaSamples,
        values,
        greyValues,
        greys,
        srgbHues,
        oklchHues,
        srgbSaturations,
        srgbValues,
        chromaDivisions,
        valueDivisions,
        hueDivisions,
        hueMin,
        hueMax,
        hueOffset,
        shiftedHueOffset,
        addBlack,
        addWhite,
        bias: biasValues(),
        offset: offsetValues(),
      };
    }

    function greyColor(value) {
      if (value <= 0) return [0, 0, 0, 255];
      if (value >= 1) return [255, 255, 255, 255];
      return oklchColor(value, 0, 0);
    }

    function generatePalette(settings) {
      const colors = settings.greys.filter(isColor);
      const indexByCell = new Map();

      for (let hueIndex = 0; hueIndex < settings.oklchHues.length; hueIndex++) {
        const hue = settings.oklchHues[hueIndex];
        for (let chromaIndex = 0; chromaIndex < settings.chromas.length; chromaIndex++) {
          for (let valueIndex = 0; valueIndex < settings.values.length; valueIndex++) {
            if (settings.values[valueIndex] <= 0 || settings.values[valueIndex] >= 1) continue;
            const color = oklchColor(settings.values[valueIndex], settings.chromas[chromaIndex], hue);
            if (color) {
              indexByCell.set(`${hueIndex}:${chromaIndex}:${valueIndex}`, colors.length);
              colors.push(color);
            }
          }
        }
      }

      if (colors.some(color => !isColor(color))) {
        throw new Error("palette generation produced an invalid colour");
      }

      return { colors, indexByCell };
    }

    function draw(settings, palette) {
      const width = settings.oklchHues.length * settings.chromas.length + 1;
      const height = settings.greyValues.length;
      const pixels = new Uint8Array(width * height);

      for (let hueIndex = 0; hueIndex < settings.oklchHues.length; hueIndex++) {
        const flipChroma = hueIndex % 2 === 1;
        for (let visualChroma = 0; visualChroma < settings.chromas.length; visualChroma++) {
          const chromaIndex = flipChroma ? visualChroma : settings.chromas.length - 1 - visualChroma;
          for (let valueIndex = 0; valueIndex < settings.values.length; valueIndex++) {
            if (settings.values[valueIndex] <= 0 || settings.values[valueIndex] >= 1) continue;
            const paletteIndex = palette.indexByCell.get(`${hueIndex}:${chromaIndex}:${valueIndex}`);
            if (paletteIndex === undefined) continue;
            const x = hueIndex * settings.chromas.length + visualChroma;
            const y = settings.greyValues.length - 1 - greyRowIndex(settings, settings.values[valueIndex]);
            pixels[y * width + x] = paletteIndex;
          }
        }
      }

      const greyX = width - 1;
      for (let valueIndex = 0; valueIndex < settings.greyValues.length; valueIndex++) {
        const y = settings.greyValues.length - 1 - valueIndex;
        pixels[y * width + greyX] = valueIndex;
      }

      canvas.width = width;
      canvas.height = height;
      const image = ctx.createImageData(width, height);
      for (let i = 0; i < pixels.length; i++) {
        const color = palette.colors[pixels[i]] || [0, 0, 0, 255];
        image.data[i * 4] = color[0];
        image.data[i * 4 + 1] = color[1];
        image.data[i * 4 + 2] = color[2];
        image.data[i * 4 + 3] = color[3];
      }
      ctx.putImageData(image, 0, 0);
      return { width, height, pixels };
    }

    function greyRowIndex(settings, value) {
      return settings.greyValues.findIndex(greyValue => Math.abs(greyValue - value) <= 0.000001);
    }

    function drawDirect(canvasElement, context, width, height, pixelColors) {
      canvasElement.width = width;
      canvasElement.height = height;
      const image = context.createImageData(width, height);
      for (let i = 0; i < pixelColors.length; i++) {
        const color = pixelColors[i] || [0, 0, 0, 255];
        image.data[i * 4] = color[0];
        image.data[i * 4 + 1] = color[1];
        image.data[i * 4 + 2] = color[2];
        image.data[i * 4 + 3] = color[3];
      }
      context.putImageData(image, 0, 0);
    }

    function makeSrgbComparisonPixels(settings) {
      const width = settings.srgbHues.length * settings.srgbSaturations.length + 1;
      const height = settings.srgbValues.length;
      const pixelColors = Array.from({ length: width * height }, () => [0, 0, 0, 255]);

      for (let hueIndex = 0; hueIndex < settings.srgbHues.length; hueIndex++) {
        const flipSaturation = hueIndex % 2 === 1;
        for (let visualSaturation = 0; visualSaturation < settings.srgbSaturations.length; visualSaturation++) {
          const saturationIndex = flipSaturation
            ? visualSaturation
            : settings.srgbSaturations.length - 1 - visualSaturation;
          const saturation = settings.srgbSaturations[saturationIndex];

          for (let valueIndex = 0; valueIndex < settings.srgbValues.length; valueIndex++) {
            const value = settings.srgbValues[valueIndex];
            if (saturation <= 0 || value <= 0 || value >= 1) continue;
            const x = hueIndex * settings.srgbSaturations.length + visualSaturation;
            const y = settings.srgbValues.length - 1 - valueIndex;
            pixelColors[y * width + x] = hsvColor(settings.srgbHues[hueIndex], saturation, value);
          }
        }
      }

      const greyX = width - 1;
      for (let y = 0; y < height; y++) {
        const value = settings.srgbValues[settings.srgbValues.length - 1 - y];
        const grey = Math.round(value * 255);
        pixelColors[y * width + greyX] = [grey, grey, grey, 255];
      }

      return { width, height, pixelColors };
    }

    function drawSrgbComparison(settings) {
      const rendered = makeSrgbComparisonPixels(settings);
      drawDirect(srgbCanvas, srgbCtx, rendered.width, rendered.height, rendered.pixelColors);
      return { width: rendered.width, height: rendered.height, pixelColors: rendered.pixelColors };
    }

    function nearestPaletteColor(color, paletteOklab, paletteColors, bias, offset) {
      const oklch = offsetInputOklch(oklabToOklch(rgbToOklab(color[0], color[1], color[2])), offset);
      const oklab = oklchToOklab(oklch);
      let bestIndex = 0;
      let bestDistance = Number.POSITIVE_INFINITY;

      for (let i = 0; i < paletteOklab.length; i++) {
        const distance = deltaEOkDistanceSquared(oklab, paletteOklab[i]);
        if (distance < bestDistance) {
          bestDistance = distance;
          bestIndex = i;
        }
      }

      return paletteColors[bestIndex] || [0, 0, 0, 255];
    }

    function drawRoundedSrgbComparison(srgbRendered, palette, bias, offset) {
      const paletteColors = palette.colors;
      const paletteOklab = paletteColors.map(color => rgbToOklab(color[0], color[1], color[2]));
      const roundedPixels = srgbRendered.pixelColors.map(color => nearestPaletteColor(color, paletteOklab, paletteColors, bias, offset));
      drawDirect(roundedSrgbCanvas, roundedSrgbCtx, srgbRendered.width, srgbRendered.height, roundedPixels);
      return { width: srgbRendered.width, height: srgbRendered.height };
    }

    function srgbColorCountFor(settings) {
      let count = settings.srgbValues.length;
      for (const hue of settings.srgbHues) {
        for (const saturation of settings.srgbSaturations) {
          for (const value of settings.srgbValues) {
            if (saturation <= 0 || value <= 0 || value >= 1) continue;
            count++;
          }
        }
      }
      return count;
    }

    function paddedPalette(colors) {
      const palette = colors.filter(isColor);
      while (palette.length < 256) palette.push([0, 0, 0, 255]);
      return palette;
    }

    function makeIpsi(width, height, colors, pixels) {
      const palette = paddedPalette(colors);
      const bytes = new Uint8Array(11 + palette.length * 4 + pixels.length);
      bytes.set([0x49, 0x50, 0x53, 0x49, 1], 0);
      bytes[5] = width & 0xff;
      bytes[6] = width >> 8;
      bytes[7] = height & 0xff;
      bytes[8] = height >> 8;
      bytes[9] = palette.length & 0xff;
      bytes[10] = palette.length >> 8;
      let cursor = 11;
      for (const color of palette) {
        bytes.set(color, cursor);
        cursor += 4;
      }
      bytes.set(pixels, cursor);
      return new Blob([bytes], { type: "application/octet-stream" });
    }

    function makeTomlText(settings, colors) {
      const palette = colors.filter(isColor);
      const lines = [
        "# PixelStream custom-host palette.",
        `# Generated by IPSC Palette Lab: normalized chroma ${settings.chromaSamples[0] ?? "none"}..${settings.chromaSamples.at(-1) ?? "none"} x${settings.chromaSamples.length}, OKLCH chroma ${settings.chromas[0] ?? "none"}..${settings.chromas.at(-1) ?? "none"}, value samples ${settings.values[0]}..${settings.values.at(-1)} x${settings.values.length}, add black ${settings.addBlack}, add white ${settings.addWhite}, sRGB hue ${settings.hueMin}..${settings.hueMax} x${settings.hueDivisions}, normalized sRGB offset ${settings.hueOffset} (effective ${settings.shiftedHueOffset}).`,
        "[matching]",
        `oklab_l = ${settings.bias.lightness.toFixed(6)}`,
        `oklab_a = ${settings.bias.chroma.toFixed(6)}`,
        `oklab_b = ${settings.bias.hue.toFixed(6)}`,
        `lightness_multiply = ${settings.offset.lightnessMultiply.toFixed(6)}`,
        `lightness_add = ${settings.offset.lightnessAdd.toFixed(6)}`,
        `chroma_multiply = ${settings.offset.chromaMultiply.toFixed(6)}`,
        `chroma_add = ${settings.offset.chromaAdd.toFixed(6)}`,
        `grey_chroma_threshold = ${settings.offset.greyChromaThreshold.toFixed(6)}`,
        `hue_add = ${settings.offset.hueAdd.toFixed(6)}`,
        "",
        "colors = [",
        ...palette.map(color => `  "${hex(color)}",`),
        "]",
        "",
      ];
      return lines.join("\n");
    }

    function revokeDownloads() {
      if (ipsiUrl) URL.revokeObjectURL(ipsiUrl);
      if (mapUrl) URL.revokeObjectURL(mapUrl);
      ipsiUrl = null;
      mapUrl = null;
      currentPaletteArtifact = null;
      downloadIpsi.removeAttribute("href");
      downloadMap.removeAttribute("href");
      downloadIpsi.setAttribute("aria-disabled", "true");
      downloadMap.setAttribute("aria-disabled", "true");
      bakeButton.disabled = true;
      bakeButton.textContent = "Bake";
      hideBakeProgress();
    }

    function setBakeProgress(percent) {
      const clamped = Math.max(0, Math.min(100, percent));
      bakeProgressWrap.hidden = false;
      bakeProgress.value = clamped;
      bakeProgressText.textContent = `${Math.round(clamped)}%`;
    }

    function hideBakeProgress() {
      bakeProgressWrap.hidden = true;
      bakeProgress.value = 0;
      bakeProgressText.textContent = "0%";
    }

    function setDownload(link, blob, filename) {
      const url = URL.createObjectURL(blob);
      link.href = url;
      link.download = filename;
      link.setAttribute("aria-disabled", "false");
      return url;
    }

    async function bakeMapBlob(artifact) {
      const tableSize = 256 * 256 * 256;
      const alteredEntries = new Uint8Array(tableSize);
      const directEntries = new Uint8Array(tableSize);
      const paletteOklab = artifact.colors.map(color => rgbToOklab(color[0], color[1], color[2]));
      const matching = artifact.settings.bias;
      const offset = artifact.settings.offset;
      let cursor = 0;
      setBakeProgress(0);

      for (let r = 0; r < 256; r++) {
        for (let g = 0; g < 256; g++) {
          for (let b = 0; b < 256; b++) {
            alteredEntries[cursor++] = nearestPaletteIndexForMap(rgbToOklab(r, g, b), paletteOklab, matching, offset);
          }
        }
        if (r % 4 === 0) {
          const percent = r / 255 * 50;
          setBakeProgress(percent);
          status.textContent = `${artifact.baseStatus}\nbaking ${artifact.base}.ipsmap... ${Math.round(percent)}%`;
          await new Promise(resolve => setTimeout(resolve, 0));
        }
      }
      cursor = 0;
      for (let r = 0; r < 256; r++) {
        for (let g = 0; g < 256; g++) {
          for (let b = 0; b < 256; b++) {
            directEntries[cursor++] = nearestPaletteIndexForMap(rgbToOklab(r, g, b), paletteOklab, matching, null);
          }
        }
        if (r % 4 === 0) {
          const percent = 50 + r / 255 * 50;
          setBakeProgress(percent);
          status.textContent = `${artifact.baseStatus}\nbaking ${artifact.base}.ipsmap... ${Math.round(percent)}%`;
          await new Promise(resolve => setTimeout(resolve, 0));
        }
      }
      const entries = new Uint8Array(tableSize * 2);
      entries.set(alteredEntries, 0);
      entries.set(directEntries, tableSize);
      setBakeProgress(100);

      const paletteBytes = new Uint8Array(artifact.colors.length * 4);
      for (let i = 0; i < artifact.colors.length; i++) {
        const color = artifact.colors[i];
        const dst = i * 4;
        paletteBytes[dst] = color[0];
        paletteBytes[dst + 1] = color[1];
        paletteBytes[dst + 2] = color[2];
        paletteBytes[dst + 3] = color[3] ?? 255;
      }
      const matchingBytes = paletteMatchingBytes(artifact.settings);
      const header = new Uint8Array(24);
      header.set([0x49, 0x50, 0x53, 0x4d, 0x41, 0x50, 0x36, 0x00], 0);
      writeU64(header, 8, lookupHash(artifact.colors, matchingBytes, entries));
      writeU16(header, 16, artifact.colors.length);
      writeU16(header, 18, matchingBytes.length);
      writeU32(header, 20, entries.length);
      return new Blob([header, paletteBytes, matchingBytes, entries], { type: "application/octet-stream" });
    }

    function nearestPaletteIndexForMap(oklab, paletteOklab, matching, offset) {
      const baseColor = oklabToOklch(oklab);
      const color = offset ? oklchToOklab(offsetInputOklch(baseColor, offset)) : oklab;
      let bestIndex = 0;
      let bestDistance = Number.POSITIVE_INFINITY;
      for (let i = 0; i < Math.min(256, paletteOklab.length); i++) {
        const distance = deltaEOkDistanceSquared(color, paletteOklab[i]);
        if (distance < bestDistance) {
          bestDistance = distance;
          bestIndex = i;
        }
      }
      return bestIndex;
    }

    function paletteMatchingBytes(settings) {
      const values = [
        settings.bias.lightness,
        settings.bias.chroma,
        settings.bias.hue,
        settings.offset.lightnessMultiply,
        settings.offset.lightnessAdd,
        settings.offset.chromaMultiply,
        settings.offset.chromaAdd,
        settings.offset.greyChromaThreshold,
        settings.offset.hueAdd,
      ];
      const bytes = new Uint8Array(values.length * 4);
      const view = new DataView(bytes.buffer);
      values.forEach((value, index) => view.setFloat32(index * 4, value, true));
      return bytes;
    }

    function lookupHash(colors, matchingBytes, entries) {
      let hash = 0xcbf29ce484222325n;
      const prime = 0x100000001b3n;
      const feed = byte => {
        hash ^= BigInt(byte);
        hash = BigInt.asUintN(64, hash * prime);
      };
      for (const byte of new TextEncoder().encode("delta-e-ok-v8")) feed(byte);
      for (const color of colors) {
        for (const byte of color) feed(byte);
      }
      for (const byte of matchingBytes) feed(byte);
      for (const byte of entries) {
        feed(byte);
      }
      return hash;
    }

    function writeU16(bytes, offset, value) {
      bytes[offset] = value & 0xff;
      bytes[offset + 1] = (value >> 8) & 0xff;
    }

    function writeU32(bytes, offset, value) {
      bytes[offset] = value & 0xff;
      bytes[offset + 1] = (value >> 8) & 0xff;
      bytes[offset + 2] = (value >> 16) & 0xff;
      bytes[offset + 3] = (value >> 24) & 0xff;
    }

    function writeU64(bytes, offset, value) {
      for (let i = 0; i < 8; i++) {
        bytes[offset + i] = Number((value >> BigInt(i * 8)) & 0xffn);
      }
    }

    function previewOnly() {
      revokeDownloads();
      const settings = readSettings();
      const required = colorCountFor(settings);
      const srgbRequired = srgbColorCountFor(settings);
      if (required > 256) {
        status.className = "status bad";
        const ratio = srgbRequired === 0 ? "n/a" : (required / srgbRequired).toFixed(3);
        status.textContent = `does not fit\nOKLCH required: ${required}\nsRGB colors: ${srgbRequired}\ncolour space compression ratio: ${ratio}\nlimit: 256`;
        ctx.clearRect(0, 0, canvas.width, canvas.height);
        roundedSrgbCtx.clearRect(0, 0, roundedSrgbCanvas.width, roundedSrgbCanvas.height);
        drawSrgbComparison(settings);
        return null;
      }

      const palette = generatePalette(settings);
      const rendered = draw(settings, palette);
      const srgbRendered = drawSrgbComparison(settings);
      const roundedSrgbRendered = drawRoundedSrgbComparison(srgbRendered, palette, settings.bias, settings.offset);
      const reserved = 256 - palette.colors.length;
      localStorage.removeItem("ipscCurrentPaletteToml");
      localStorage.setItem("ipscCurrentPaletteName", `${filenameBase()}.ipsmap`);
      status.className = "status ok";
      const ratio = srgbRequired === 0 ? "n/a" : (palette.colors.length / srgbRequired).toFixed(3);
      const baseStatus = `fits\nOKLCH colors: ${palette.colors.length}\nreserved: ${reserved}\nsRGB colors: ${srgbRequired}\ncolour space compression ratio: ${ratio}\nimage: ${rendered.width}x${rendered.height}\nsRGB image: ${srgbRendered.width}x${srgbRendered.height}\nrounded sRGB image: ${roundedSrgbRendered.width}x${roundedSrgbRendered.height}\npriority L/a/b: ${settings.bias.lightness.toFixed(3)} / ${settings.bias.chroma.toFixed(3)} / ${settings.bias.hue.toFixed(3)}\noffset Vx/V+/Cx/C+/G/H+: ${settings.offset.lightnessMultiply.toFixed(3)} / ${settings.offset.lightnessAdd.toFixed(3)} / ${settings.offset.chromaMultiply.toFixed(3)} / ${settings.offset.chromaAdd.toFixed(3)} / ${settings.offset.greyChromaThreshold.toFixed(3)} / ${settings.offset.hueAdd.toFixed(3)}`;
      status.textContent = `${baseStatus}\npreview only; press Generate to build files`;
      return { settings, palette, rendered, baseStatus };
    }

    async function generate() {
      const preview = previewOnly();
      if (!preview) {
        return;
      }

      const { settings, palette, rendered, baseStatus } = preview;
      const base = filenameBase();
      ipsiUrl = setDownload(downloadIpsi, makeIpsi(rendered.width, rendered.height, palette.colors, rendered.pixels), `${base}.ipsi`);
      currentPaletteArtifact = { base, settings, colors: palette.colors.filter(isColor), baseStatus };
      bakeButton.disabled = false;
      status.textContent = `${baseStatus}\nfiles ready; press Bake for ${base}.ipsmap`;
    }

    for (const input of biasInputs) {
      input.addEventListener("input", () => {
        normalizeBias(input);
        markSettingsChanged();
      });
    }

    for (const input of offsetInputs) {
      input.addEventListener("input", () => {
        updateOffsetLabels();
        markSettingsChanged();
      });
    }

    for (const [rangeId, numberId, min, max] of sliderPairs) {
      const range = document.getElementById(rangeId);
      const number = document.getElementById(numberId);
      const commitNumber = () => {
        range.value = clampSliderValue(Number(number.value), min, max).toFixed(3);
        if (biasInputs.includes(range)) {
          normalizeBias(range);
        } else {
          updateOffsetLabels();
        }
        markSettingsChanged();
      };
      number.addEventListener("change", commitNumber);
      number.addEventListener("keydown", event => {
        if (event.key === "Enter") {
          event.preventDefault();
          commitNumber();
          number.blur();
        } else if (event.key === "Escape") {
          if (biasInputs.includes(range)) {
            updateBiasLabels();
          } else {
            updateOffsetLabels();
          }
          number.blur();
        }
      });
    }

    bakeButton.addEventListener("click", async () => {
      if (!currentPaletteArtifact) return;
      bakeButton.disabled = true;
      bakeButton.textContent = "Baking...";
      status.className = "status ok";
      status.textContent = `${currentPaletteArtifact.baseStatus}\nbaking ${currentPaletteArtifact.base}.ipsmap in browser...`;
      try {
        const mapBlob = await bakeMapBlob(currentPaletteArtifact);
        if (mapUrl) URL.revokeObjectURL(mapUrl);
        mapUrl = setDownload(downloadMap, mapBlob, `${currentPaletteArtifact.base}.ipsmap`);
        status.textContent = `${currentPaletteArtifact.baseStatus}\nmap: ${currentPaletteArtifact.base}.ipsmap (${(mapBlob.size / 1024 / 1024).toFixed(1)} MB)`;
      } catch (error) {
        status.className = "status bad";
        status.textContent = error.toString();
      } finally {
        bakeButton.disabled = false;
        bakeButton.textContent = "Bake";
      }
    });

    form.addEventListener("submit", async event => {
      event.preventDefault();
      try {
        await generate();
      } catch (error) {
        revokeDownloads();
        status.className = "status bad";
        status.textContent = error.toString();
      }
    });

    updateBiasLabels();
    updateOffsetLabels();
    generate().catch(error => {
      revokeDownloads();
      status.className = "status bad";
      status.textContent = error.toString();
    });
  </script>
</body>
</html>"##
    .replace("__OKLCH_MAX_CHROMA__", &OKLCH_MAX_CHROMA.to_string())
}
