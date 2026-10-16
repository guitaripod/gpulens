//! Colour palette, terminal capability detection and glyph sets for the TUI.

use ratatui::style::{Color, Modifier, Style};

use crate::model::{COLOR_SLOTS, Slot};
use crate::types::Category;

/// How many colours the terminal can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorMode {
    TrueColor,
    Ansi256,
    Ansi16,
    Mono,
}

impl ColorMode {
    /// Picks the colour depth from an environment lookup, so tests can supply their own variables.
    pub fn detect_from(var: &dyn Fn(&str) -> Option<String>) -> Self {
        if is_set_non_empty(var, "NO_COLOR") {
            return Self::Mono;
        }
        let colorterm = var("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        if colorterm.contains("truecolor") || colorterm.contains("24bit") {
            return Self::TrueColor;
        }
        let term = var("TERM").unwrap_or_default();
        if term.is_empty() || term == "dumb" {
            return Self::Ansi16;
        }
        if is_truecolor_terminal(&term) {
            return Self::TrueColor;
        }
        if term.contains("256color") {
            return Self::Ansi256;
        }
        Self::Ansi16
    }
}

/// Terminals that always support 24-bit colour even when `COLORTERM` is not exported.
fn is_truecolor_terminal(term: &str) -> bool {
    matches!(
        term,
        "xterm-kitty" | "alacritty" | "foot" | "wezterm" | "ghostty"
    ) || term.starts_with("xterm-ghostty")
}

/// True when the variable is present with a non-empty value.
fn is_set_non_empty(var: &dyn Fn(&str) -> Option<String>, name: &str) -> bool {
    var(name).is_some_and(|value| !value.is_empty())
}

/// Reports whether the terminal background is light, from `COLORFGBG` (the last numeric field is the background).
pub fn terminal_is_light(var: &dyn Fn(&str) -> Option<String>) -> bool {
    var("COLORFGBG")
        .and_then(|value| last_numeric_field(&value))
        .is_some_and(|background| background == 7 || background == 15)
}

/// The last field of a `;`-separated value that parses as a number.
fn last_numeric_field(value: &str) -> Option<u32> {
    value
        .rsplit(';')
        .find_map(|field| field.trim().parse().ok())
}

/// Reports whether the effective locale is UTF-8; an absent locale is assumed to be a modern terminal.
pub fn locale_is_utf8(var: &dyn Fn(&str) -> Option<String>) -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .find_map(|name| var(name).filter(|value| !value.is_empty()))
        .is_none_or(|locale| names_utf8(&locale))
}

/// Matches the `UTF-8` and `utf8` spellings of a locale codeset, ignoring case.
fn names_utf8(locale: &str) -> bool {
    let lower = locale.to_ascii_lowercase();
    lower.contains("utf-8") || lower.contains("utf8")
}

/// Characters and strings used to draw bars, markers and separators.
#[derive(Clone, Debug)]
pub struct Glyphs {
    pub h_eighths: [char; 8],
    pub full: char,
    pub busy: &'static str,
    pub working: &'static str,
    pub light: &'static str,
    pub idle: &'static str,
    pub ai_mark: &'static str,
    pub arrow_up: &'static str,
    pub arrow_down: &'static str,
    pub selected: &'static str,
    pub sep: &'static str,
    pub fills: [char; 8],
}

const FILLS: [char; 8] = ['#', '=', '+', '*', '%', '@', '&', '$'];

