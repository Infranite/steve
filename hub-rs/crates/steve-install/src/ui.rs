//! The terminal interface of the `steve` CLI. On a terminal it is a framed wizard: prompts chosen
//! with the arrow keys, steps behind a spinner, a summary box. Everywhere else (a pipe,
//! `TERM=dumb`, `STEVE_PLAIN=1`) the same calls print and read plain lines, so scripts and logs
//! keep working. `NO_COLOR` keeps the frame and drops the colours.
use anyhow::Result;
use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use steve_i18n::t;

/// The user left a prompt (Ctrl-C, Ctrl-D): already said on the terminal, exit code 130.
#[derive(Debug)]
pub struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&t!("install.ui.cancelled_error"))
    }
}
impl std::error::Error for Cancelled {}

fn set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// Standard output is a terminal that understands cursor movement: styled output is in order.
pub fn decorated() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = io::stdout().is_terminal()
            && !set("STEVE_PLAIN")
            && std::env::var("TERM").is_ok_and(|t| !t.is_empty() && t != "dumb");
        if on {
            /* Ctrl-C during a step or a child's run: give the cursor back before leaving */
            extern "C" fn interrupted(_: libc::c_int) {
                const RESTORE: &[u8] = b"\x1b[?25h\x1b[0m\n";
                unsafe {
                    libc::write(1, RESTORE.as_ptr().cast(), RESTORE.len());
                    libc::_exit(130);
                }
            }
            let handler = interrupted as extern "C" fn(libc::c_int);
            unsafe { libc::signal(libc::SIGINT, handler as *const () as libc::sighandler_t) };
        }
        on
    })
}

/// Both ends are a terminal: prompts are interactive widgets.
pub fn fancy() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| decorated() && io::stdin().is_terminal())
}

fn colours() -> bool {
    decorated() && !set("NO_COLOR")
}

fn truecolour() -> bool {
    std::env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit"))
}

#[derive(Clone, Copy, PartialEq)]
pub enum Tone {
    Accent,
    Sky,
    Green,
    Yellow,
    Red,
    Dim,
    Bold,
}

/// `text` in `tone`, or as it is where colours are off.
pub fn paint(tone: Tone, text: &str) -> String {
    if !colours() {
        return text.to_owned();
    }
    let rgb = |r: u8, g: u8, b: u8, fallback: u8| {
        if truecolour() {
            format!("\x1b[38;2;{r};{g};{b}m{text}\x1b[0m")
        } else {
            format!("\x1b[38;5;{fallback}m{text}\x1b[0m")
        }
    };
    match tone {
        Tone::Accent => rgb(167, 139, 250, 141),
        Tone::Sky => rgb(56, 189, 248, 75),
        Tone::Green => rgb(74, 222, 128, 78),
        Tone::Yellow => rgb(250, 204, 21, 220),
        Tone::Red => rgb(248, 113, 113, 203),
        Tone::Dim => format!("\x1b[2m{text}\x1b[0m"),
        Tone::Bold => format!("\x1b[1m{text}\x1b[0m"),
    }
}

fn char_width(c: char) -> usize {
    match c as u32 {
        0x0300..=0x036F | 0x200B..=0x200F | 0xFE00..=0xFE0F => 0,
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// The columns `text` takes on a terminal; escape sequences take none, CJK characters two.
pub fn width(text: &str) -> usize {
    let mut total = 0;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for e in chars.by_ref() {
                if e.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            total += char_width(c);
        }
    }
    total
}

fn columns() -> usize {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut size) } == 0;
    if ok && size.ws_col >= 20 {
        size.ws_col as usize
    } else {
        80
    }
}

/// Unstyled `text` cut to `max` columns, with an ellipsis when something was cut.
fn clip(text: &str, max: usize) -> String {
    if width(text) <= max {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        if used + char_width(c) + 1 > max {
            break;
        }
        used += char_width(c);
        out.push(c);
    }
    out.push('…');
    out
}

