//! Port of `src/cli/list_themes.zig`.
//!
//! STATUS: ported
//!
//! Port notes:
//! * The TUI uses the [`tui::vaxis`](super::tui::vaxis) stand-in for
//!   libvaxis (same drawing model, key bindings and layout).
//! * The `zf` fuzzy ranking package is ported in the private [`zf`]
//!   module (`rank`/`rankNeedle` of zf 0.11, the version pinned in
//!   `build.zig.zon`, with its tests).
//! * `updateFiltered` compares the old and new filtered lists directly
//!   instead of comparing their Wyhash digests.

use std::cell::RefCell;
use std::io::{self, BufWriter, IsTerminal, Write};
use std::rc::Rc;
use std::time::Duration;

use super::args::{self, cli_enum, cli_struct, Error, ParseHooks};
use super::ghostty::{global_args, Action, RunResult};
use super::tui::{self, vaxis};
use crate::config::config::TerminalColor;
use crate::config::theme as themepkg;
use crate::config::{self as configpkg, Config};
use crate::global;

// When the number of filtered themes is less than or equal to this threshold,
// the window position will be reset to 0 to show all results from the top.
// This ensures better visibility for small result sets while maintaining
// scroll position for larger lists.
const SMALL_LIST_THRESHOLD: usize = 10;

cli_enum! {
    pub enum ColorScheme {
        All => "all",
        Dark => "dark",
        Light => "light",
    }
}

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// If true, print the full path to the theme.
        pub path: bool = false,

        /// If true, force a plain list of themes.
        pub plain: bool = false,

        /// Specifies the color scheme of the themes to include in the list.
        pub color: ColorScheme = ColorScheme::All,
    }
    with { custom_hooks }
}

impl ParseHooks for Options {
    /// Enables "-h" and "--help" to work.
    fn help(&mut self) -> Option<Result<(), Error>> {
        Action::help_result()
    }
}

#[derive(Debug, Clone)]
struct ThemeListElement {
    location: themepkg::Location,
    path: String,
    theme: String,
    rank: Option<f64>,
}

/// Zig `std.ascii.orderIgnoreCase`.
fn order_ignore_case(lhs: &str, rhs: &str) -> std::cmp::Ordering {
    lhs.bytes()
        .map(|b| b.to_ascii_lowercase())
        .cmp(rhs.bytes().map(|b| b.to_ascii_lowercase()))
}

/// Zig `std.Uri` path percent-encoding (`isPathChar`).
fn percent_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for &b in path.as_bytes() {
        let ok = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'.' | b'_' | b'~' | b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b','
                    | b';' | b'=' | b':' | b'@' | b'/'
            );
        if ok {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

impl ThemeListElement {
    fn less_than(lhs: &Self, rhs: &Self) -> std::cmp::Ordering {
        // TODO: use Unicode-aware comparison
        order_ignore_case(&lhs.theme, &rhs.theme)
    }

    pub fn to_uri(&self) -> String {
        format!("file://{}", percent_encode_path(&self.path))
    }
}

fn location_tag(location: themepkg::Location) -> &'static str {
    match location {
        themepkg::Location::User => "user",
        themepkg::Location::Resources => "resources",
    }
}

/// The help text of this action (Zig `help_strings.Action.@"list-themes"`).
pub const HELP: &str = include_str!("list_themes_help.txt");

/// The `list-themes` command is used to preview or list all the available
/// themes for Ghostty. See [`HELP`].
pub fn run() -> RunResult {
    let mut opts = Options::default();

    {
        let mut iter = args::args_iterator(global_args());
        args::parse(&mut opts, &mut iter)?;
    }

    let stdout_file = io::stdout();
    let tty = stdout_file.is_terminal();
    let mut stdout = BufWriter::with_capacity(4096, stdout_file.lock());
    let mut stderr = io::stderr().lock();

    let resources_dir = global::resources_dir();
    if resources_dir.app().is_none() {
        write!(
            stderr,
            "Could not find the Ghostty resources directory. Please ensure \
             that Ghostty is installed correctly.\n"
        )?;
    }

    let mut count: usize = 0;

    let mut themes: Vec<ThemeListElement> = Vec::new();

    let mut it = themepkg::LocationIterator::default();

    while let Some(loc) = it.next() {
        let dir = match std::fs::read_dir(&loc.dir) {
            Ok(dir) => dir,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                eprintln!("error trying to open {}: {}", loc.dir, err);
                continue;
            }
        };

        for entry in dir {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_file() || kind.is_symlink() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name == ".DS_Store" {
                    continue;
                }
                count += 1;

                let path = std::path::Path::new(&loc.dir).join(&name);
                themes.push(ThemeListElement {
                    path: path.to_string_lossy().into_owned(),
                    location: loc.location,
                    theme: name,
                    rank: None,
                });
            }
        }
    }

    if count == 0 {
        write!(
            stderr,
            "No themes found, check to make sure that the themes were installed correctly."
        )?;
        return Ok(1);
    }

    themes.sort_unstable_by(ThemeListElement::less_than);

    if tui::CAN_PRETTY_PRINT && !opts.plain && tty {
        drop(stdout);
        preview(themes, opts.color)?;
        return Ok(0);
    }

    let mut theme_config = Config::default();
    for theme in &themes {
        theme_config.load_file(&theme.path)?;
        if !should_include_theme(opts.color, &theme_config) {
            continue;
        }
        if opts.path {
            writeln!(stdout, "{} ({}) {}", theme.theme, location_tag(theme.location), theme.path)?;
        } else {
            writeln!(stdout, "{} ({})", theme.theme, location_tag(theme.location))?;
        }
    }

    // Don't forget to flush!
    stdout.flush()?;
    Ok(0)
}

fn resolve_auto_theme_path() -> Result<std::path::PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let main_cfg_path = configpkg::preferred_default_file_path()?;

    let base_dir = std::path::Path::new(&main_cfg_path)
        .parent()
        .ok_or_else(|| Box::new(Error::Other("BadPathName")))?;
    Ok(base_dir.join("auto").join("theme.ghostty"))
}

fn write_auto_theme_file(theme_name: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let auto_path = resolve_auto_theme_path()?;

    if let Some(dir) = auto_path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let mut f = std::fs::File::create(&auto_path)?;
    write!(f, "theme = {}\n", theme_name)?;
    f.flush()?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Help,
    Search,
    Save,
}

struct Preview {
    should_quit: bool,
    tty: vaxis::Tty,
    vx: vaxis::Vaxis,
    mouse: Option<vaxis::Mouse>,
    themes: Vec<ThemeListElement>,
    filtered: Vec<usize>,
    current: usize,
    window: usize,
    hex: bool,
    mode: Mode,
    color_scheme: vaxis::ColorScheme,
    text_input: vaxis::widgets::TextInput,
    theme_filter: ColorScheme,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowStyle {
    Normal,
    Highlighted,
    Selected,
}

fn rgb(r: u8, g: u8, b: u8) -> vaxis::Color {
    vaxis::Color::Rgb([r, g, b])
}

impl Preview {
    fn init(themes: Vec<ThemeListElement>, theme_filter: ColorScheme) -> io::Result<Preview> {
        let mut this = Preview {
            should_quit: false,
            tty: vaxis::Tty::init()?,
            vx: vaxis::Vaxis::init(),
            mouse: None,
            filtered: Vec::with_capacity(themes.len()),
            themes,
            current: 0,
            window: 0,
            hex: false,
            mode: Mode::Normal,
            color_scheme: vaxis::ColorScheme::Light,
            text_input: vaxis::widgets::TextInput::init(),
            theme_filter,
        };
        this.update_filtered();
        Ok(this)
    }