/// Block-element glyphs for Unicode terminals.
fn unicode_glyphs() -> Glyphs {
    Glyphs {
        h_eighths: [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'],
        full: '█',
        busy: "●",
        working: "◐",
        light: "•",
        idle: "○",
        ai_mark: "◆",
        arrow_up: "▲",
        arrow_down: "▼",
        selected: "▶",
        sep: "·",
        fills: FILLS,
    }
}

/// Pure-ASCII fallback for terminals or locales that cannot show Unicode.
fn ascii_glyphs() -> Glyphs {
    const EIGHTHS: [char; 8] = [' ', ' ', ' ', ' ', ' ', ' ', ' ', '#'];
    Glyphs {
        h_eighths: EIGHTHS,
        full: '#',
        busy: "*",
        working: "+",
        light: "-",
        idle: ".",
        ai_mark: "*",
        arrow_up: "^",
        arrow_down: "v",
        selected: ">",
        sep: "-",
        fills: FILLS,
    }
}

/// Resolved colours and glyphs for one terminal session.
#[derive(Clone, Debug)]
pub struct Theme {
    pub mode: ColorMode,
    pub ascii: bool,
    pub light: bool,
    pub glyphs: Glyphs,
}

impl Theme {
    /// Builds the theme from the real environment, with explicit overrides for ASCII and monochrome output.
    pub fn detect(force_ascii: bool, force_mono: bool) -> Self {
        Self::from_env(&real_var, force_ascii, force_mono)
    }

    /// Builds a theme for an explicit mode, glyph set and background.
    pub fn new(mode: ColorMode, ascii: bool, light: bool) -> Self {
        let glyphs = if ascii {
            ascii_glyphs()
        } else {
            unicode_glyphs()
        };
        Self {
            mode,
            ascii,
            light,
            glyphs,
        }
    }

    /// True when the theme emits no colour, only modifiers.
    pub fn is_mono(&self) -> bool {
        self.mode == ColorMode::Mono
    }

    /// Colour for a workload slot; out-of-range slots take the `Other` colour.
    pub fn slot(&self, slot: Slot) -> Color {
        let entry = match slot {
            Slot::Color(index) if index < COLOR_SLOTS => {
                self.slot_palette().get(usize::from(index)).copied()
            }
            _ => None,
        };
        self.tone(entry.unwrap_or((self.other_hex(), Color::DarkGray)))
    }

    /// Colour of the small tag that marks a category.
    pub fn category(&self, category: Category) -> Color {
        let palette = if self.light {
            &CATEGORY_LIGHT
        } else {
            &CATEGORY_DARK
        };
        self.tone(palette[category_index(category)])
    }

    /// Plain body text.
    pub fn text(&self) -> Style {
        self.paint(Role::Text, Modifier::empty())
    }

    /// Secondary text.
    pub fn dim(&self) -> Style {
        self.paint(Role::Dim, Modifier::DIM)
    }

    /// Tertiary text such as idle markers and rules.
    pub fn faint(&self) -> Style {
        self.paint(Role::Faint, Modifier::DIM)
    }

    /// Highlighted values that should stand out from body text.
    pub fn accent(&self) -> Style {
        self.paint(Role::Accent, Modifier::BOLD)
    }

    /// Healthy or successful state.
    pub fn good(&self) -> Style {
        self.paint(Role::Good, Modifier::empty())
    }

    /// Approaching a limit.
    pub fn warn(&self) -> Style {
        self.paint(Role::Warn, Modifier::UNDERLINED)
    }

    /// Failed or critical state.
    pub fn bad(&self) -> Style {
        self.paint(Role::Bad, Modifier::BOLD | Modifier::UNDERLINED)
    }

    /// Unfocused panel border.
    pub fn border(&self) -> Style {
        self.paint(Role::Border, Modifier::empty())
    }

    /// Border of the focused panel.
    pub fn border_focus(&self) -> Style {
        self.paint(Role::BorderFocus, Modifier::BOLD)
    }

    /// The highlighted row in a list.
    pub fn selected(&self) -> Style {
        if self.is_mono() {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
                .fg(self.role(Role::Text))
                .bg(self.role(Role::SelectedBg))
        }
    }

    /// Panel and screen titles.
    pub fn title(&self) -> Style {
        if self.is_mono() {
            Style::new().add_modifier(Modifier::BOLD)
        } else {
            Style::new()
                .fg(self.role(Role::Text))
                .add_modifier(Modifier::BOLD)
        }
    }

    /// The marker that flags AI-related processes.
    pub fn ai(&self) -> Style {
        self.paint(Role::Ai, Modifier::empty())
    }

    /// Keyboard shortcut text in the footer.
    pub fn key_hint(&self) -> Style {
        self.paint(Role::Dim, Modifier::BOLD)
    }

    /// Background colour of unfilled bar cells and grid cells.
    pub fn empty_cell(&self) -> Color {
        self.role(Role::Empty)
    }

    /// Colour for a 0..=1 utilisation or temperature fraction; NaN counts as 0 and out-of-range values are clamped.
    pub fn heat(&self, fraction: f64) -> Style {
        let level = clamped_fraction(fraction);
        if level < 0.6 {
            self.good()
        } else if level < 0.85 {
            self.warn()
        } else {
            self.bad()
        }
    }

    /// Style for a process activity label: `busy`, `working`, `light` or `idle`; anything else reads as text.
    pub fn activity_style(&self, label: &str) -> Style {
        match label {
            "busy" => self.accent().add_modifier(Modifier::BOLD),
            "light" => self.dim(),
            "idle" => self.faint(),
            _ => self.text(),
        }
    }

    /// Resolves a theme from an environment lookup, applying the forced overrides.
    fn from_env(var: &dyn Fn(&str) -> Option<String>, force_ascii: bool, force_mono: bool) -> Self {
        let mode = if force_mono {
            ColorMode::Mono
        } else {
            ColorMode::detect_from(var)
        };
        let ascii = force_ascii || !locale_is_utf8(var);
        Self::new(mode, ascii, terminal_is_light(var))
    }

    /// Converts a palette entry into the colour this terminal can show.
    fn tone(&self, (hex, ansi16): (u32, Color)) -> Color {
        match self.mode {
            ColorMode::TrueColor => rgb(hex),
            ColorMode::Ansi256 => Color::Indexed(nearest_ansi256(hex)),
            ColorMode::Ansi16 => ansi16,
            ColorMode::Mono => Color::Reset,
        }
    }

    /// The colour of a semantic role in the current background.
    fn role(&self, role: Role) -> Color {
        self.tone(role.entry(self.light))
    }

    /// The `Other` slot colour for the current background.
    fn other_hex(&self) -> u32 {
        if self.light { 0x8A8F98 } else { 0x6B7280 }
    }

    /// The workload slot palette for the current background.
    fn slot_palette(&self) -> &'static [(u32, Color); 8] {
        if self.light {
            &SLOTS_LIGHT
        } else {
            &SLOTS_DARK
        }
    }

    /// A style that is coloured in colour modes and carries `mono` modifiers in Mono.
    fn paint(&self, role: Role, mono: Modifier) -> Style {
        if self.is_mono() {
            Style::new().add_modifier(mono)
        } else {
            Style::new().fg(self.role(role))
        }
    }
}

