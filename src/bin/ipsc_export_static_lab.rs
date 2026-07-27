//! Exports the embedded palette lab pages into a static `dist` directory.

use std::fs;

const OUT_DIR: &str = "dist/ipsc_lab";
const OKLCH_MAX_CHROMA: f32 = 0.2576833;

fn main() {
    if let Err(err) = export_static_lab() {
        eprintln!("Could not export static IPSC lab: {err}");
        std::process::exit(1);
    }
    eprintln!("Static IPSC lab exported to {OUT_DIR}");
}

fn export_static_lab() -> Result<(), String> {
    fs::create_dir_all(OUT_DIR).map_err(|err| err.to_string())?;
    write("index.html", &lab_shell_html())?;
    write("palette.html", &palette_html())?;
    write("converter.html", &converter_html())?;
    Ok(())
}

fn write(name: &str, contents: &str) -> Result<(), String> {
    fs::write(format!("{OUT_DIR}/{name}"), contents).map_err(|err| err.to_string())
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
  <iframe id="paletteFrame" class="active" src="palette.html"></iframe>
  <iframe id="converterFrame" src="converter.html"></iframe>
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