    fn deinit(&mut self) {
        self.vx.deinit(self.tty.writer());
    }

    fn run(&mut self) -> RunResult {
        let mut event_loop = vaxis::Loop::start(&mut self.tty)?;
        let result = self.run_loop(&mut event_loop);
        self.deinit();
        event_loop.stop(&mut self.tty);
        result
    }

    fn run_loop(&mut self, event_loop: &mut vaxis::Loop) -> RunResult {
        self.vx.enter_alt_screen(self.tty.writer())?;
        self.vx.set_title(self.tty.writer(), "👻 Ghostty Theme Preview 👻")?;
        self.vx.query_terminal(&mut self.tty, Duration::from_secs(1))?;
        self.vx.set_mouse_mode(self.tty.writer(), true)?;
        if self.vx.caps.color_scheme_updates {
            self.vx.subscribe_to_color_scheme_updates(self.tty.writer())?;
        }
        let winsize = self.tty.get_winsize()?;
        self.vx.resize(self.tty.writer(), winsize)?;

        while !self.should_quit {
            if let Some(event) = event_loop.poll_event(&mut self.vx)? {
                self.update(event)?;
            }
            while let Some(event) = event_loop.try_event(&mut self.vx)? {
                self.update(event)?;
            }
            self.draw();

            self.vx.render(self.tty.writer())?;
            self.tty.writer().flush()?;
        }
        Ok(0)
    }

