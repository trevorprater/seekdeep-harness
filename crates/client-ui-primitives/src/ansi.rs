//! Terminal output replay and ANSI style projection used by `TerminalBlock`.

use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthChar as _;

const ESC: char = '\u{1b}';
const TAB_WIDTH: usize = 8;

/// Inline CSS fields produced by the source ANSI renderer.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnsiStyle {
    /// Resolved foreground token or literal RGB value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Literal RGB background.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_color: Option<String>,
    /// Bold weight.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_weight: Option<u16>,
    /// Dim opacity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    /// Italic style.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_style: Option<String>,
    /// Underline or strikethrough, whichever was declared later.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_decoration: Option<String>,
    /// Hidden visibility.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
}

impl AnsiStyle {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// One plain-text run and its optional resolved style.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnsiSpan {
    /// Escape-free run text.
    pub text: String,
    /// Absent when no SGR state affects rendering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<AnsiStyle>,
}

/// Ordered spans for one output line.
pub type AnsiLine = Vec<AnsiSpan>;

#[derive(Clone, Debug, PartialEq, Eq)]
enum ColorSpec {
    Basic(u8),
    Palette(u8),
    Rgb(u8, u8, u8),
}

impl ColorSpec {
    fn rgb(&self) -> (u8, u8, u8) {
        match *self {
            Self::Basic(code) => basic_rgb(code),
            Self::Palette(index) => palette_rgb(index),
            Self::Rgb(red, green, blue) => (red, green, blue),
        }
    }