/// Unstyled `text` in lines of at most `max` columns: broken at a space where one is near, else
/// between any two characters (Chinese has no spaces to break at).
fn wrap(text: &str, max: usize) -> Vec<String> {
    let max = max.max(10);
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        let mut used = 0;
        for c in paragraph.chars() {
            if used + char_width(c) > max {
                let at = line
                    .rfind(' ')
                    .filter(|at| c != ' ' && width(&line[*at..]) <= 24);
                let rest = match at {
                    Some(at) => {
                        let rest = line[at + 1..].to_owned();
                        line.truncate(at);
                        rest
                    }
                    None => String::new(),
                };
                lines.push(std::mem::replace(&mut line, rest));
                used = width(&line);
                if c == ' ' {
                    continue;
                }
            }
            used += char_width(c);
            line.push(c);
        }
        lines.push(line);
    }
    lines
}

fn pad(text: &str, to: usize) -> String {
    format!("{text}{}", " ".repeat(to.saturating_sub(width(text))))
}

fn bar() -> String {
    paint(Tone::Dim, "│")
}

/// Whether the last line of the frame is an empty one: a prompt or a heading opens after one.
static SPACED: AtomicBool = AtomicBool::new(false);

fn out(text: &str) {
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
    SPACED.store(false, Ordering::SeqCst);
}

fn spaced() {
    if !SPACED.load(Ordering::SeqCst) {
        gap();
    }
}

const LOGO: [&str; 6] = [
    "███████╗████████╗███████╗██╗   ██╗███████╗",
    "██╔════╝╚══██╔══╝██╔════╝██║   ██║██╔════╝",
    "███████╗   ██║   █████╗  ██║   ██║█████╗  ",
    "╚════██║   ██║   ██╔══╝  ╚██╗ ██╔╝██╔══╝  ",
    "███████║   ██║   ███████╗ ╚████╔╝ ███████╗",
    "╚══════╝   ╚═╝   ╚══════╝  ╚═══╝  ╚══════╝",
];

/// The logo, coloured left to right, with `tagline` under it. Nothing off a terminal.
pub fn banner(tagline: &str) {
    if !decorated() {
        return;
    }
    let mut text = String::from("\n");
    if columns() >= 48 {
        const STOPS: [(f32, f32, f32); 3] =
            [(244., 114., 182.), (167., 139., 250.), (56., 189., 248.)];
        const BANDS: [u8; 6] = [211, 177, 141, 105, 75, 45];
        let span = LOGO[0].chars().count() as f32 - 1.;
        for row in LOGO {
            text.push_str("  ");
            for (at, c) in row.chars().enumerate() {
                if c == ' ' || !colours() {
                    text.push(c);
                    continue;
                }
                let t = at as f32 / span;
                if truecolour() {
                    let (from, to, local) = if t < 0.5 {
                        (STOPS[0], STOPS[1], t * 2.)
                    } else {
                        (STOPS[1], STOPS[2], (t - 0.5) * 2.)
                    };
                    let mix = |a: f32, b: f32| (a + (b - a) * local).round() as u8;
                    text.push_str(&format!(
                        "\x1b[38;2;{};{};{}m{c}",
                        mix(from.0, to.0),
                        mix(from.1, to.1),
                        mix(from.2, to.2)
                    ));
                } else {
                    let band = BANDS[((t * BANDS.len() as f32) as usize).min(BANDS.len() - 1)];
                    text.push_str(&format!("\x1b[38;5;{band}m{c}"));
                }
            }
            text.push_str(if colours() { "\x1b[0m\n" } else { "\n" });
        }
    } else {
        text.push_str(&format!(
            "  {}\n",
            paint(Tone::Accent, &paint(Tone::Bold, "STEVE"))
        ));
    }
    text.push_str(&format!("  {}\n\n", paint(Tone::Dim, tagline)));
    out(&text);
}

/// Opens the frame. Nothing off a terminal.
pub fn intro(title: &str) {
    if decorated() {
        out(&format!(
            "{}  {}\n{}\n",
            paint(Tone::Dim, "┌"),
            paint(Tone::Bold, title),
            bar()
        ));
        SPACED.store(true, Ordering::SeqCst);
    }
}