    fn update_filtered(&mut self) {
        let relative = self.current.saturating_sub(self.window);
        let selected: Option<String> = if !self.filtered.is_empty() {
            Some(self.themes[self.filtered[self.current]].theme.clone())
        } else {
            None
        };

        let old = std::mem::take(&mut self.filtered);

        let mut theme_config = Config::default();
        if self.text_input.buf.real_length() > 0 {
            let buffer = format!(
                "{}{}",
                self.text_input.buf.first_half(),
                self.text_input.buf.second_half()
            );

            let string = buffer.to_ascii_lowercase();

            let tokens: Vec<&str> = string.split(' ').filter(|t| !t.is_empty()).collect();

            for (i, theme) in self.themes.iter_mut().enumerate() {
                let _ = theme_config.load_file(&theme.path);
                if !should_include_theme(self.theme_filter, &theme_config) {
                    continue;
                }

                theme.rank = zf::rank(
                    &theme.theme,
                    &tokens,
                    zf::RankOptions {
                        // NOTE: Changed from ".to_lower = true" (the option was
                        // renamed). I think this is the correct analog for case
                        // insensitive ranking (which is what I'm guessing
                        // ".to_lower = true" implies, but this comment serves as a
                        // hint if there's a regression.
                        case_sensitive: false,
                        plain: true,
                    },
                );
                if theme.rank.is_some() {
                    self.filtered.push(i);
                }
            }
        } else {
            for (i, theme) in self.themes.iter_mut().enumerate() {
                let _ = theme_config.load_file(&theme.path);
                if should_include_theme(self.theme_filter, &theme_config) {
                    self.filtered.push(i);
                    theme.rank = None;
                }
            }
        }

        if old == self.filtered {
            return;
        }

        if self.filtered.is_empty() {
            self.current = 0;
            self.window = 0;
            return;
        }

        let (current, window) = 'current: {
            let Some(selected) = selected.filter(|s| !s.is_empty()) else {
                break 'current (0, 0);
            };

            for (i, &index) in self.filtered.iter().enumerate() {
                if self.themes[index].theme == selected {
                    // Keep the relative position but ensure all search results are visible
                    let new_window = i.saturating_sub(relative);
                    // If the new window would hide some results at the top, adjust it
                    break 'current (
                        i,
                        if self.filtered.len() <= SMALL_LIST_THRESHOLD {
                            0
                        } else {
                            new_window
                        },
                    );
                }
            }
            (0, 0)
        };
        self.current = current;
        self.window = window;
    }

    fn up(&mut self, count: usize) {
        if self.filtered.is_empty() {
            self.current = 0;
            return;
        }
        self.current = self.current.saturating_sub(count);
    }

    fn down(&mut self, count: usize) {
        if self.filtered.is_empty() {
            self.current = 0;
            return;
        }
        self.current += count;
        if self.current >= self.filtered.len() {
            self.current = self.filtered.len() - 1;
        }
    }

    fn update(&mut self, event: vaxis::Event) -> io::Result<()> {
        use vaxis::{Key, Modifiers};
        let none = Modifiers::NONE;
        let ch = |c: char| c as u32;
        match event {
            vaxis::Event::KeyPress(key) => {
                if key.matches(ch('c'), Modifiers::CTRL) {
                    self.should_quit = true;
                }
                match self.mode {
                    Mode::Normal => {
                        if key.matches_any(&[ch('q'), Key::ESCAPE], none) {
                            self.should_quit = true;
                        }
                        if key.matches_any(&[ch('?'), Key::F1], none) {
                            self.mode = Mode::Help;
                        }
                        if key.matches(ch('h'), Modifiers::CTRL) {
                            self.mode = Mode::Help;
                        }
                        if key.matches(ch('/'), none) {
                            self.mode = Mode::Search;
                        }
                        // The save screen shows the selected theme, so there
                        // must be one to select.
                        if key.matches_any(&[Key::ENTER, Key::KP_ENTER], none) && !self.filtered.is_empty() {
                            self.mode = Mode::Save;
                        }
                        if key.matches_any(&[ch('x'), ch('/')], Modifiers::CTRL) {
                            self.text_input.buf.clear_retaining_capacity();
                            self.update_filtered();
                        }
                        if key.matches_any(&[Key::HOME, Key::KP_HOME, ch('g')], none) {
                            self.current = 0;
                        }
                        if key.matches_any(&[Key::END, Key::KP_END, ch('G')], none) {
                            self.current = self.filtered.len().saturating_sub(1);
                        }
                        if key.matches_any(&[ch('j'), ch('+'), Key::DOWN, Key::KP_DOWN, Key::KP_ADD], none) {
                            self.down(1);
                        }
                        if key.matches_any(&[Key::PAGE_DOWN, Key::KP_PAGE_DOWN], none) {
                            self.down(20);
                        }
                        if key.matches(ch('d'), Modifiers::CTRL) {
                            self.down(20);
                        }
                        if key.matches_any(&[ch('k'), ch('-'), Key::UP, Key::KP_UP, Key::KP_SUBTRACT], none) {
                            self.up(1);
                        }
                        if key.matches_any(&[Key::PAGE_UP, Key::KP_PAGE_UP], none) {
                            self.up(20);
                        }
                        if key.matches(ch('u'), Modifiers::CTRL) {
                            self.up(20);
                        }
                        if key.matches_any(&[ch('h'), ch('x')], none) {
                            self.hex = true;
                        }
                        if key.matches(ch('d'), none) {
                            self.hex = false;
                        }
                        if !self.filtered.is_empty() {
                            let theme = &self.themes[self.filtered[self.current]];
                            if key.matches(ch('c'), none) {
                                self.vx.copy_to_system_clipboard(self.tty.writer(), &theme.theme)?;
                            } else if key.matches(ch('c'), Modifiers::SHIFT) {
                                self.vx.copy_to_system_clipboard(self.tty.writer(), &theme.path)?;
                            }
                        }
                        if key.matches(ch('f'), none) {
                            self.theme_filter = match self.theme_filter {
                                ColorScheme::All => ColorScheme::Dark,
                                ColorScheme::Dark => ColorScheme::Light,
                                ColorScheme::Light => ColorScheme::All,
                            };
                            self.update_filtered();
                        }
                    }
                    Mode::Help => {
                        if key.matches(ch('q'), none) {
                            self.should_quit = true;
                        }
                        if key.matches_any(&[ch('?'), Key::ESCAPE, Key::F1], none) {
                            self.mode = Mode::Normal;
                        }
                        if key.matches(ch('h'), Modifiers::CTRL) {
                            self.mode = Mode::Normal;
                        }
                    }
                    Mode::Search => 'search: {
                        if key.matches_any(&[Key::ESCAPE, Key::ENTER], none) {
                            self.mode = Mode::Normal;
                            break 'search;
                        }
                        if key.matches_any(&[ch('x'), ch('/')], Modifiers::CTRL) {
                            self.text_input.clear_retaining_capacity();
                            self.update_filtered();
                            break 'search;
                        }
                        self.text_input.update(&key);
                        self.update_filtered();
                    }
                    Mode::Save => {
                        if key.matches(ch('q'), none) {
                            self.should_quit = true;
                        }
                        if key.matches_any(&[Key::ESCAPE, Key::ENTER, Key::KP_ENTER], none) {
                            self.mode = Mode::Normal;
                        }
                        if key.matches(ch('w'), none) {
                            self.save_selected_theme();
                        }
                    }
                }
            }
            vaxis::Event::ColorScheme(color_scheme) => self.color_scheme = color_scheme,
            vaxis::Event::Mouse(mouse) => self.mouse = Some(mouse),
            vaxis::Event::Winsize(ws) => self.vx.resize(self.tty.writer(), ws)?,
        }
        Ok(())
    }

    fn ui_fg(&self) -> vaxis::Color {
        match self.color_scheme {
            vaxis::ColorScheme::Light => rgb(0x00, 0x00, 0x00),
            vaxis::ColorScheme::Dark => rgb(0xff, 0xff, 0xff),
        }
    }

    fn ui_bg(&self) -> vaxis::Color {
        match self.color_scheme {
            vaxis::ColorScheme::Light => rgb(0xff, 0xff, 0xff),
            vaxis::ColorScheme::Dark => rgb(0x00, 0x00, 0x00),
        }
    }

    fn ui_standard(&self) -> vaxis::Style {
        vaxis::Style {
            fg: self.ui_fg(),
            bg: self.ui_bg(),
            ..Default::default()
        }
    }

    fn ui_hover_bg(&self) -> vaxis::Color {
        match self.color_scheme {
            vaxis::ColorScheme::Light => rgb(0xbb, 0xbb, 0xbb),
            vaxis::ColorScheme::Dark => rgb(0x22, 0x22, 0x22),
        }
    }

    fn ui_highlighted(&self) -> vaxis::Style {
        vaxis::Style {
            fg: self.ui_fg(),
            bg: self.ui_hover_bg(),
            ..Default::default()
        }
    }

    fn ui_selected_fg(&self) -> vaxis::Color {
        match self.color_scheme {
            vaxis::ColorScheme::Light => rgb(0x00, 0xaa, 0x00),
            vaxis::ColorScheme::Dark => rgb(0x00, 0xaa, 0x00),
        }
    }

    fn ui_selected_bg(&self) -> vaxis::Color {
        match self.color_scheme {
            vaxis::ColorScheme::Light => rgb(0xaa, 0xaa, 0xaa),
            vaxis::ColorScheme::Dark => rgb(0x33, 0x33, 0x33),
        }
    }

    fn ui_selected(&self) -> vaxis::Style {
        vaxis::Style {
            fg: self.ui_selected_fg(),
            bg: self.ui_selected_bg(),
            ..Default::default()
        }
    }

    fn ui_err_fg(&self) -> vaxis::Color {
        rgb(0xff, 0x00, 0x00)
    }

    fn ui_err(&self) -> vaxis::Style {
        vaxis::Style {
            fg: self.ui_err_fg(),
            bg: self.ui_bg(),
            ..Default::default()
        }
    }

    fn draw(&mut self) {
        let screen: Rc<RefCell<vaxis::Screen>> = Rc::clone(&self.vx.screen);
        let win = vaxis::window_for(&screen);
        win.clear();

        let theme_list = win.child(vaxis::ChildOptions {
            x_off: 0,
            y_off: 0,
            width: Some(32),
            height: Some(win.height),
            ..Default::default()
        });

        let mut highlight: Option<usize> = None;

        if let Some(mouse) = self.mouse.take() {
            if self.mode == Mode::Normal {
                if mouse.button == vaxis::MouseButton::WheelUp {
                    self.up(1);
                }
                if mouse.button == vaxis::MouseButton::WheelDown {
                    self.down(1);
                }
                if theme_list.has_mouse(Some(mouse)).is_some() {
                    // NOTE: mouse co-ordinates can be negative (see
                    // https://github.com/rockorager/libvaxis/pull/276).
                    // Working around it in this case, but putting this here in
                    // case there are issues.
                    if mouse.button == vaxis::MouseButton::Left && mouse.type_ == vaxis::MouseType::Release {
                        let selection: usize = {
                            let mut window: i32 = self.window.min(i32::MAX as usize) as i32;
                            window += mouse.row as i32;
                            window.max(0) as usize
                        };
                        if selection < self.filtered.len() {
                            self.current = selection;
                        }
                    }
                    highlight = Some(mouse.row.max(0) as usize);
                }
            }
        }

        if self.filtered.is_empty() {
            self.current = 0;
            self.window = 0;
        } else {
            let start = self.window;
            let end = (self.window + theme_list.height as usize).saturating_sub(1);
            if self.current > end {
                self.window = self.current + 1 - theme_list.height as usize;
            }
            if self.current < start {
                self.window = self.current;
            }
            if self.window >= self.filtered.len() {
                self.window = self.filtered.len() - 1;
            }
        }

        theme_list.fill(vaxis::Cell::styled(self.ui_standard()));

        let at = |row: u16, col: u16| vaxis::PrintOptions {
            row_offset: row,
            col_offset: col,
            ..Default::default()
        };

        for row in 0..theme_list.height {
            let index = self.window + row as usize;
            if index >= self.filtered.len() {
                break;
            }

            let theme = &self.themes[self.filtered[index]];

            let style = if index == self.current {
                RowStyle::Selected
            } else if highlight == Some(row as usize) {
                RowStyle::Highlighted
            } else {
                RowStyle::Normal
            };

            if style == RowStyle::Selected {
                theme_list.print_segment(
                    vaxis::Segment {
                        text: "❯ ",
                        style: self.ui_selected(),
                        ..Default::default()
                    },
                    at(row, 0),
                );
            }
            let uri = theme.to_uri();
            theme_list.print_segment(
                vaxis::Segment {
                    text: &theme.theme,
                    style: match style {
                        RowStyle::Normal => self.ui_standard(),
                        RowStyle::Highlighted => self.ui_highlighted(),
                        RowStyle::Selected => self.ui_selected(),
                    },
                    link: &uri,
                },
                at(row, 2),
            );
            if style == RowStyle::Selected {
                if theme.theme.len() < (theme_list.width as usize).saturating_sub(4) {
                    for i in 2 + theme.theme.len()..(theme_list.width as usize).saturating_sub(2) {
                        theme_list.print_segment(
                            vaxis::Segment {
                                text: " ",
                                style: self.ui_selected(),
                                ..Default::default()
                            },
                            at(row, i as u16),
                        );
                    }
                }
                theme_list.print_segment(
                    vaxis::Segment {
                        text: " ❮",
                        style: self.ui_selected(),
                        ..Default::default()
                    },
                    at(row, theme_list.width.saturating_sub(2)),
                );
            }
        }

        self.draw_preview(win, theme_list.x_off + theme_list.width as i32);

        match self.mode {
            Mode::Normal => {
                win.hide_cursor();
            }
            Mode::Help => {
                win.hide_cursor();
                let width: u16 = 60;
                let height: u16 = 22;
                let child = win.child(vaxis::ChildOptions {
                    x_off: (win.width / 2).saturating_sub(width / 2) as i32,
                    y_off: (win.height / 2).saturating_sub(height / 2) as i32,
                    width: Some(width),
                    height: Some(height),
                    border: vaxis::BorderOptions {
                        where_: vaxis::BorderWhere::All,
                        style: self.ui_standard(),
                    },
                });

                child.fill(vaxis::Cell::styled(self.ui_standard()));

                const KEY_HELP: [(&str, &str); 19] = [
                    ("^C, q, ESC", "Quit."),
                    ("F1, ?, ^H", "Toggle help window."),
                    ("f", "Cycle through theme filters."),
                    ("k, ↑", "Move up 1 theme."),
                    ("ScrollUp", "Move up 1 theme."),
                    ("PgUp, ^U", "Move up 20 themes."),
                    ("j, ↓", "Move down 1 theme."),
                    ("ScrollDown", "Move down 1 theme."),
                    ("PgDown, ^D", "Move down 20 themes."),
                    ("h, x", "Show palette numbers in hexadecimal."),
                    ("d", "Show palette numbers in decimal."),
                    ("c", "Copy theme name to the clipboard."),
                    ("C", "Copy theme path to the clipboard."),
                    ("Home, g", "Go to the start of the list."),
                    ("End, G", "Go to the end of the list."),
                    ("/", "Start search."),
                    ("^X, ^/", "Clear search."),
                    ("⏎", "Save theme or close search window."),
                    ("w", "Write theme to auto config file."),
                ];

                for (i, (keys, help)) in KEY_HELP.iter().enumerate() {
                    let i = i as u16;
                    let seg = |text| vaxis::Segment {
                        text,
                        style: self.ui_standard(),
                        ..Default::default()
                    };
                    child.print_segment(seg(keys), at(i + 1, 2));
                    child.print_segment(seg("—"), at(i + 1, 15));
                    child.print_segment(seg(help), at(i + 1, 17));
                }
            }
            Mode::Search => {
                let child = win.child(vaxis::ChildOptions {
                    x_off: 20,
                    y_off: win.height as i32 - 5,
                    width: Some(win.width.saturating_sub(40)),
                    height: Some(3),
                    border: vaxis::BorderOptions {
                        where_: vaxis::BorderWhere::All,
                        style: self.ui_standard(),
                    },
                });
                child.fill(vaxis::Cell::styled(self.ui_standard()));
                let style = self.ui_standard();
                self.text_input.draw_with_style(child, style);
            }
            Mode::Save => {
                let theme = &self.themes[self.filtered[self.current]];

                let width: u16 = 92;
                let height: u16 = 17;
                let child = win.child(vaxis::ChildOptions {
                    x_off: (win.width / 2).saturating_sub(width / 2) as i32,
                    y_off: (win.height / 2).saturating_sub(height / 2) as i32,
                    width: Some(width),
                    height: Some(height),
                    border: vaxis::BorderOptions {
                        where_: vaxis::BorderWhere::All,
                        style: self.ui_standard(),
                    },
                });

                child.fill(vaxis::Cell::styled(self.ui_standard()));

                let theme_line = format!("theme = {}", theme.theme);
                let save_instructions: [&str; 14] = [
                    "To apply this theme, add the following line to your Ghostty configuration:",
                    "",
                    &theme_line,
                    "",
                    "Save the configuration file and then reload it to apply the new theme.",
                    "",
                    "Or press 'w' to write an auto theme file to your system's preferred default config path.",
                    "Then add the following line to your Ghostty configuration and reload:",
                    "",
                    "config-file = ?auto/theme.ghostty",
                    "",
                    "For more details on configuration and themes, visit the Ghostty documentation:",
                    "",
                    "https://ghostty.org/docs/config/reference",
                ];

                for (i, instruction) in save_instructions.iter().enumerate() {
                    child.print_segment(
                        vaxis::Segment {
                            text: instruction,
                            style: self.ui_standard(),
                            ..Default::default()
                        },
                        at(i as u16 + 1, 2),
                    );
                }
            }
        }
    }

    fn draw_preview(&self, win: vaxis::Window<'_>, x_off_unconverted: i32) {
        let x_off: u16 = x_off_unconverted as u16;
        let width: u16 = win.width.saturating_sub(x_off);

        let at = |row: u16, col: u16| vaxis::PrintOptions {
            row_offset: row,
            col_offset: col,
            ..Default::default()
        };

        if self.filtered.is_empty() {
            let child = win.child(vaxis::ChildOptions {
                x_off: 0,
                y_off: 0,
                width: Some(win.width),
                height: Some(win.height),
                ..Default::default()
            });
            child.fill(vaxis::Cell::styled(self.ui_standard()));

            child.print_segment(
                vaxis::Segment {
                    text: "No theme found!",
                    style: self.ui_standard(),
                    ..Default::default()
                },
                at((win.height / 2).saturating_sub(1), (win.width / 2).saturating_sub(7)),
            );
            return;
        }

        let theme = &self.themes[self.filtered[self.current]];
        let uri = theme.to_uri();

        let mut config = Config::default();

        if let Err(err) = config.load_file(&theme.path) {
            let theme_path_len: u16 = theme.path.len() as u16;

            let child = win.child(vaxis::ChildOptions {
                x_off: x_off as i32,
                y_off: 0,
                width: Some(width),
                height: Some(win.height),
                ..Default::default()
            });
            child.fill(vaxis::Cell::styled(self.ui_standard()));
            let middle = child.height / 2;
            {
                let text = format!("Unable to open {} from:", theme.theme);
                let text_len: u16 = text.len() as u16;
                child.print_segment(
                    vaxis::Segment {
                        text: &text,
                        style: self.ui_err(),
                        ..Default::default()
                    },
                    at(middle.saturating_sub(1), (child.width / 2).saturating_sub(text_len / 2)),
                );
            }
            child.print_segment(
                vaxis::Segment {
                    text: &theme.path,
                    style: self.ui_err(),
                    link: &uri,
                },
                at(middle, (child.width / 2).saturating_sub(theme_path_len / 2)),
            );
            {
                let text = format!("{}", err);
                let text_len: u16 = text.len() as u16;
                child.print_segment(
                    vaxis::Segment {
                        text: &text,
                        style: self.ui_err(),
                        ..Default::default()
                    },
                    at(middle + 1, (child.width / 2).saturating_sub(text_len / 2)),
                );
            }
            return;
        }

        let mut next_start: u16 = 0;

        let fg = rgb(config.foreground.r, config.foreground.g, config.foreground.b);
        let bg = rgb(config.background.r, config.background.g, config.background.b);
        let terminal_color = |c: Option<TerminalColor>, default: vaxis::Color| match c {
            Some(TerminalColor::Color(c)) => rgb(c.r, c.g, c.b),
            _ => default,
        };
        let cursor_fg = terminal_color(config.cursor_text, bg);
        let cursor_bg = terminal_color(config.cursor_color, fg);
        let selection_fg = terminal_color(config.selection_foreground, bg);
        let selection_bg = terminal_color(config.selection_background, fg);
        let style = |fg: vaxis::Color| vaxis::Style {
            fg,
            bg,
            ..Default::default()
        };
        let cursor = vaxis::Style {
            fg: cursor_fg,
            bg: cursor_bg,
            ..Default::default()
        };
        let standard_selection = vaxis::Style {
            fg: selection_fg,
            bg: selection_bg,
            ..Default::default()
        };
        let standard = style(fg);
        let standard_bold = vaxis::Style { bold: true, ..standard };
        let standard_italic = vaxis::Style { italic: true, ..standard };
        let standard_bold_italic = vaxis::Style {
            bold: true,
            italic: true,
            ..standard
        };
        let ul = |ul_style| vaxis::Style { ul_style, ..standard };
        let standard_underline = ul(vaxis::UnderlineStyle::Single);
        let standard_double_underline = ul(vaxis::UnderlineStyle::Double);
        let standard_dashed_underline = ul(vaxis::UnderlineStyle::Dashed);
        let standard_curly_underline = ul(vaxis::UnderlineStyle::Curly);
        let standard_dotted_underline = ul(vaxis::UnderlineStyle::Dotted);

        {
            let theme_len: u16 = theme.theme.len() as u16;
            let theme_path_len: u16 = theme.path.len() as u16;
            let child = win.child(vaxis::ChildOptions {
                x_off: x_off as i32,
                y_off: next_start as i32,
                width: Some(width),
                height: Some(4),
                ..Default::default()
            });
            child.fill(vaxis::Cell::styled(standard));
            child.print_segment(
                vaxis::Segment {
                    text: &theme.theme,
                    style: standard_bold_italic,
                    link: &uri,
                },
                at(1, (child.width / 2).saturating_sub(theme_len / 2)),
            );
            child.print_segment(
                vaxis::Segment {
                    text: &theme.path,
                    style: standard,
                    link: &uri,
                },
                vaxis::PrintOptions {
                    row_offset: 2,
                    col_offset: (child.width / 2).saturating_sub(theme_path_len / 2),
                    wrap: vaxis::Wrap::None,
                },
            );
            next_start += child.height;
        }

        if !config._diagnostics.items().is_empty() {
            let diagnostic_items_len: u16 = config._diagnostics.items().len() as u16;
            let child = win.child(vaxis::ChildOptions {
                x_off: x_off as i32,
                y_off: next_start as i32,
                width: Some(width),
                height: Some(2 + diagnostic_items_len),
                ..Default::default()
            });
            {
                let text = "Problems were encountered trying to load the theme:";
                let text_len: u16 = text.len() as u16;
                child.print_segment(
                    vaxis::Segment {
                        text,
                        style: self.ui_err(),
                        ..Default::default()
                    },
                    at(0, (child.width / 2).saturating_sub(text_len / 2)),
                );
            }

            for (i, diag) in config._diagnostics.items().iter().enumerate() {
                let text = diag.to_string();
                child.print_segment(
                    vaxis::Segment {
                        text: &text,
                        style: self.ui_err(),
                        ..Default::default()
                    },
                    at(2 + i as u16, 2),
                );
            }
            next_start += child.height;
        }
        {
            let child = win.child(vaxis::ChildOptions {
                x_off: x_off as i32,
                y_off: next_start as i32,
                width: Some(width),
                height: Some(6),
                ..Default::default()
            });

            child.fill(vaxis::Cell::styled(standard));

            for i in 0..16u16 {
                let r = i / 8;
                let c = i % 8;
                let text = if self.hex {
                    format!(" {:02x}", i)
                } else {
                    format!("{:3}", i)
                };
                child.print_segment(
                    vaxis::Segment {
                        text: &text,
                        style: standard,
                        ..Default::default()
                    },
                    at(3 * r, c * 8),
                );
                for dr in 0..2 {
                    child.print_segment(
                        vaxis::Segment {
                            text: "████",
                            style: style(color(&config, i as usize)),
                            ..Default::default()
                        },
                        at(3 * r + dr, 4 + c * 8),
                    );
                }
            }
            next_start += child.height;
        }
        {
            let child = win.child(vaxis::ChildOptions {
                x_off: x_off as i32,
                y_off: next_start as i32,
                width: Some(width),
                height: Some(28),
                ..Default::default()
            });
            let bold = standard_bold;
            let color1 = style(color(&config, 1));
            let color2 = style(color(&config, 2));
            let color3 = style(color(&config, 3));
            let color4 = style(color(&config, 4));
            let color5 = style(color(&config, 5));
            let color6 = style(color(&config, 6));
            let color6ul = vaxis::Style {
                ul_style: vaxis::UnderlineStyle::Single,
                ..color6
            };
            let color10 = style(color(&config, 10));
            let color12 = style(color(&config, 12));
            let color238 = style(color(&config, 238));
            child.fill(vaxis::Cell::styled(standard));

            let line = |row: u16, parts: &[(&str, vaxis::Style)]| {
                let segments: Vec<vaxis::Segment<'_>> = parts
                    .iter()
                    .map(|&(text, style)| vaxis::Segment {
                        text,
                        style,
                        ..Default::default()
                    })
                    .collect();
                child.print(&segments, at(row, 2));
            };
            let rule = |row: u16, start: &str| {
                line(row, &[(start, color238)]);
                if child.width > 10 {
                    for col in 10..child.width {
                        child.print(
                            &[vaxis::Segment {
                                text: "─",
                                style: color238,
                                ..Default::default()
                            }],
                            at(row, col),
                        );
                    }
                }
            };

            line(
                0,
                &[
                    ("→", color2),
                    (" ", standard),
                    ("bat", color4),
                    (" ", standard),
                    ("ziggzagg.zig", color6ul),
                ],
            );
            rule(1, "───────┬");
            line(2, &[("       │ ", color238), ("File: ", standard), ("ziggzagg.zig", bold)]);
            rule(3, "───────┼");
            line(
                4,
                &[
                    ("   1   │ ", color238),
                    ("const", color5),
                    (" std ", standard),
                    ("= @import", color5),
                    ("(", standard),
                    ("\"std\"", color10),
                    (");", standard),
                ],
            );
            line(5, &[("   2   │", color238)]);
            line(
                6,
                &[
                    ("   3   │ ", color238),
                    ("pub ", color5),
                    ("fn ", color12),
                    ("main", color2),
                    ("(init: ", standard),
                    ("std.process.Init", color12),
                    (") ", standard),
                    ("!", color5),
                    ("void", color12),
                    (" {", standard),
                ],
            );
            line(
                7,
                &[
                    ("   4   │     ", color238),
                    ("var ", color5),
                    ("buf:", standard),
                    (" [1024]u8", color12),
                    (" =", color5),
                    (" undefined;", standard),
                ],
            );
            line(
                8,
                &[
                    ("   5   │     ", color238),
                    ("var ", color5),
                    ("stdout ", standard),
                    ("=", color5),
                    (" std.Io.File.stdout().writer(init.io, &buf);", standard),
                ],
            );
            line(
                9,
                &[
                    ("   6   │     ", color238),
                    ("const ", color5),
                    ("w ", standard),
                    ("=", color5),
                    (" &stdout.interface;", standard),
                ],
            );
            line(10, &[("   7   │", color238)]);
            line(
                11,
                &[
                    ("   8   │     ", color238),
                    ("var ", color5),
                    ("i:", standard),
                    (" usize", color12),
                    (" =", color5),
                    (" 1", color4),
                    (";", standard),
                ],
            );
            line(
                12,
                &[
                    ("   9   │     ", color238),
                    ("while ", color5),
                    ("(i ", standard),
                    ("<= ", color5),
                    ("16", color4),
                    (") : (i ", standard),
                    ("+= ", color5),
                    ("1", color4),
                    (") {", standard),
                ],
            );
            line(
                13,
                &[
                    ("  10   │         ", color238),
                    ("if ", color5),
                    ("(i ", standard),
                    ("% ", color5),
                    ("15 ", color4),
                    ("== ", color5),
                    ("0", color4),
                    (") {", standard),
                ],
            );
            line(
                14,
                &[
                    ("  11   │             ", color238),
                    ("try ", color5),
                    ("w.writeAll(", standard),
                    ("\"ZiggZagg", color10),
                    ("\\n", color12),
                    ("\"", color10),
                    (");", standard),
                ],
            );
            line(
                15,
                &[
                    ("  12   │         ", color238),
                    ("} ", standard),
                    ("else if ", color5),
                    ("(i ", standard),
                    ("% ", color5),
                    ("3 ", color4),
                    ("== ", color5),
                    ("0", color4),
                    (") {", standard),
                ],
            );
            line(
                16,
                &[
                    ("  13   │             ", color238),
                    ("try ", color5),
                    ("w.writeAll(", standard),
                    ("\"Zigg", color10),
                    ("\\n", color12),
                    ("\"", color10),
                    (");", standard),
                ],
            );
            line(
                17,
                &[
                    ("  14   │         ", color238),
                    ("} ", standard),
                    ("else if ", color5),
                    ("(i ", standard),
                    ("% ", color5),
                    ("5 ", color4),
                    ("== ", color5),
                    ("0", color4),
                    (") {", standard),
                ],
            );
            line(
                18,
                &[
                    ("  15   │             ", color238),
                    ("try ", color5),
                    ("w.writeAll(", standard),
                    ("\"Zagg", color10),
                    ("\\n", color12),
                    ("\"", color10),
                    (");", standard),
                ],
            );
            line(
                19,
                &[
                    ("  16   │         ", color238),
                    ("} ", standard),
                    ("else ", color5),
                    ("{", standard),
                ],
            );
            line(
                20,
                &[
                    ("  17   │             ", color238),
                    ("try ", color5),
                    ("w.print(\"{d}\\n\", .{i})", standard_selection),
                    (";", cursor),
                ],
            );
            line(21, &[("  18   │         ", color238), ("}", standard)]);
            line(22, &[("  19   │     ", color238), ("}", standard)]);
            line(
                23,
                &[
                    ("  20   │     ", color238),
                    ("try ", color5),
                    ("w.flush();", standard),
                ],
            );
            line(24, &[("  21   │ ", color238), ("}", standard)]);
            rule(25, "───────┴");
            line(
                26,
                &[
                    ("ghostty ", color6),
                    ("on ", standard),
                    (" main ", color4),
                    ("[+] ", color1),
                    ("via ", standard),
                    (" v0.16.0 ", color3),
                    ("via ", standard),
                    ("  impure (ghostty-env)", color4),
                ],
            );
            line(
                27,
                &[
                    ("✦ ", color4),
                    ("at ", standard),
                    ("10:36:15 ", color3),
                    ("→", color2),
                ],
            );
            next_start += child.height;
        }
        if next_start < win.height {
            let child = win.child(vaxis::ChildOptions {
                x_off: x_off as i32,
                y_off: next_start as i32,
                width: Some(width),
                height: Some(win.height - next_start),
                ..Default::default()
            });
            child.fill(vaxis::Cell::styled(standard));
            let mut it = LOREM_IPSUM.split([' ', '\n']);
            let mut row: u16 = 1;
            let mut col: u16 = 2;
            while row < child.height {
                let word = match it.next() {
                    Some(word) => word,
                    None => {
                        it = LOREM_IPSUM.split([' ', '\n']);
                        it.next().unwrap()
                    }
                };
                let word_len: u16 = word.len() as u16;
                if col as usize + word.len() > child.width as usize {
                    row += 1;
                    col = 2;
                }
                let style = match word {
                    "ipsum" => style(color(&config, 2)),
                    "consectetur" => standard_bold,
                    "reprehenderit" => standard_italic,
                    "Praesent" => standard_bold_italic,
                    "auctor" => standard_underline,
                    "dui" => standard_double_underline,
                    "erat" => standard_dashed_underline,
                    "enim" => standard_dotted_underline,
                    "odio" => standard_curly_underline,
                    _ => standard,
                };
                child.print_segment(
                    vaxis::Segment {
                        text: word,
                        style,
                        ..Default::default()
                    },
                    at(row, col),
                );
                col += word_len + 1;
            }
        }
    }

    fn save_selected_theme(&self) {
        if self.filtered.is_empty() {
            return;
        }

        let idx = self.filtered[self.current];
        let theme = &self.themes[idx];

        let _ = write_auto_theme_file(&theme.theme);
    }
}