/// Semantic UI colours, each with a hex value for truecolor and a named fallback for 16-colour terminals.
#[derive(Clone, Copy, Debug)]
enum Role {
    Text,
    Dim,
    Faint,
    Accent,
    Good,
    Warn,
    Bad,
    Border,
    BorderFocus,
    SelectedBg,
    Ai,
    Empty,
}

impl Role {
    /// The hex value and named fallback for this role on a dark or light background.
    fn entry(self, light: bool) -> (u32, Color) {
        match (self, light) {
            (Self::Text, false) => (0xE6E6E6, Color::White),
            (Self::Text, true) => (0x1F2328, Color::Black),
            (Self::Dim, false) => (0x9AA0A6, Color::Gray),
            (Self::Dim, true) => (0x57606A, Color::DarkGray),
            (Self::Faint, false) => (0x4B5563, Color::DarkGray),
            (Self::Faint, true) => (0x8C959F, Color::Gray),
            (Self::Accent, false) => (0x7DD3FC, Color::LightCyan),
            (Self::Accent, true) => (0x0969DA, Color::Blue),
            (Self::Good, false) => (0x34D399, Color::LightGreen),
            (Self::Good, true) => (0x1A7F37, Color::Green),
            (Self::Warn, false) => (0xFBBF24, Color::LightYellow),
            (Self::Warn, true) => (0x9A6700, Color::Magenta),
            (Self::Bad, false) => (0xF87171, Color::LightRed),
            (Self::Bad, true) => (0xCF222E, Color::Red),
            (Self::Border, false) => (0x3F4654, Color::DarkGray),
            (Self::Border, true) => (0xC9D1D9, Color::Gray),
            (Self::BorderFocus, false) => (0x7DD3FC, Color::LightCyan),
            (Self::BorderFocus, true) => (0x0969DA, Color::Blue),
            (Self::SelectedBg, false) => (0x1F2937, Color::DarkGray),
            (Self::SelectedBg, true) => (0xDDE3EA, Color::Gray),
            (Self::Ai, false) => (0xC4B5FD, Color::LightMagenta),
            (Self::Ai, true) => (0x6639BA, Color::Magenta),
            (Self::Empty, false) => (0x2B313C, Color::DarkGray),
            (Self::Empty, true) => (0xD0D7DE, Color::Gray),
        }
    }
}

