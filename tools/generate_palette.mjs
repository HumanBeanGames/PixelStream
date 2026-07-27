// Generates the default OKLCH-spaced palette used by PixelStream authoring tools.

import fs from "node:fs/promises";

const HUE_COUNT = 20;
const HUE_OFFSET_DEGREES = 29.233885;
const LIGHTNESS_LEVELS = [0, 0.06666667, 0.13333334, 0.2, 0.26666668, 0.33333334, 0.4, 0.46666667, 0.53333336, 0.6, 0.6666667, 0.73333335, 0.8, 0.8666667, 0.93333334, 1];
const CHROMA_LEVELS = [0.08589443, 0.17178887, 0.2576833];

function linearToSrgbByte(value) {
  value = Math.max(0, Math.min(1, value));
  const srgb =
    value <= 0.0031308 ? value * 12.92 : 1.055 * Math.pow(value, 1 / 2.4) - 0.055;
  return Math.round(Math.max(0, Math.min(255, srgb * 255)));
}

function oklchToLinearSrgb(lightness, chroma, hueDegrees) {
  const hue = (hueDegrees * Math.PI) / 180;
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

function inSrgbGamut([r, g, b]) {
  return (
    Number.isFinite(r) &&
    Number.isFinite(g) &&
    Number.isFinite(b) &&
    r >= 0 &&
    r <= 1 &&
    g >= 0 &&
    g <= 1 &&
    b >= 0 &&
    b <= 1
  );
}

function maxSrgbChroma(lightness, hueDegrees) {
  let low = 0;
  let high = 0.4;

  for (let i = 0; i < 16; i++) {
    const mid = (low + high) * 0.5;
    if (inSrgbGamut(oklchToLinearSrgb(lightness, mid, hueDegrees))) {
      low = mid;
    } else {
      high = mid;
    }
  }

  return low;
}

function oklchHex(lightness, chroma, hueDegrees) {
  const [r, g, b] = oklchToLinearSrgb(lightness, chroma, hueDegrees).map(linearToSrgbByte);
  return `"#${r.toString(16).padStart(2, "0").toUpperCase()}${g
    .toString(16)
    .padStart(2, "0")
    .toUpperCase()}${b.toString(16).padStart(2, "0").toUpperCase()}"`;
}

function checkedOklchHex(lightness, chroma, hueDegrees) {
  const rgb = oklchToLinearSrgb(lightness, chroma, hueDegrees);
  return inSrgbGamut(rgb) ? oklchHex(lightness, chroma, hueDegrees) : null;
}

function greyscaleHex(lightness) {
  if (lightness <= 0) return '"#000000"';
  if (lightness >= 1) return '"#FFFFFF"';
  return oklchHex(lightness, 0, 0);
}

function generatedColors() {
  const colors = [];

  for (const lightness of LIGHTNESS_LEVELS) {
    colors.push(greyscaleHex(lightness));
  }

  for (let hueIndex = 0; hueIndex < HUE_COUNT; hueIndex++) {
    const hueDegrees = HUE_OFFSET_DEGREES + (hueIndex * 360) / HUE_COUNT;
    for (const chroma of CHROMA_LEVELS) {
      for (const lightness of LIGHTNESS_LEVELS) {
        if (lightness <= 0 || lightness >= 1) {
          continue;
        }
        const color = checkedOklchHex(lightness, chroma, hueDegrees);
        if (color) {
          colors.push(color);
        }
      }
    }
  }

  while (colors.length < 256) {
    colors.push('"#000000"');
  }

  return colors;
}

function paletteToml(header, colors) {
  return `${header}
# Generated OKLCH palette: 16 greyscale entries, 20 compact hue blocks.
# First hue is offset to approximately sRGB pure red in OKLCH.
# Out-of-gamut OKLCH cells are omitted instead of clipped or remapped.
colors = [
${colors.map((color) => `  ${color},`).join("\n")}
]
`;
}

const colors = generatedColors();
const output = process.argv[2] ?? "generated_palette.toml";
await fs.writeFile(output, paletteToml("# Generated PixelStream-compatible palette.", colors));

const realColorCount = colors.findLastIndex((color) => color !== '"#000000"') + 1;
console.error(`Generated ${output}: ${colors.length} colors (${realColorCount} real, ${256 - realColorCount} reserved).`);