fn color(config: &Config, palette: usize) -> vaxis::Color {
    let c = config.palette.value[palette];
    rgb(c.r, c.g, c.b)
}

const LOREM_IPSUM: &str = include_str!("lorem_ipsum.txt");

fn preview(themes: Vec<ThemeListElement>, theme_filter: ColorScheme) -> RunResult {
    let mut app = Preview::init(themes, theme_filter)?;
    app.run()
}

fn should_include_theme(theme_filter: ColorScheme, theme_config: &Config) -> bool {
    let rf = theme_config.background.r as f32 / 255.0;
    let gf = theme_config.background.g as f32 / 255.0;
    let bf = theme_config.background.b as f32 / 255.0;
    let luminance = 0.2126 * rf + 0.7152 * gf + 0.0722 * bf;
    let is_dark = luminance < 0.5;
    theme_filter == ColorScheme::All
        || (theme_filter == ColorScheme::Dark && is_dark)
        || (theme_filter == ColorScheme::Light && !is_dark)
}

/// Port of the ranking part of the `zf` fuzzy finding package
/// (natecraddock/zf 0.11: `zf.zig` `rank`/`rankNeedle` and `filter.zig`).
/// Inspired by <https://github.com/garybernhardt/selecta>.
mod zf {
    const SEP: u8 = b'/';