const SLOTS_DARK: [(u32, Color); 8] = [
    (0xE69F00, Color::Yellow),
    (0x56B4E9, Color::Cyan),
    (0x009E73, Color::Green),
    (0xF0E442, Color::LightYellow),
    (0x3E8FD6, Color::LightBlue),
    (0xD55E00, Color::Red),
    (0xCC79A7, Color::Magenta),
    (0x9A8CFF, Color::LightGreen),
];

const SLOTS_LIGHT: [(u32, Color); 8] = [
    (0xC27C00, Color::Yellow),
    (0x3581B8, Color::Cyan),
    (0x007A5A, Color::Green),
    (0x8F7A00, Color::LightYellow),
    (0x0072B2, Color::LightBlue),
    (0xB84A00, Color::Red),
    (0xA3457A, Color::Magenta),
    (0x6A4FD0, Color::LightGreen),
];

const CATEGORY_DARK: [(u32, Color); 9] = [
    (0xC084FC, Color::LightMagenta),
    (0x4ADE80, Color::LightGreen),
    (0xFB923C, Color::LightRed),
    (0xF472B6, Color::Magenta),
    (0x60A5FA, Color::LightBlue),
    (0x22D3EE, Color::LightCyan),
    (0xFDE047, Color::LightYellow),
    (0xA8B0BC, Color::Gray),
    (0x7C8593, Color::DarkGray),
];

const CATEGORY_LIGHT: [(u32, Color); 9] = [
    (0x7E3FD1, Color::Magenta),
    (0x1A7F37, Color::Green),
    (0xC2410C, Color::Red),
    (0xBE185D, Color::Magenta),
    (0x1D4ED8, Color::Blue),
    (0x0E7490, Color::Cyan),
    (0xA16207, Color::Yellow),
    (0x57606A, Color::Black),
    (0x8A8F98, Color::DarkGray),
];

/// Position of a category in `CATEGORY_DARK` and `CATEGORY_LIGHT`, matching `Category::ALL`.
fn category_index(category: Category) -> usize {
    match category {
        Category::Ai => 0,
        Category::Game => 1,
        Category::Media => 2,
        Category::Creative => 3,
        Category::Browser => 4,
        Category::Desktop => 5,
        Category::Dev => 6,
        Category::System => 7,
        Category::Other => 8,
    }
}

/// Clamps a heat fraction to 0..=1, mapping NaN to 0 because `f64::clamp` would keep it as NaN.
fn clamped_fraction(fraction: f64) -> f64 {
    if fraction.is_nan() {
        0.0
    } else {
        fraction.clamp(0.0, 1.0)
    }
}

/// Reads one process environment variable, treating unset and non-Unicode values as absent.
fn real_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Splits a `0xRRGGBB` value into a truecolor `Color`.
fn rgb(hex: u32) -> Color {
    Color::Rgb(channel(hex, 16), channel(hex, 8), channel(hex, 0))
}

/// One 8-bit channel of a `0xRRGGBB` value, selected by its bit shift.
fn channel(hex: u32, shift: u32) -> u8 {
    ((hex >> shift) & 0xFF) as u8
}

/// The six intensity levels of the xterm-256 colour cube.
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The xterm-256 indexed colour closest to a `0xRRGGBB` value, searching both the colour cube and the grey ramp.
fn nearest_ansi256(hex: u32) -> u8 {
    let rgb = [channel(hex, 16), channel(hex, 8), channel(hex, 0)];
    let cube = nearest_cube(rgb);
    let grey = nearest_grey(rgb);
    if distance_sq(rgb, cube.1) <= distance_sq(rgb, grey.1) {
        cube.0
    } else {
        grey.0
    }
}