/// Closes the frame.
pub fn outro(text: &str) {
    if decorated() {
        spaced();
        let room = columns().saturating_sub(4);
        let mut lines = wrap(text, room).into_iter();
        let first = lines.next().unwrap_or_default();
        let mut block = format!("{}  {first}\n", paint(Tone::Dim, "└"));
        for line in lines {
            block.push_str(&format!("   {line}\n"));
        }
        block.push('\n');
        out(&block);
    } else if !text.is_empty() {
        println!("{text}");
    }
}

/// An empty line of the frame.
pub fn gap() {
    if decorated() {
        out(&format!("{}\n", bar()));
        SPACED.store(true, Ordering::SeqCst);
    }
}

/// Running text inside the frame, wrapped to the terminal.
pub fn info(text: &str) {
    if decorated() {
        let room = columns().saturating_sub(4);
        let block: String = wrap(text, room)
            .iter()
            .map(|line| format!("{}  {}\n", bar(), paint(Tone::Dim, line)))
            .collect();
        out(&block);
    } else {
        println!("{text}");
    }
}

fn mark(glyph: &str, tone: Tone, text: &str, detail: &str) {
    if decorated() {
        let room = columns().saturating_sub(4);
        let mut lines = wrap(text, room).into_iter();
        let first = lines.next().unwrap_or_default();
        let detail = if detail.is_empty() || width(&first) + width(detail) + 2 > room {
            String::new()
        } else {
            format!("  {}", paint(Tone::Dim, detail))
        };
        let mut block = format!("{}  {first}{detail}\n", paint(tone, glyph));
        for line in lines {
            block.push_str(&format!("{}  {line}\n", bar()));
        }
        out(&block);
    } else if detail.is_empty() {
        println!("{text}");
    } else {
        println!(
            "{}",
            t!("install.ui.text_with_detail", text = text, detail = detail)
        );
    }
}

/// A finished step.
pub fn done(text: &str, detail: &str) {
    mark("✓", Tone::Green, text, detail);
}

/// Something to know that does not stop the work.
pub fn warn(text: &str) {
    if decorated() {
        mark("▲", Tone::Yellow, text, "");
    } else {
        eprintln!("{text}");
    }
}

/// A failure (the error itself is reported by the caller).
pub fn fail(text: &str) {
    if decorated() {
        mark("✗", Tone::Red, text, "");
    } else {
        eprintln!("{text}");
    }
}

/// A heading inside the frame, like an answered prompt.
pub fn heading(text: &str) {
    if decorated() {
        spaced();
        out(&format!(
            "{}  {}\n",
            paint(Tone::Green, "◇"),
            paint(Tone::Bold, text)
        ));
    } else {
        println!("{text}");
    }
}

/// Label and value rows in a box (plain `label：value` lines off a terminal). A row too wide for
/// the terminal is never cut: the box is dropped instead, so a token stays whole.
pub fn note(rows: &[(&str, String)]) {
    if !decorated() {
        for (label, value) in rows {
            println!("{}", t!("install.ui.row", label = label, value = value));
        }
        return;
    }
    let labels = rows.iter().map(|(l, _)| width(l)).max().unwrap_or(0);
    let values = rows.iter().map(|(_, v)| width(v)).max().unwrap_or(0);
    let inner = labels + 3 + values;
    let mut block = String::new();
    if inner + 8 > columns() {
        for (label, value) in rows {
            block.push_str(&format!(
                "{}  {}  {value}\n",
                bar(),
                paint(Tone::Dim, &pad(label, labels))
            ));
        }
    } else {
        let edge =
            |l: &str, r: &str| paint(Tone::Accent, &format!("{l}{}{r}", "─".repeat(inner + 2)));
        block.push_str(&format!("{}  {}\n", bar(), edge("╭", "╮")));
        for (label, value) in rows {
            block.push_str(&format!(
                "{}  {} {}   {} {}\n",
                bar(),
                paint(Tone::Accent, "│"),
                paint(Tone::Dim, &pad(label, labels)),
                pad(value, values),
                paint(Tone::Accent, "│"),
            ));
        }
        block.push_str(&format!("{}  {}\n", bar(), edge("╰", "╯")));
    }
    out(&block);
}

/// A step in progress: a spinner with the seconds it has taken. Ends with [`Spinner::done`],
/// [`Spinner::warn`] or [`Spinner::fail`]; dropped unfinished, it only clears its line.
pub struct Spinner {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    started: Instant,
}