    #[derive(Debug, Clone, Copy)]
    pub struct RankOptions {
        /// Set to false for case insensitive ranking.
        pub case_sensitive: bool,
        /// If true, the zf filepath algorithms are disabled (useful for
        /// matching arbitrary strings)
        pub plain: bool,
    }

    impl Default for RankOptions {
        fn default() -> Self {
            RankOptions {
                case_sensitive: true,
                plain: false,
            }
        }
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct RankNeedleOptions<'a> {
        /// Set to false for case insensitive ranking.
        pub case_sensitive: bool,
        /// Set to true when the needle has path separators in it
        pub strict_path: bool,
        /// Set to the filename (basename) of the haystack for filepath
        /// matching
        pub filename: Option<&'a str>,
    }

    /// Zig `std.fs.path.basename`.
    fn basename(path: &str) -> &str {
        let trimmed = path.trim_end_matches('/');
        match trimmed.rfind('/') {
            Some(i) => &trimmed[i + 1..],
            None => trimmed,
        }
    }

    /// rank a given haystack against a slice of needles
    pub fn rank(haystack: &str, needles: &[&str], opts: RankOptions) -> Option<f64> {
        let filename = if opts.plain { None } else { Some(basename(haystack)) };

        // the haystack must contain all of the characters (in order) in each needle.
        // each needle's rank is summed. if any needle does not match the haystack is ignored.
        let mut sum: f64 = 0.0;
        for needle in needles {
            let strict_path = !opts.plain && has_separator(needle.as_bytes());
            sum += rank_needle_impl(
                haystack.as_bytes(),
                filename.map(str::as_bytes),
                needle.as_bytes(),
                opts.case_sensitive,
                strict_path,
            )?;
        }

        // all needles matched and the best ranks for each needle are summed
        Some(sum)
    }