/// The nearest colour-cube entry per channel, as its palette index and its RGB value.
fn nearest_cube(rgb: [u8; 3]) -> (u8, [u8; 3]) {
    let [r, g, b] = rgb.map(nearest_level);
    let index = 16 + 36 * r + 6 * g + b;
    let levels = [
        CUBE_LEVELS[usize::from(r)],
        CUBE_LEVELS[usize::from(g)],
        CUBE_LEVELS[usize::from(b)],
    ];
    (index, levels)
}

/// Index into `CUBE_LEVELS` whose level is closest to one channel value.
fn nearest_level(channel: u8) -> u8 {
    CUBE_LEVELS
        .iter()
        .enumerate()
        .min_by_key(|(_, level)| (i32::from(**level) - i32::from(channel)).abs())
        .map_or(0, |(index, _)| index as u8)
}

/// The nearest grey-ramp entry to the channel average, as its palette index and its RGB value.
fn nearest_grey(rgb: [u8; 3]) -> (u8, [u8; 3]) {
    let average = (u32::from(rgb[0]) + u32::from(rgb[1]) + u32::from(rgb[2])) / 3;
    let step = ((average.saturating_sub(8) + 5) / 10).min(23) as u8;
    let level = 8 + 10 * step;
    (232 + step, [level, level, level])
}

