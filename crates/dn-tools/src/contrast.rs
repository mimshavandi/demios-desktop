//! `contrast_check`: the WCAG 2.x contrast ratio of two colours, worked out in code so a
//! model never estimates it (#365). The design-review skill allows it; `dn eval` calls the
//! same function.
//!
//! - Colours: `#rgb`, `#rrggbb`, `rgb(r, g, b)` (commas or spaces, 0–255) or a CSS named
//!   colour. Transparent colours are refused: the ratio needs the colour as it's seen.
//! - Ratio: (L1 + 0.05) / (L2 + 0.05) from relative luminance with sRGB linearisation
//!   (WCAG 2.2, "relative luminance", threshold 0.04045). It's shown rounded down to 2
//!   decimals, so 4.499 never reads as 4.5; the pass flags use the exact value.
//! - Large text is at least 24 px, or 18.66 px when bold (18 pt / 14 pt).

use dn_core::chat::ToolSpec;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The tool's name as the model sees it.
pub const TOOL_NAME: &str = "contrast_check";
/// Its capability id, for agent manifests and skills' `allowed-tools`.
pub const CAPABILITY_ID: &str = "contrast.check";

const LARGE_PX: f64 = 24.0;
const LARGE_BOLD_PX: f64 = 18.66;

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContrastInput {
    pub foreground: String,
    pub background: String,
    /// Text size in CSS px, to say whether it's large text.
    #[serde(default)]
    pub font_size_px: Option<f64>,
    #[serde(default)]
    pub bold: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ContrastResult {
    /// The two colours as `#rrggbb`.
    pub foreground: String,
    pub background: String,
    /// Rounded down to 2 decimals.
    pub ratio: f64,
    /// Whether the text is large, when a size was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub large_text: Option<bool>,
    /// 4.5:1, WCAG 1.4.3.
    pub aa_normal_text: bool,
    /// 3:1, WCAG 1.4.3.
    pub aa_large_text: bool,
    /// 3:1 for icons, borders and focus indicators, WCAG 1.4.11.
    pub aa_non_text: bool,
    /// 7:1, WCAG 1.4.6.
    pub aaa_normal_text: bool,
    /// 4.5:1, WCAG 1.4.6.
    pub aaa_large_text: bool,
}

/// The ratio and its WCAG results, or why the input can't be checked.
pub fn check(input: &ContrastInput) -> Result<ContrastResult, String> {
    let fg = parse_colour(&input.foreground).map_err(|e| format!("foreground: {e}"))?;
    let bg = parse_colour(&input.background).map_err(|e| format!("background: {e}"))?;
    let large_text = match input.font_size_px {
        Some(px) if !(px.is_finite() && px > 0.0) => {
            return Err("font_size_px must be a positive number".into());
        }
        Some(px) => Some(
            px >= if input.bold == Some(true) {
                LARGE_BOLD_PX
            } else {
                LARGE_PX
            },
        ),
        None => None,
    };
    let exact = ratio(fg, bg);
    Ok(ContrastResult {
        foreground: hex(fg),
        background: hex(bg),
        // The small epsilon keeps an exact 4.5 from flooring to 4.49 through float error.
        ratio: ((exact * 100.0) + 1e-9).floor() / 100.0,
        large_text,
        aa_normal_text: exact >= 4.5,
        aa_large_text: exact >= 3.0,
        aa_non_text: exact >= 3.0,
        aaa_normal_text: exact >= 7.0,
        aaa_large_text: exact >= 4.5,
    })
}

/// The tool for the model.
pub fn tool_spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME.into(),
        description: "Measures the WCAG contrast ratio between a text or UI colour and its \
            background, with AA and AAA results. Use it for every contrast ratio you report; \
            never estimate one. Colours: #rgb, #rrggbb, rgb(r, g, b) or a CSS colour name. \
            Give font_size_px (and bold) to learn whether the text counts as large."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "foreground": {"type": "string", "description": "Text or UI colour"},
                "background": {"type": "string", "description": "Colour behind it"},
                "font_size_px": {"type": "number", "description": "Text size in CSS px"},
                "bold": {"type": "boolean"}
            },
            "required": ["foreground", "background"],
            "additionalProperties": false
        }),
    }
}