impl Spinner {
    pub fn start(text: &str) -> Spinner {
        let stop = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let thread = if decorated() {
            let (stop, text) = (stop.clone(), text.to_owned());
            out("\x1b[?25l");
            Some(std::thread::spawn(move || {
                const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let mut at = 0;
                while !stop.load(Ordering::SeqCst) {
                    let seconds = started.elapsed().as_secs();
                    let elapsed = if seconds >= 2 {
                        paint(Tone::Dim, &format!("  {seconds}s"))
                    } else {
                        String::new()
                    };
                    let line = clip(&text, columns().saturating_sub(12));
                    out(&format!(
                        "\r\x1b[K{}  {line}{elapsed}",
                        paint(Tone::Accent, FRAMES[at % FRAMES.len()])
                    ));
                    at += 1;
                    std::thread::sleep(Duration::from_millis(80));
                }
            }))
        } else {
            println!("{text}…");
            None
        };
        Spinner {
            stop,
            thread,
            started,
        }
    }

    fn clear(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
            out("\r\x1b[K\x1b[?25h");
        }
    }

    fn took(&self) -> String {
        match self.started.elapsed().as_secs() {
            0..=2 => String::new(),
            seconds => format!("{seconds}s"),
        }
    }

    pub fn done(mut self, text: &str, detail: &str) {
        self.clear();
        let took = self.took();
        let detail = match (detail.is_empty(), took.is_empty()) {
            (_, true) => detail.to_owned(),
            (true, false) => took,
            (false, false) => format!("{detail} · {took}"),
        };
        done(text, &detail);
    }

    pub fn warn(mut self, text: &str) {
        self.clear();
        warn(text);
    }

    pub fn fail(mut self, text: &str) {
        self.clear();
        fail(text);
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Runs `work` behind a spinner labelled `doing`; `finished` words the line that replaces it.
pub fn step<T>(
    doing: &str,
    work: impl FnOnce() -> Result<T>,
    finished: impl FnOnce(&T) -> (String, String),
) -> Result<T> {
    let spinner = Spinner::start(doing);
    match work() {
        Ok(value) => {
            let (text, detail) = finished(&value);
            spinner.done(&text, &detail);
            Ok(value)
        }
        Err(error) => {
            spinner.fail(&t!("install.ui.step_failed", doing = doing));
            Err(error)
        }
    }
}

/// One option of [`select`] and [`multiselect`]: `key` is what a plain prompt takes as the answer.
pub struct Choice {
    pub key: String,
    pub label: String,
    pub hint: String,
}

impl Choice {
    pub fn new(key: &str, label: &str, hint: &str) -> Choice {
        Choice {
            key: key.into(),
            label: label.into(),
            hint: hint.into(),
        }
    }
}

enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Space,
    Char(char),
    Cancel,
    Other,
}

/// The terminal without line editing, echo or signal keys, until dropped.
struct Raw(libc::termios);