/// Squared Euclidean distance between two RGB triples.
fn distance_sq(a: [u8; 3], b: [u8; 3]) -> i32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| {
            let delta = i32::from(*x) - i32::from(y);
            delta * delta
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key: &str| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_string())
        }
    }

    fn all_slots(theme: &Theme) -> Vec<Color> {
        let mut colours: Vec<Color> = (0..COLOR_SLOTS)
            .map(|index| theme.slot(Slot::Color(index)))
            .collect();
        colours.push(theme.slot(Slot::Other));
        colours
    }

    #[test]
    fn detect_from_table() {
        let cases: &[(&[(&str, &str)], ColorMode)] = &[
            (&[("NO_COLOR", "")], ColorMode::Ansi16),
            (
                &[("NO_COLOR", ""), ("COLORTERM", "truecolor")],
                ColorMode::TrueColor,
            ),
            (
                &[("NO_COLOR", "1"), ("COLORTERM", "truecolor")],
                ColorMode::Mono,
            ),
            (
                &[("NO_COLOR", "anything"), ("TERM", "xterm-256color")],
                ColorMode::Mono,
            ),
            (&[("COLORTERM", "truecolor")], ColorMode::TrueColor),
            (&[("COLORTERM", "TrueColor")], ColorMode::TrueColor),
            (&[("COLORTERM", "24bit")], ColorMode::TrueColor),
            (
                &[("COLORTERM", "24BIT"), ("TERM", "dumb")],
                ColorMode::TrueColor,
            ),
            (&[("TERM", "dumb")], ColorMode::Ansi16),
            (&[], ColorMode::Ansi16),
            (&[("TERM", "")], ColorMode::Ansi16),
            (&[("TERM", "xterm")], ColorMode::Ansi16),
            (&[("TERM", "linux")], ColorMode::Ansi16),
            (&[("TERM", "xterm-256color")], ColorMode::Ansi256),
            (&[("TERM", "screen-256color")], ColorMode::Ansi256),
            (
                &[("COLORTERM", ""), ("TERM", "xterm-256color")],
                ColorMode::Ansi256,
            ),
            (&[("TERM", "xterm-kitty")], ColorMode::TrueColor),
            (&[("TERM", "foot")], ColorMode::TrueColor),
            (&[("TERM", "alacritty")], ColorMode::TrueColor),
            (&[("TERM", "wezterm")], ColorMode::TrueColor),
            (&[("TERM", "ghostty")], ColorMode::TrueColor),
            (&[("TERM", "xterm-ghostty")], ColorMode::TrueColor),
        ];
        for (pairs, expected) in cases {
            let env = vars(pairs);
            assert_eq!(ColorMode::detect_from(&env), *expected, "{pairs:?}");
        }
    }

    #[test]
    fn terminal_is_light_table() {
        let cases: &[(&[(&str, &str)], bool)] = &[
            (&[("COLORFGBG", "0;15")], true),
            (&[("COLORFGBG", "0;7")], true),
            (&[("COLORFGBG", "0;default;15")], true),
            (&[("COLORFGBG", "15;0")], false),
            (&[("COLORFGBG", "0;default")], false),
            (&[("COLORFGBG", "0;250")], false),
            (&[("COLORFGBG", "garbage")], false),
            (&[("COLORFGBG", "")], false),
            (&[], false),
        ];
        for (pairs, expected) in cases {
            let env = vars(pairs);
            assert_eq!(terminal_is_light(&env), *expected, "{pairs:?}");
        }
    }

    #[test]
    fn locale_is_utf8_table() {
        let cases: &[(&[(&str, &str)], bool)] = &[
            (&[], true),
            (&[("LC_ALL", "C")], false),
            (&[("LANG", "en_US.UTF-8")], true),
            (&[("LANG", "C.utf8")], true),
            (&[("LC_ALL", "en_US.UTF8")], true),
            (&[("LC_ALL", ""), ("LANG", "C")], false),
            (
                &[("LC_CTYPE", "de_DE.ISO-8859-1"), ("LANG", "en_US.UTF-8")],
                false,
            ),
            (
                &[("LC_ALL", ""), ("LC_CTYPE", ""), ("LANG", "en_US.UTF-8")],
                true,
            ),
            (&[("LC_ALL", "POSIX"), ("LANG", "en_US.UTF-8")], false),
        ];
        for (pairs, expected) in cases {
            let env = vars(pairs);
            assert_eq!(locale_is_utf8(&env), *expected, "{pairs:?}");
        }
    }

    #[test]
    fn slots_and_other_are_distinct_in_colour_modes() {
        for mode in [ColorMode::TrueColor, ColorMode::Ansi256, ColorMode::Ansi16] {
            for light in [false, true] {
                let colours = all_slots(&Theme::new(mode, false, light));
                for (index, first) in colours.iter().enumerate() {
                    for second in colours.iter().skip(index + 1) {
                        assert_ne!(first, second, "{mode:?} light={light}");
                    }
                }
            }
        }
    }

    #[test]
    fn mono_emits_reset_colours_everywhere() {
        for light in [false, true] {
            let theme = Theme::new(ColorMode::Mono, false, light);
            assert!(
                all_slots(&theme)
                    .iter()
                    .all(|colour| *colour == Color::Reset)
            );
            assert!(
                Category::ALL
                    .into_iter()
                    .all(|category| theme.category(category) == Color::Reset)
            );
            assert_eq!(theme.empty_cell(), Color::Reset);
        }
    }

    #[test]
    fn mono_styles_use_modifiers() {
        let theme = Theme::new(ColorMode::Mono, false, false);
        assert_eq!(
            theme.selected(),
            Style::new().add_modifier(Modifier::REVERSED)
        );
        assert_eq!(theme.title(), Style::new().add_modifier(Modifier::BOLD));
        assert_eq!(
            theme.bad(),
            Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        );
        assert_eq!(theme.dim(), Style::new().add_modifier(Modifier::DIM));
        assert_eq!(theme.faint(), Style::new().add_modifier(Modifier::DIM));
        assert_eq!(theme.accent(), Style::new().add_modifier(Modifier::BOLD));
    }

    #[test]
    fn heat_thresholds_and_clamping() {
        let theme = Theme::new(ColorMode::TrueColor, false, false);
        assert_eq!(theme.heat(0.0), theme.good());
        assert_eq!(theme.heat(0.59), theme.good());
        assert_eq!(theme.heat(0.6), theme.warn());
        assert_eq!(theme.heat(0.84), theme.warn());
        assert_eq!(theme.heat(0.85), theme.bad());
        assert_eq!(theme.heat(1.0), theme.bad());
        assert_eq!(theme.heat(f64::NAN), theme.good());
        assert_eq!(theme.heat(-5.0), theme.good());
        assert_eq!(theme.heat(f64::NEG_INFINITY), theme.good());
        assert_eq!(theme.heat(5.0), theme.bad());
        assert_eq!(theme.heat(f64::INFINITY), theme.bad());
    }

    #[test]
    fn ascii_glyphs_are_all_ascii() {
        let glyphs = Theme::new(ColorMode::Mono, true, false).glyphs;
        let chars: Vec<char> = glyphs
            .h_eighths
            .into_iter()
            .chain(glyphs.fills)
            .chain([glyphs.full])
            .collect();
        assert!(chars.iter().all(|c| c.is_ascii()), "{chars:?}");
        let strings = [
            glyphs.busy,
            glyphs.working,
            glyphs.light,
            glyphs.idle,
            glyphs.ai_mark,
            glyphs.arrow_up,
            glyphs.arrow_down,
            glyphs.selected,
            glyphs.sep,
        ];
        assert!(strings.iter().all(|s| s.is_ascii()), "{strings:?}");
    }

    #[test]
    fn unicode_glyphs_use_block_elements() {
        let glyphs = Theme::new(ColorMode::TrueColor, false, false).glyphs;
        assert_eq!(glyphs.h_eighths[4], '▌');
        assert_eq!(glyphs.full, '█');
        assert_eq!(glyphs.busy, "●");
    }

    #[test]
    fn new_swaps_glyph_set_with_ascii_flag() {
        assert_eq!(
            Theme::new(ColorMode::TrueColor, true, false).glyphs.full,
            '#'
        );
        assert_eq!(
            Theme::new(ColorMode::TrueColor, false, false).glyphs.full,
            '█'
        );
    }

    #[test]
    fn out_of_range_slot_takes_other_colour() {
        for mode in [
            ColorMode::TrueColor,
            ColorMode::Ansi256,
            ColorMode::Ansi16,
            ColorMode::Mono,
        ] {
            let theme = Theme::new(mode, false, false);
            assert_eq!(
                theme.slot(Slot::Color(200)),
                theme.slot(Slot::Other),
                "{mode:?}"
            );
        }
        let truecolor = Theme::new(ColorMode::TrueColor, false, false);
        assert_eq!(
            truecolor.slot(Slot::Color(200)),
            Color::Rgb(0x6B, 0x72, 0x80)
        );
    }

    #[test]
    fn nearest_ansi256_hits_known_indices() {
        assert_eq!(nearest_ansi256(0x000000), 16);
        assert_eq!(nearest_ansi256(0xFFFFFF), 231);
        assert_eq!(nearest_ansi256(0x808080), 244);
    }

    #[test]
    fn activity_style_maps_labels() {
        let theme = Theme::new(ColorMode::TrueColor, false, false);
        assert_eq!(
            theme.activity_style("busy"),
            theme.accent().add_modifier(Modifier::BOLD)
        );
        assert_eq!(theme.activity_style("working"), theme.text());
        assert_eq!(theme.activity_style("light"), theme.dim());
        assert_eq!(theme.activity_style("idle"), theme.faint());
        assert_eq!(theme.activity_style("unknown"), theme.text());
    }

    #[test]
    fn from_env_applies_overrides() {
        let truecolor = vars(&[("COLORTERM", "truecolor"), ("LANG", "en_US.UTF-8")]);
        let theme = Theme::from_env(&truecolor, false, false);
        assert_eq!(theme.mode, ColorMode::TrueColor);
        assert!(!theme.ascii);
        assert!(!theme.light);
        assert!(Theme::from_env(&truecolor, false, true).is_mono());
        assert!(Theme::from_env(&truecolor, true, false).ascii);

        let no_color = vars(&[("NO_COLOR", "1")]);
        assert!(Theme::from_env(&no_color, false, false).is_mono());

        let c_locale = vars(&[("LANG", "C"), ("COLORFGBG", "0;15")]);
        let theme = Theme::from_env(&c_locale, false, false);
        assert!(theme.ascii);
        assert!(theme.light);
    }
}