    /// rank a given haystack against a single needle
    pub fn rank_needle(haystack: &str, needle: &str, opts: RankNeedleOptions<'_>) -> Option<f64> {
        rank_needle_impl(
            haystack.as_bytes(),
            opts.filename.map(str::as_bytes),
            needle.as_bytes(),
            opts.case_sensitive,
            opts.strict_path,
        )
    }

    fn index_of(haystack: &[u8], start_index: usize, needle: u8) -> Option<usize> {
        haystack.get(start_index..)?.iter().position(|&h| h == needle).map(|i| i + start_index)
    }

    fn index_of_insensitive(haystack: &[u8], start_index: usize, needle: u8) -> Option<usize> {
        let n = needle.to_ascii_lowercase();
        haystack
            .get(start_index..)?
            .iter()
            .position(|&h| h.to_ascii_lowercase() == n)
            .map(|i| i + start_index)
    }

    struct IndexIterator<'a> {
        str: &'a [u8],
        char: u8,
        index: usize,
        case_sensitive: bool,
    }

    impl<'a> IndexIterator<'a> {
        fn init(str: &'a [u8], char: u8, case_sensitive: bool) -> Self {
            IndexIterator {
                str,
                char,
                index: 0,
                case_sensitive,
            }
        }

        fn next(&mut self) -> Option<usize> {
            let index = if self.case_sensitive {
                index_of(self.str, self.index, self.char)
            } else {
                index_of_insensitive(self.str, self.index, self.char)
            };
            if let Some(i) = index {
                self.index = i + 1;
            }
            index
        }
    }

    pub fn has_separator(str: &[u8]) -> bool {
        str.contains(&SEP)
    }

    struct PathIterator<'a> {
        str: &'a [u8],
        index: usize,
    }

    impl<'a> PathIterator<'a> {
        fn init(str: &'a [u8]) -> Self {
            PathIterator { str, index: 0 }
        }

        fn next(&mut self) -> Option<&'a [u8]> {
            if self.index >= self.str.len() {
                return None;
            }

            let start = self.index;
            if self.str[self.index] == SEP {
                self.index += 1;
                return Some(&self.str[start..self.index]);
            }

            while self.index < self.str.len() {
                if self.str[self.index] == SEP {
                    return Some(&self.str[start..self.index]);
                }
                self.index += 1;
            }

            Some(&self.str[start..])
        }
    }

    /// Scan left and right for the length of the current path segment
    pub fn segment_len(str: &[u8], index: usize) -> usize {
        if str[index] == SEP {
            return 1;
        }

        let mut start = index;
        let mut end = index;
        while start > 0 {
            if str[start - 1] == SEP {
                break;
            }
            start -= 1;
        }
        while end < str.len() && str[end] != SEP {
            end += 1;
        }
        end - start
    }

    fn rank_needle_impl(
        haystack: &[u8],
        filename: Option<&[u8]>,
        needle: &[u8],
        case_sensitive: bool,
        strict_path: bool,
    ) -> Option<f64> {
        if haystack.is_empty() || needle.is_empty() {
            return None;
        }

        // iterates over the string performing a match starting at each possible index
        // the best (minimum) overall ranking is kept and returned
        let mut best_rank: Option<f64> = None;

        if strict_path {
            let mut iter = PathIterator::init(needle);
            let mut start: usize = 0;
            while let Some(segment) = iter.next() {
                let mut it = IndexIterator::init(haystack, segment[0], case_sensitive);
                it.index = start;
                let mut matched = false;
                while let Some(start_index) = it.next() {
                    if let Some(scan) =
                        scan_to_end(haystack, &segment[1..], start_index, case_sensitive, strict_path)
                    {
                        // how much of the needle segment matched the path segment?
                        // "mod" would match "module" better than "modules" for example
                        let path_segment_len = segment_len(haystack, start_index);
                        let coverage = 1.0 - (segment.len() as f64 / path_segment_len as f64);
                        let rank = coverage * scan.rank;

                        best_rank = Some(match best_rank {
                            None => rank,
                            Some(b) => b + rank,
                        });

                        start = scan.index;
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    return None;
                }
            }
            return best_rank;
        }

        // perform search on the filename only if requested
        if let Some(filename) = filename {
            let mut it = IndexIterator::init(filename, needle[0], case_sensitive);
            while let Some(start_index) = it.next() {
                if let Some(scan) = scan_to_end(filename, &needle[1..], start_index, case_sensitive, false) {
                    if best_rank.is_none_or(|b| scan.rank < b) {
                        best_rank = Some(scan.rank);
                    }
                } else {
                    break;
                }
            }

            if let Some(mut b) = best_rank {
                // was a filename match, give priority
                b /= 2.0;

                // how much of the needle matched the filename?
                if needle.len() == filename.len() {
                    b /= 2.0;
                } else {
                    let coverage = 1.0 - (needle.len() as f64 / filename.len() as f64);
                    b *= coverage;
                }

                return Some(b);
            }
        }

        // perform search on the full string if requested or if no match was found on the filename
        let mut it = IndexIterator::init(haystack, needle[0], case_sensitive);
        while let Some(start_index) = it.next() {
            if let Some(scan) = scan_to_end(haystack, &needle[1..], start_index, case_sensitive, false) {
                if best_rank.is_none_or(|b| scan.rank < b) {
                    best_rank = Some(scan.rank);
                }
            } else {
                break;
            }
        }

        best_rank
    }

    fn is_start_of_word(byte: u8) -> bool {
        matches!(byte, SEP | b'_' | b'-' | b'.' | b' ')
    }

    struct ScanResult {
        rank: f64,
        index: usize,
    }

    /// this is the core of the ranking algorithm. special precedence is given to
    /// filenames. if a match is found on a filename the rank is better.
    fn scan_to_end(
        haystack: &[u8],
        needle: &[u8],
        start_index: usize,
        case_sensitive: bool,
        strict_path: bool,
    ) -> Option<ScanResult> {
        let mut rank: f64 = 1.0;
        let mut last_index = start_index;
        let mut last_sequential = false;

        // penalty for not starting on a word boundary
        if start_index > 0 && !is_start_of_word(haystack[start_index - 1]) {
            rank += 2.0;
        }

        for &c in needle {
            let idx = if case_sensitive {
                index_of(haystack, last_index + 1, c)
            } else {
                index_of_insensitive(haystack, last_index + 1, c)
            }?;

            // did the match span a slash in strict path mode?
            if strict_path && has_separator(&haystack[last_index..idx + 1]) {
                return None;
            }

            if idx == last_index + 1 {
                // sequential matches only count the first character
                if !last_sequential {
                    last_sequential = true;
                    rank += 1.0;
                }
            } else {
                // penalty for not starting on a word boundary
                if !is_start_of_word(haystack[idx - 1]) {
                    rank += 2.0;
                }

                // normal match
                last_sequential = false;
                rank += (idx - last_index) as f64;
            }

            last_index = idx;
        }

        Some(ScanResult {
            rank,
            index: last_index + 1,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn rn(h: &str, f: Option<&str>, n: &str, strict: bool) -> Option<f64> {
            rank_needle_impl(h.as_bytes(), f.map(str::as_bytes), n.as_bytes(), false, strict)
        }

        #[test]
        fn rank_library_interface() {
            let d = RankOptions::default();
            assert!(rank("abcdefg", &["a", "z"], d).is_none());
            assert!(rank("abcdefg", &["a", "b"], d).is_some());
            assert!(rank("abcdefg", &["a", "B"], d).is_none());
            assert!(rank("aBcdefg", &["a", "B"], d).is_some());
            assert!(rank("a/path/to/file", &["zig"], d).is_none());
            assert!(rank("a/path/to/file", &["path", "file"], d).is_some());

            let nd = RankNeedleOptions {
                case_sensitive: true,
                ..Default::default()
            };
            assert!(rank_needle("abcdefg", "a", nd).is_some());
            assert!(rank_needle("abcdefg", "z", nd).is_none());
            assert!(rank_needle("abcdefG", "G", nd).is_some());
            assert!(rank_needle("abcdefg", "A", nd).is_none());
            let fnd = RankNeedleOptions {
                filename: Some("file"),
                ..nd
            };
            assert!(rank_needle("a/path/to/file", "file", fnd).is_some());
            assert!(rank_needle("a/path/to/file", "zig", fnd).is_none());

            // zero length haystacks and needles
            assert!(rank("", &["a"], d).is_none());
            assert!(rank_needle("", "a", nd).is_none());
            assert!(rank("a", &[""], d).is_none());
            assert!(rank_needle("a", "", nd).is_none());
        }

        #[test]
        fn path_iterator() {
            let collect = |s: &str| -> Vec<String> {
                let mut iter = PathIterator::init(s.as_bytes());
                let mut out = Vec::new();
                while let Some(seg) = iter.next() {
                    out.push(String::from_utf8(seg.to_vec()).unwrap());
                }
                out
            };
            assert!(collect("").is_empty());
            assert_eq!(vec!["/"], collect("/"));
            assert_eq!(vec!["a"], collect("a"));
            assert_eq!(vec!["/", "a"], collect("/a"));
            assert_eq!(vec!["a", "/"], collect("a/"));
            assert_eq!(vec!["a", "/", "b", "/"], collect("a/b/"));
            assert_eq!(vec!["src", "/", "data", "/", "b.zig"], collect("src/data/b.zig"));
        }

        #[test]
        fn segment_len_test() {
            assert_eq!(1, segment_len(b"a", 0));
            assert_eq!(1, segment_len(b"/a", 1));
            assert_eq!(1, segment_len(b"/a/", 1));
            assert_eq!(3, segment_len(b"src/main.zig", 0));
            assert_eq!(3, segment_len(b"src/main.zig", 2));
            assert_eq!(8, segment_len(b"src/main.zig", 5));
            assert_eq!(1, segment_len(b"a/b", 1));
        }

        #[test]
        fn rank_needle_test() {
            // plain string matching
            assert_eq!(None, rn("", None, "", false));
            assert_eq!(None, rn("", None, "b", false));
            assert_eq!(None, rn("a", None, "", false));
            assert_eq!(None, rn("a", None, "b", false));
            assert_eq!(None, rn("aaa", None, "aab", false));
            assert_eq!(None, rn("abbba", None, "abab", false));

            assert!(rn("a", None, "a", false).is_some());
            assert!(rn("abc", None, "abc", false).is_some());
            assert!(rn("aaabbbccc", None, "abc", false).is_some());
            assert!(rn("azbycx", None, "x", false).is_some());
            assert!(rn("azbycx", None, "ax", false).is_some());
            assert!(rn("a", None, "a", false).is_some());

            // file name matching
            assert_eq!(None, rn("", Some(""), "", false));
            assert_eq!(None, rn("/a", Some("a"), "b", false));
            assert_eq!(None, rn("c/a", Some("a"), "b", false));
            assert_eq!(None, rn("/file.ext", Some("file.ext"), "z", false));
            assert_eq!(None, rn("/file.ext", Some("file.ext"), "fext.", false));
            assert_eq!(None, rn("/a/b/c", Some("c"), "d", false));

            assert!(rn("/b", Some("b"), "b", false).is_some());
            assert!(rn("/a/b/c", Some("c"), "c", false).is_some());
            assert!(rn("/file.ext", Some("file.ext"), "ext", false).is_some());
            assert!(rn("path/to/file.ext", Some("file.ext"), "file", false).is_some());
            assert!(rn("path/to/file.ext", Some("file.ext"), "to", false).is_some());
            assert!(rn("path/to/file.ext", Some("file.ext"), "path", false).is_some());
            assert!(rn("path/to/file.ext", Some("file.ext"), "pfile", false).is_some());
            assert!(rn("path/to/file.ext", Some("file.ext"), "ptf", false).is_some());
            assert!(rn("path/to/file.ext", Some("file.ext"), "p/t/f", false).is_some());

            // strict path matching
            assert_eq!(None, rn("a/b", Some("b"), "ab", true));
            assert_eq!(None, rn("a/b/c", Some("c"), "abc", true));
            assert_eq!(None, rn("app/monsters/dungeon/foo/bar/baz.rb", Some("baz.rb"), "mod/", true));
            assert_eq!(None, rn("app/models/foo/bar/baz.rb", Some("baz.rb"), "mod/barbaz", true));
            assert_eq!(None, rn("/some/path/here", Some("here"), "/somepath", true));

            assert!(rn("a/b/c", Some("c"), "a/c", true).is_some());
            assert!(rn("a/b/c", Some("c"), "//", true).is_some());
            assert!(rn("src/config/__init__.py", Some("__init__.py"), "con/i", true).is_some());
            assert!(rn("a/b/c/d", Some("d"), "a/b/c", true).is_some());
            assert!(rn("./app/models/foo/bar/baz.rb", Some("baz.rb"), "a/m/f/b/baz", true).is_some());
            assert!(rn("/app/monsters/dungeon/foo/bar/baz.rb", Some("baz.rb"), "a/m/f/b/baz", true).is_some());
            assert!(rn("app/models/foo/bar/baz.rb", Some("baz.rb"), "mod/baz.rb", true).is_some());
        }

        #[test]
        fn plain_case_insensitive_theme_ranking() {
            let opts = RankOptions {
                case_sensitive: false,
                plain: true,
            };
            assert!(rank("Catppuccin Mocha", &["cat", "mocha"], opts).is_some());
            assert!(rank("Catppuccin Mocha", &["dracula"], opts).is_none());
            // Better (contiguous) matches rank lower.
            let a = rank("Dracula", &["drac"], opts).unwrap();
            let b = rank("Dark Ocean Raccoon", &["drac"], opts).unwrap();
            assert!(a < b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_uri() {
        let theme = ThemeListElement {
            location: themepkg::Location::User,
            path: "/themes/Builtin Dark".to_owned(),
            theme: "Builtin Dark".to_owned(),
            rank: None,
        };
        assert_eq!("file:///themes/Builtin%20Dark", theme.to_uri());
    }

    #[test]
    fn include_theme_by_luminance() {
        let mut config = Config::default();
        config.background = crate::config::config::Color { r: 0, g: 0, b: 0 };
        assert!(should_include_theme(ColorScheme::All, &config));
        assert!(should_include_theme(ColorScheme::Dark, &config));
        assert!(!should_include_theme(ColorScheme::Light, &config));
        config.background = crate::config::config::Color { r: 255, g: 255, b: 255 };
        assert!(should_include_theme(ColorScheme::Light, &config));
        assert!(!should_include_theme(ColorScheme::Dark, &config));
    }

    #[test]
    fn theme_ordering_ignores_case() {
        let mk = |t: &str| ThemeListElement {
            location: themepkg::Location::User,
            path: String::new(),
            theme: t.to_owned(),
            rank: None,
        };
        let mut v = vec![mk("b"), mk("A"), mk("a2")];
        v.sort_unstable_by(ThemeListElement::less_than);
        let names: Vec<&str> = v.iter().map(|t| t.theme.as_str()).collect();
        assert_eq!(vec!["A", "a2", "b"], names);
    }
}