impl Raw {
    fn enter() -> io::Result<Raw> {
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(0, &mut saved) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = saved;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        if unsafe { libc::tcsetattr(0, libc::TCSADRAIN, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        out("\x1b[?25l");
        Ok(Raw(saved))
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(0, libc::TCSADRAIN, &self.0) };
        out("\x1b[?25h");
    }
}

fn byte(wait: Option<i32>) -> io::Result<Option<u8>> {
    if let Some(milliseconds) = wait {
        let mut fd = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut fd, 1, milliseconds) } <= 0 {
            return Ok(None);
        }
    }
    let mut one = [0u8; 1];
    loop {
        match unsafe { libc::read(0, one.as_mut_ptr().cast(), 1) } {
            1 => return Ok(Some(one[0])),
            0 => return Ok(None),
            _ => {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
    }
}

fn key() -> io::Result<Key> {
    Ok(match byte(None)? {
        None | Some(3) | Some(4) => Key::Cancel,
        Some(b'\r') | Some(b'\n') => Key::Enter,
        Some(b' ') => Key::Space,
        Some(b'\t') => Key::Right,
        Some(0x1b) => match byte(Some(40))? {
            Some(b'[') | Some(b'O') => {
                let mut last = byte(Some(40))?;
                /* parameters of a longer sequence, up to its final byte */
                while last.is_some_and(|b| !(0x40..=0x7e).contains(&b)) {
                    last = byte(Some(40))?;
                }
                match last {
                    Some(b'A') => Key::Up,
                    Some(b'B') => Key::Down,
                    Some(b'C') => Key::Right,
                    Some(b'D') => Key::Left,
                    _ => Key::Other,
                }
            }
            _ => Key::Other,
        },
        Some(b) if b.is_ascii_graphic() => Key::Char(b as char),
        Some(_) => Key::Other,
    })
}

/// Draws `lines` over the `drawn` lines of the previous frame.
fn redraw(drawn: &mut usize, lines: &[String]) {
    let mut block = String::new();
    if *drawn > 0 {
        block.push_str(&format!("\x1b[{}A", *drawn));
    }
    block.push_str("\r\x1b[J");
    for line in lines {
        block.push_str(line);
        block.push('\n');
    }
    *drawn = lines.len();
    out(&block);
}

fn answered(drawn: &mut usize, title: &str, answer: &str) {
    let room = columns().saturating_sub(4);
    redraw(
        drawn,
        &[
            format!("{}  {title}", paint(Tone::Green, "◇")),
            format!("{}  {}", bar(), paint(Tone::Dim, &clip(answer, room))),
            bar(),
        ],
    );
    SPACED.store(true, Ordering::SeqCst);
}

fn cancelled(drawn: &mut usize, title: &str) -> anyhow::Error {
    redraw(
        drawn,
        &[
            format!("{}  {title}", paint(Tone::Red, "■")),
            format!(
                "{}  {}",
                paint(Tone::Dim, "└"),
                paint(Tone::Red, &t!("install.ui.cancelled"))
            ),
            String::new(),
        ],
    );
    Cancelled.into()
}

fn option_lines(choices: &[Choice], at: usize, glyph: impl Fn(usize) -> String) -> Vec<String> {
    let room = columns().saturating_sub(6);
    let labels = choices.iter().map(|c| width(&c.label)).max().unwrap_or(0);
    choices
        .iter()
        .enumerate()
        .map(|(i, choice)| {
            let label = pad(&choice.label, labels);
            let hint = clip(&choice.hint, room.saturating_sub(labels + 2));
            let label = if i == at {
                paint(Tone::Bold, &label)
            } else {
                paint(Tone::Dim, &label)
            };
            format!(
                "{}  {} {label}  {}",
                bar(),
                glyph(i),
                paint(Tone::Dim, &hint)
            )
        })
        .collect()
}

/// One of `choices`, with the arrow keys; starts on `initial`.
pub fn select(title: &str, choices: &[Choice], initial: usize) -> Result<usize> {
    spaced();
    let raw = Raw::enter()?;
    let (mut at, mut drawn) = (initial.min(choices.len() - 1), 0);
    loop {
        let mut lines = vec![format!(
            "{}  {}",
            paint(Tone::Accent, "◆"),
            paint(Tone::Bold, title)
        )];
        lines.extend(option_lines(choices, at, |i| {
            if i == at {
                paint(Tone::Accent, "●")
            } else {
                paint(Tone::Dim, "○")
            }
        }));
        lines.push(format!(
            "{}  {}",
            paint(Tone::Dim, "└"),
            paint(Tone::Dim, &t!("install.ui.select_hint"))
        ));
        redraw(&mut drawn, &lines);
        match key()? {
            Key::Up | Key::Left | Key::Char('k') => at = (at + choices.len() - 1) % choices.len(),
            Key::Down | Key::Right | Key::Char('j') => at = (at + 1) % choices.len(),
            Key::Char(c)
                if c.is_ascii_digit()
                    && (1..=choices.len()).contains(&(c as usize - '0' as usize)) =>
            {
                at = c as usize - '1' as usize;
            }
            Key::Enter | Key::Space => break,
            Key::Cancel => {
                drop(raw);
                return Err(cancelled(&mut drawn, title));
            }
            _ => {}
        }
    }
    drop(raw);
    answered(&mut drawn, title, &choices[at].label);
    Ok(at)
}

/// Any of `choices`: space ticks, `a` ticks or clears all, Enter confirms. `ticked` is the start.
pub fn multiselect(title: &str, choices: &[Choice], ticked: &[bool]) -> Result<Vec<bool>> {
    spaced();
    let raw = Raw::enter()?;
    let mut on: Vec<bool> = (0..choices.len())
        .map(|i| ticked.get(i).copied().unwrap_or(true))
        .collect();
    let (mut at, mut drawn) = (0, 0);
    loop {
        let mut lines = vec![format!(
            "{}  {}",
            paint(Tone::Accent, "◆"),
            paint(Tone::Bold, title)
        )];
        lines.extend(option_lines(choices, at, |i| {
            let pointer = if i == at {
                paint(Tone::Accent, "›")
            } else {
                " ".into()
            };
            let tick = if on[i] {
                paint(Tone::Green, "■")
            } else {
                paint(Tone::Dim, "□")
            };
            format!("{pointer} {tick}")
        }));
        lines.push(format!(
            "{}  {}",
            paint(Tone::Dim, "└"),
            paint(Tone::Dim, &t!("install.ui.multiselect_hint"))
        ));
        redraw(&mut drawn, &lines);
        match key()? {
            Key::Up | Key::Left | Key::Char('k') => at = (at + choices.len() - 1) % choices.len(),
            Key::Down | Key::Right | Key::Char('j') => at = (at + 1) % choices.len(),
            Key::Space => on[at] = !on[at],
            Key::Char('a') => {
                let all = on.iter().all(|v| *v);
                on.iter_mut().for_each(|v| *v = !all);
            }
            Key::Enter => break,
            Key::Cancel => {
                drop(raw);
                return Err(cancelled(&mut drawn, title));
            }
            _ => {}
        }
    }
    drop(raw);
    let chosen: Vec<&str> = choices
        .iter()
        .zip(&on)
        .filter(|(_, on)| **on)
        .map(|(c, _)| c.label.as_str())
        .collect();
    let answer = if chosen.is_empty() {
        t!("install.ui.multiselect_none")
    } else {
        chosen.join(&t!("install.ui.list_separator"))
    };
    answered(&mut drawn, title, &answer);
    Ok(on)
}

/// Yes or no; `y`/`n` answer at once.
pub fn confirm(title: &str, initial: bool) -> Result<bool> {
    spaced();
    let raw = Raw::enter()?;
    let (mut yes, mut drawn) = (initial, 0);
    loop {
        let option = |on: bool, text: &str| {
            if on {
                format!("{} {}", paint(Tone::Accent, "●"), paint(Tone::Bold, text))
            } else {
                format!("{} {}", paint(Tone::Dim, "○"), paint(Tone::Dim, text))
            }
        };
        redraw(
            &mut drawn,
            &[
                format!("{}  {}", paint(Tone::Accent, "◆"), paint(Tone::Bold, title)),
                format!(
                    "{}  {}  {}  {}",
                    bar(),
                    option(yes, &t!("install.ui.yes")),
                    paint(Tone::Dim, "/"),
                    option(!yes, &t!("install.ui.no"))
                ),
                format!(
                    "{}  {}",
                    paint(Tone::Dim, "└"),
                    paint(Tone::Dim, &t!("install.ui.confirm_hint"))
                ),
            ],
        );
        match key()? {
            Key::Up
            | Key::Down
            | Key::Left
            | Key::Right
            | Key::Space
            | Key::Char('h' | 'l' | 'j' | 'k') => yes = !yes,
            Key::Char('y' | 'Y') => {
                yes = true;
                break;
            }
            Key::Char('n' | 'N') => {
                yes = false;
                break;
            }
            Key::Enter => break,
            Key::Cancel => {
                drop(raw);
                return Err(cancelled(&mut drawn, title));
            }
            _ => {}
        }
    }
    drop(raw);
    answered(
        &mut drawn,
        title,
        &if yes {
            t!("install.ui.yes")
        } else {
            t!("install.ui.no")
        },
    );
    Ok(yes)
}

/// A line of text, edited by the terminal itself (so pasting and input methods work); an empty
/// answer takes `default`. The end of input cancels.
pub fn input(title: &str, default: Option<&str>) -> Result<String> {
    spaced();
    let hint = default
        .map(|d| paint(Tone::Dim, &t!("install.ui.input_default", default = d)))
        .unwrap_or_default();
    out(&format!(
        "{}  {}{hint}\n{}  {} ",
        paint(Tone::Accent, "◆"),
        paint(Tone::Bold, title),
        bar(),
        paint(Tone::Accent, "›")
    ));
    let mut line = String::new();
    if io::stdin().read_line(&mut line)? == 0 {
        out("\n");
        let mut drawn = 2;
        return Err(cancelled(&mut drawn, title));
    }
    let typed = line.trim();
    let answer = if typed.is_empty() {
        default.unwrap_or("")
    } else {
        typed
    };
    let mut drawn = 2;
    let empty = t!("install.ui.input_empty");
    answered(
        &mut drawn,
        title,
        if answer.is_empty() { &empty } else { answer },
    );
    Ok(answer.to_owned())
}

/// A service summary (`标签：值` lines under a heading, as `output::service` writes it) with the
/// heading and the labels set apart. Unchanged where output is not decorated.
pub fn summary(text: &str) -> String {
    if !decorated() {
        return text.to_owned();
    }
    let separator = t!("install.status.label_separator");
    let labels = text
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(separator.as_str()))
        .map(|(label, _)| width(label))
        .filter(|w| *w <= 16)
        .max()
        .unwrap_or(0);
    let mut styled = String::from("\n");
    for (at, line) in text.lines().enumerate() {
        if at == 0 {
            styled.push_str(&format!(
                "  {}\n",
                paint(Tone::Accent, &paint(Tone::Bold, line))
            ));
        } else if let Some((label, value)) = line.split_once(separator.as_str()) {
            styled.push_str(&format!(
                "  {}  {value}\n",
                paint(Tone::Dim, &pad(label, labels))
            ));
        } else if line.is_empty() {
            styled.push('\n');
        } else {
            styled.push_str(&format!("  {line}\n"));
        }
    }
    styled.push('\n');
    styled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_counts_columns_not_bytes_or_escapes() {
        assert_eq!(width("Hub"), 3);
        assert_eq!(width("仅本机"), 6);
        assert_eq!(width("本机 + LAN"), 10);
        assert_eq!(width("\x1b[1m\x1b[38;2;167;139;250m安装\x1b[0m ok"), 7);
        assert_eq!(width("e\u{301}"), 1, "a combining mark takes no column");
        assert_eq!(pad("中", 5), "中   ");
        assert_eq!(pad("too long", 3), "too long");
    }

    #[test]
    fn clip_keeps_text_within_its_columns() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("0123456789", 10), "0123456789");
        assert_eq!(clip("0123456789ab", 10), "012345678…");
        let cut = clip("监听 0.0.0.0:8440，其他设备和 Node 可以连进来", 20);
        assert!(cut.ends_with('…') && width(&cut) <= 20, "{cut}");
        // a wide character that would straddle the edge is left out whole
        assert_eq!(clip("一二三四五六", 6), "一二…");
    }

    #[test]
    fn wrap_breaks_at_spaces_and_between_chinese_characters() {
        let lines = wrap("the quick brown fox jumps over the lazy dog", 16);
        assert!(lines.iter().all(|l| width(l) <= 16), "{lines:?}");
        assert_eq!(
            lines.join(" "),
            "the quick brown fox jumps over the lazy dog"
        );
        let text = "这个监听地址推不出网页地址：填你在浏览器里打开用的地址。";
        let lines = wrap(text, 20);
        assert!(
            lines.len() > 1 && lines.iter().all(|l| width(l) <= 20),
            "{lines:?}"
        );
        assert_eq!(
            lines.concat(),
            text,
            "nothing is lost where there is no space to break at"
        );
        assert_eq!(wrap("one\ntwo", 40), ["one", "two"]);
        assert_eq!(wrap("", 40), Vec::<String>::new());
        // a width too small to be a terminal is treated as ten columns
        assert!(wrap("0123456789abcdef", 2).iter().all(|l| width(l) <= 10));
    }
}