/// Runs one call: the arguments as JSON in, the result (or the error) as JSON text out.
pub fn call(arguments: &serde_json::Value) -> Result<String, String> {
    let input: ContrastInput =
        serde_json::from_value(arguments.clone()).map_err(|e| format!("invalid arguments: {e}"))?;
    let result = check(&input)?;
    serde_json::to_string(&result).map_err(|e| e.to_string())
}

type Rgb = [u8; 3];

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn channel(v: u8) -> f64 {
    let c = f64::from(v) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn luminance(c: Rgb) -> f64 {
    0.2126 * channel(c[0]) + 0.7152 * channel(c[1]) + 0.0722 * channel(c[2])
}

fn ratio(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

fn parse_colour(s: &str) -> Result<Rgb, String> {
    let t = s.trim().to_ascii_lowercase();
    if t.is_empty() {
        return Err("no colour given".into());
    }
    if let Some(h) = t.strip_prefix('#') {
        return parse_hex(h).ok_or_else(|| {
            format!("`{s}` isn't a colour: use #rgb or #rrggbb (transparency isn't supported)")
        });
    }
    if let Some(inner) = t.strip_prefix("rgb(").and_then(|r| r.strip_suffix(')')) {
        return parse_rgb(inner).ok_or_else(|| {
            format!("`{s}` isn't a colour: use rgb(r, g, b) with values from 0 to 255")
        });
    }
    if t.starts_with("rgba(") || t.starts_with("hsl") || t == "transparent" {
        return Err(format!(
            "`{s}` isn't supported: give the colour as #rrggbb or rgb(r, g, b), without transparency"
        ));
    }
    NAMED
        .iter()
        .find(|(n, _)| *n == t)
        .map(|(_, v)| [(v >> 16) as u8, (v >> 8) as u8, *v as u8])
        .ok_or_else(|| format!("`{s}` isn't a colour this tool knows"))
}

fn parse_hex(h: &str) -> Option<Rgb> {
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let d = |i: usize, n: usize| u8::from_str_radix(&h[i..i + n], 16).ok();
    match h.len() {
        3 => Some([d(0, 1)? * 17, d(1, 1)? * 17, d(2, 1)? * 17]),
        6 => Some([d(0, 2)?, d(2, 2)?, d(4, 2)?]),
        _ => None,
    }
}

fn parse_rgb(inner: &str) -> Option<Rgb> {
    let parts: Vec<&str> = inner
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() != 3 {
        return None;
    }
    let mut out = [0u8; 3];
    for (o, p) in out.iter_mut().zip(parts) {
        *o = p.parse::<u8>().ok()?;
    }
    Some(out)
}

/// CSS named colours (CSS Color Module Level 4), without `transparent`.
const NAMED: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn run(fg: &str, bg: &str, px: Option<f64>, bold: Option<bool>) -> ContrastResult {
        check(&ContrastInput {
            foreground: fg.into(),
            background: bg.into(),
            font_size_px: px,
            bold,
        })
        .unwrap()
    }

    #[test]
    fn known_ratios() {
        // Black on white is the maximum, 21:1; equal colours are 1:1.
        assert_eq!(run("#000", "#fff", None, None).ratio, 21.0);
        assert_eq!(run("white", "#ffffff", None, None).ratio, 1.0);
        // The order of the two colours doesn't matter.
        assert_eq!(run("#ffffff", "#4f9cf9", None, None).ratio, 2.81);
        assert_eq!(run("#4f9cf9", "#ffffff", None, None).ratio, 2.81);
        assert_eq!(run("#6b7280", "#f3f4f6", None, None).ratio, 4.39);
        assert_eq!(run("#1f2937", "#f59e0b", None, None).ratio, 6.83);
    }

    #[test]
    fn the_aa_boundary_is_exact() {
        // #767676 on white is the lightest grey that passes 4.5:1; #777777 fails.
        let pass = run("#767676", "#fff", None, None);
        assert_eq!(pass.ratio, 4.54);
        assert!(pass.aa_normal_text);
        let fail = run("#777777", "#fff", None, None);
        assert_eq!(fail.ratio, 4.47);
        assert!(!fail.aa_normal_text && fail.aa_large_text && fail.aa_non_text);
    }

    #[test]
    fn rounds_down_never_up() {
        // An exact ratio just under a threshold must not read as passing.
        for (fg, bg) in [
            ("#777777", "#ffffff"),
            ("#949494", "#ffffff"),
            ("#595959", "#ffffff"),
        ] {
            let r = run(fg, bg, None, None);
            let exact = ratio(parse_colour(fg).unwrap(), parse_colour(bg).unwrap());
            assert!(
                r.ratio <= exact && exact - r.ratio < 0.01,
                "{fg}: {} vs {exact}",
                r.ratio
            );
        }
    }

    #[test]
    fn levels() {
        let r = run("#a3a3a3", "#171717", None, None);
        assert_eq!(r.ratio, 7.1);
        assert!(r.aa_normal_text && r.aaa_normal_text && r.aaa_large_text);
        let low = run("#d1d5db", "#ffffff", None, None);
        assert_eq!(low.ratio, 1.47);
        assert!(!low.aa_non_text && !low.aa_large_text);
    }

    #[test]
    fn large_text() {
        assert_eq!(run("#000", "#fff", Some(24.0), None).large_text, Some(true));
        assert_eq!(
            run("#000", "#fff", Some(23.9), Some(false)).large_text,
            Some(false)
        );
        assert_eq!(
            run("#000", "#fff", Some(18.66), Some(true)).large_text,
            Some(true)
        );
        assert_eq!(
            run("#000", "#fff", Some(18.0), Some(true)).large_text,
            Some(false)
        );
        assert_eq!(run("#000", "#fff", None, Some(true)).large_text, None);
    }

    #[test]
    fn colour_forms() {
        let want = run("#336699", "#fff", None, None).ratio;
        for c in [
            "#369",
            "#336699",
            "#336699 ",
            "RGB(51, 102, 153)",
            "rgb(51 102 153)",
        ] {
            assert_eq!(run(c, "white", None, None).ratio, want, "{c}");
        }
        assert_eq!(
            run("rebeccapurple", "white", None, None).foreground,
            "#663399"
        );
    }

    #[test]
    fn refuses_what_it_cant_measure() {
        for bad in [
            "",
            "#12",
            "#12345678",
            "#ggg",
            "rgb(256, 0, 0)",
            "rgb(1, 2)",
            "rgba(0,0,0,0.5)",
            "hsl(0, 0%, 50%)",
            "transparent",
            "notacolour",
        ] {
            let e = check(&ContrastInput {
                foreground: bad.into(),
                background: "#fff".into(),
                font_size_px: None,
                bold: None,
            })
            .unwrap_err();
            assert!(e.starts_with("foreground:"), "{bad}: {e}");
        }
        let e = check(&ContrastInput {
            foreground: "#000".into(),
            background: "#fff".into(),
            font_size_px: Some(-1.0),
            bold: None,
        })
        .unwrap_err();
        assert!(e.contains("font_size_px"), "{e}");
    }

    #[test]
    fn call_takes_and_gives_json() {
        let out =
            call(&json!({"foreground": "#9a9a9a", "background": "#ffffff", "font_size_px": 14}))
                .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ratio"], 2.81);
        assert_eq!(v["large_text"], false);
        assert_eq!(v["aa_normal_text"], false);
        assert!(call(&json!({"foreground": "#000"})).is_err());
        assert!(call(&json!({"foreground": "#000", "background": "#fff", "extra": 1})).is_err());
        assert_eq!(tool_spec().name, TOOL_NAME);
    }
}