    fn text(&self) -> String {
        let (red, green, blue) = self.rgb();
        if matches!(self, Self::Basic(37 | 97)) {
            format!("{red},{green},{blue}")
        } else {
            format!("{red}, {green}, {blue}")
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SgrState {
    foreground: Option<ColorSpec>,
    background: Option<ColorSpec>,
    attributes: Vec<u8>,
}

impl SgrState {
    fn fold(&mut self, parameters: &str) {
        let codes = if parameters.is_empty() {
            vec!["0"]
        } else {
            parameters.split(';').collect::<Vec<_>>()
        };
        let mut index = 0;
        while index < codes.len() {
            let code = codes[index];
            if code.is_empty() || code == "0" {
                *self = Self::default();
                index += 1;
                continue;
            }
            if code == "38" || code == "48" {
                let parsed = parse_extended_color(&codes, index);
                if let Some((color, consumed)) = parsed {
                    if code == "38" {
                        self.foreground = Some(color);
                    } else {
                        self.background = Some(color);
                    }
                    index += consumed;
                    continue;
                }
            }
            let numeric = code.parse::<u16>().unwrap_or(u16::MAX);
            match numeric {
                22 => self.attributes.retain(|value| !matches!(value, 1 | 2)),
                23 => self.attributes.retain(|value| *value != 3),
                24 => self.attributes.retain(|value| *value != 4),
                25 => self.attributes.retain(|value| !matches!(value, 5 | 6)),
                27 => self.attributes.retain(|value| *value != 7),
                28 => self.attributes.retain(|value| *value != 8),
                29 => self.attributes.retain(|value| *value != 9),
                39 => self.foreground = None,
                49 => self.background = None,
                30..=37 | 90..=97 => {
                    self.foreground = Some(ColorSpec::Basic(narrow_u8(numeric)));
                }
                40..=47 | 100..=107 => {
                    self.background = Some(ColorSpec::Basic(narrow_u8(numeric - 10)));
                }
                _ if numeric <= u8::MAX.into() => {
                    let numeric = narrow_u8(numeric);
                    if !self.attributes.contains(&numeric) {
                        self.attributes.push(numeric);
                    }
                }
                _ => {}
            }
            index += 1;
        }
    }

    fn style(&self) -> Option<AnsiStyle> {
        let reversed = self.attributes.contains(&7);
        let mut foreground = self.foreground.clone();
        let mut background = self.background.clone();
        if reversed {
            std::mem::swap(&mut foreground, &mut background);
            if foreground.is_none() {
                foreground = Some(ColorSpec::Basic(30));
            }
        }
        let mut style = AnsiStyle::default();
        if let Some(background) = background.as_ref() {
            style.background_color = Some(format!("rgb({})", background.text()));
        }
        if let Some(foreground) = foreground.as_ref() {
            let literal = format!("rgb({})", foreground.text());
            style.color = if background.is_none() {
                basic_token(foreground).map_or(Some(literal), |token| Some(token.to_owned()))
            } else {
                Some(literal)
            };
        }
        for attribute in &self.attributes {
            match attribute {
                1 => style.font_weight = Some(700),
                2 => style.opacity = Some(0.7),
                3 => style.font_style = Some("italic".to_owned()),
                4 => style.text_decoration = Some("underline".to_owned()),
                8 => style.visibility = Some("hidden".to_owned()),
                9 => style.text_decoration = Some("line-through".to_owned()),
                _ => {}
            }
        }
        (!style.is_empty()).then_some(style)
    }
}

#[derive(Clone, Debug)]
struct Cell {
    state: SgrState,
    text: String,
    spacer: bool,
}

impl Cell {
    fn painted(state: &SgrState, text: impl Into<String>) -> Self {
        Self {
            state: state.clone(),
            text: text.into(),
            spacer: false,
        }
    }
}

/// Parses command output into terminal-painted, styled lines.
///
/// Cursor movement is replayed before inert controls are removed. SGR state
/// crosses newlines and is stored per painted terminal cell.
#[must_use]
pub fn parse_ansi_lines(text: &str) -> Vec<AnsiLine> {
    let escaped = strip_non_color_escapes(text);
    let raw_lines = escaped.split('\n').collect::<Vec<_>>();
    let mut state = SgrState::default();
    let mut lines = Vec::with_capacity(raw_lines.len());
    for raw in raw_lines {
        let line = raw.trim_end_matches('\r');
        let (spans, next) = if needs_replay(line) {
            replay_line(line, &state)
        } else {
            parse_linear_line(line, &state)
        };
        lines.push(spans);
        state = next;
    }
    lines
}

fn replay_line(line: &str, entry_state: &SgrState) -> (AnsiLine, SgrState) {
    let mut columns = Vec::<Option<Cell>>::new();
    let mut cursor = 0_usize;
    let mut state = entry_state.clone();
    let mut at = 0_usize;
    for sequence in csi_sequences(line) {
        consume_cells(&line[at..sequence.start], &mut columns, &mut cursor, &state);
        at = sequence.end;
        match sequence.final_byte {
            'K' => erase_line(&mut columns, cursor, &state, sequence.parameters),
            'm' => state.fold(sequence.parameters),
            _ => {}
        }
    }
    consume_cells(&line[at..], &mut columns, &mut cursor, &state);
    (cells_to_spans(&columns), state)
}

fn consume_cells(
    text: &str,
    columns: &mut Vec<Option<Cell>>,
    cursor: &mut usize,
    state: &SgrState,
) {
    for character in text.chars() {
        match character {
            '\r' => *cursor = 0,
            '\u{8}' => *cursor = cursor.saturating_sub(1),
            '\t' => {
                let stop = *cursor + TAB_WIDTH - (*cursor % TAB_WIDTH);
                while *cursor < stop {
                    ensure_cell(columns, *cursor);
                    if columns[*cursor].is_none() {
                        columns[*cursor] = Some(Cell::painted(state, " "));
                    }
                    *cursor += 1;
                }
            }
            _ if is_zero_width(character) => {
                if *cursor > 0
                    && let Some(base) = columns.get_mut(*cursor - 1).and_then(Option::as_mut)
                {
                    base.text.push(character);
                }
            }
            _ => {
                clear_cell(columns, *cursor, state, " ");
                columns[*cursor] = Some(Cell::painted(state, character.to_string()));
                *cursor += 1;
                if is_wide(character) {
                    ensure_cell(columns, *cursor);
                    columns[*cursor] = Some(Cell {
                        state: state.clone(),
                        text: String::new(),
                        spacer: true,
                    });
                    *cursor += 1;
                }
            }
        }
    }
}

fn erase_line(columns: &mut Vec<Option<Cell>>, cursor: usize, state: &SgrState, parameters: &str) {
    match parameters.split(';').next().unwrap_or_default() {
        "1" => {
            for index in 0..=cursor {
                clear_cell(columns, index, state, " ");
            }
        }
        "2" => columns.clear(),
        _ => columns.resize(cursor, None),
    }
}

fn clear_cell(columns: &mut Vec<Option<Cell>>, index: usize, state: &SgrState, fill: &str) {
    ensure_cell(columns, index);
    let spacer = columns[index].as_ref().is_some_and(|cell| cell.spacer);
    if spacer && index > 0 {
        columns[index - 1] = Some(Cell::painted(state, fill));
    } else if columns[index]
        .as_ref()
        .and_then(|cell| cell.text.chars().next())
        .is_some_and(is_wide)
    {
        ensure_cell(columns, index + 1);
        if columns[index + 1].as_ref().is_some_and(|cell| cell.spacer) {
            columns[index + 1] = Some(Cell::painted(state, fill));
        }
    }
    columns[index] = Some(Cell::painted(state, fill));
}

fn ensure_cell(columns: &mut Vec<Option<Cell>>, index: usize) {
    if columns.len() <= index {
        columns.resize(index + 1, None);
    }
}

fn cells_to_spans(columns: &[Option<Cell>]) -> AnsiLine {
    let mut spans = Vec::<AnsiSpan>::new();
    let mut last_emitted_state = None::<SgrState>;
    for (index, cell) in columns.iter().enumerate() {
        let fallback;
        let cell = if let Some(cell) = cell {
            cell
        } else {
            fallback = Cell::painted(&SgrState::default(), " ");
            &fallback
        };
        let lead_intact = index > 0
            && columns[index - 1]
                .as_ref()
                .and_then(|lead| lead.text.chars().next())
                .is_some_and(is_wide);
        let text = if cell.spacer && !lead_intact {
            " "
        } else {
            &cell.text
        };
        let filtered = visible_text(text);
        if filtered.is_empty() {
            continue;
        }
        if last_emitted_state.as_ref() == Some(&cell.state) {
            spans
                .last_mut()
                .expect("an emitted state owns a span")
                .text
                .push_str(&filtered);
        } else {
            spans.push(AnsiSpan {
                text: filtered,
                style: cell.state.style(),
            });
            last_emitted_state = Some(cell.state.clone());
        }
    }
    spans
}

fn parse_linear_line(line: &str, entry_state: &SgrState) -> (AnsiLine, SgrState) {
    let mut spans = Vec::new();
    let mut state = entry_state.clone();
    let mut at = 0_usize;
    for sequence in csi_sequences(line) {
        push_visible_text(&mut spans, &line[at..sequence.start], state.style());
        at = sequence.end;
        if sequence.final_byte == 'm' {
            state.fold(sequence.parameters);
        }
    }
    push_visible_text(&mut spans, &line[at..], state.style());
    (spans, state)
}

fn push_visible_text(spans: &mut Vec<AnsiSpan>, text: &str, style: Option<AnsiStyle>) {
    let filtered = visible_text(text);
    if filtered.is_empty() {
        return;
    }
    spans.push(AnsiSpan {
        text: filtered,
        style,
    });
}

fn visible_text(text: &str) -> String {
    text.chars()
        .filter(|character| !is_inert_control(*character))
        .collect()
}

#[derive(Clone, Copy)]
struct Csi<'a> {
    start: usize,
    end: usize,
    parameters: &'a str,
    final_byte: char,
}

fn csi_sequences(text: &str) -> Vec<Csi<'_>> {
    let bytes = text.as_bytes();
    let mut output = Vec::new();
    let mut index = 0;
    while index + 2 < bytes.len() {
        if bytes[index] != ESC as u8 || bytes[index + 1] != b'[' {
            index += text[index..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let parameter_start = index + 2;
        let mut cursor = parameter_start;
        while cursor < bytes.len() && (0x30..=0x3f).contains(&bytes[cursor]) {
            cursor += 1;
        }
        let parameter_end = cursor;
        while cursor < bytes.len() && (0x20..=0x2f).contains(&bytes[cursor]) {
            cursor += 1;
        }
        if cursor < bytes.len() && (0x40..=0x7e).contains(&bytes[cursor]) {
            output.push(Csi {
                start: index,
                end: cursor + 1,
                parameters: &text[parameter_start..parameter_end],
                final_byte: bytes[cursor] as char,
            });
            index = cursor + 1;
        } else {
            index += 1;
        }
    }
    output
}

fn needs_replay(line: &str) -> bool {
    line.contains(['\r', '\u{8}'])
        || csi_sequences(line)
            .iter()
            .any(|sequence| sequence.final_byte == 'K')
}

fn strip_non_color_escapes(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != ESC as u8 {
            let character = text[index..].chars().next().expect("valid UTF-8 boundary");
            output.push(character);
            index += character.len_utf8();
            continue;
        }
        if bytes.get(index + 1) == Some(&b'[') {
            output.push(ESC);
            index += 1;
            continue;
        }
        if bytes.get(index + 1) == Some(&b']') {
            let mut cursor = index + 2;
            while cursor < bytes.len() && !matches!(bytes[cursor], 0x07 | 0x1b) {
                cursor += 1;
            }
            if cursor >= bytes.len() {
                break;
            }
            if bytes[cursor] == 0x07 {
                index = cursor + 1;
            } else if bytes.get(cursor + 1) == Some(&b'\\') {
                index = cursor + 2;
            } else {
                index = cursor;
            }
            continue;
        }
        let mut cursor = index + 1;
        while cursor < bytes.len() && (0x20..=0x2f).contains(&bytes[cursor]) {
            cursor += 1;
        }
        if cursor < bytes.len() && (0x30..=0x7e).contains(&bytes[cursor]) {
            cursor += 1;
        }
        index = cursor;
    }
    output
}

fn parse_extended_color(codes: &[&str], index: usize) -> Option<(ColorSpec, usize)> {
    match codes.get(index + 1).copied() {
        Some("5") => Some((ColorSpec::Palette(codes.get(index + 2)?.parse().ok()?), 3)),
        Some("2") => Some((
            ColorSpec::Rgb(
                codes.get(index + 2)?.parse().ok()?,
                codes.get(index + 3)?.parse().ok()?,
                codes.get(index + 4)?.parse().ok()?,
            ),
            5,
        )),
        _ => None,
    }
}

fn basic_rgb(code: u8) -> (u8, u8, u8) {
    const NORMAL: [(u8, u8, u8); 8] = [
        (0, 0, 0),
        (187, 0, 0),
        (0, 187, 0),
        (187, 187, 0),
        (0, 0, 187),
        (187, 0, 187),
        (0, 187, 187),
        (255, 255, 255),
    ];
    const BRIGHT: [(u8, u8, u8); 8] = [
        (85, 85, 85),
        (255, 85, 85),
        (0, 255, 0),
        (255, 255, 85),
        (85, 85, 255),
        (255, 85, 255),
        (85, 255, 255),
        (255, 255, 255),
    ];
    match code {
        30..=37 => NORMAL[(code - 30) as usize],
        90..=97 => BRIGHT[(code - 90) as usize],
        _ => (0, 0, 0),
    }
}

fn narrow_u8(value: u16) -> u8 {
    u8::try_from(value).expect("ANSI code is known to fit in u8")
}

fn palette_rgb(index: u8) -> (u8, u8, u8) {
    if index < 8 {
        return basic_rgb(30 + index);
    }
    if index < 16 {
        return basic_rgb(90 + index - 8);
    }
    if index < 232 {
        const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let value = index - 16;
        return (
            CUBE[(value / 36) as usize],
            CUBE[((value / 6) % 6) as usize],
            CUBE[(value % 6) as usize],
        );
    }
    let gray = 8 + (index - 232) * 10;
    (gray, gray, gray)
}

fn basic_token(color: &ColorSpec) -> Option<&'static str> {
    match color.rgb() {
        (0, 0, 0) | (255, 255, 255) => Some("var(--dsw-alias-label-primary)"),
        (85, 85, 85) => Some("var(--dsw-alias-label-tertiary)"),
        (187, 0, 0) => Some("var(--dsw-alias-state-error-primary)"),
        (255, 85, 85) => Some("var(--dsw-alias-state-error-secondary)"),
        (0, 187, 0) => Some("var(--dsw-alias-state-success-primary)"),
        (0, 255, 0) => Some("var(--dsw-alias-state-success-secondary)"),
        (187, 187, 0) => Some("var(--dsw-alias-state-warn-primary)"),
        (255, 255, 85) => Some("var(--dsw-alias-state-warn-secondary)"),
        (0, 0, 187) => Some("var(--dsw-alias-state-business-primary)"),
        (85, 85, 255) => Some("var(--dsw-static-blue-400)"),
        _ => None,
    }
}

fn is_zero_width(character: char) -> bool {
    !character.is_control() && character.width().unwrap_or(1) == 0
}

fn is_wide(character: char) -> bool {
    character.width() == Some(2)
}

fn is_inert_control(character: char) -> bool {
    matches!(character as u32, 0x00..=0x07 | 0x0b..=0x1a | 0x1c..=0x1f | 0x7f)
}
