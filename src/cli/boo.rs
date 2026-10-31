//! Port of `src/cli/boo.zig`.
//!
//! STATUS: ported
//!
//! The animation frames come from `src/build/framegen/frames` in the Zig
//! tree, which the Zig build joins with `\x01` and compresses with raw
//! DEFLATE (`src/build/framegen/main.c`). The same compressed data is
//! checked in as `framedata.compressed` (generated with zlib, default
//! level, `wbits = -15`, frame files sorted by name) and decompressed at
//! runtime with `flate2`. The `vxfw` app/widget framework is replaced by a
//! small event loop over the [`tui::vaxis`](super::tui::vaxis) stand-in.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use super::args::{self, cli_struct, Error, ParseHooks};
use super::ghostty::{global_args, Action, RunResult};
use super::tui::vaxis;

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {}
    with { custom_hooks }
}

impl ParseHooks for Options {
    /// Enables `-h` and `--help` to work.
    fn help(&mut self) -> Option<Result<(), Error>> {
        Action::help_result()
    }
}

/// The help text of this action (Zig `help_strings.Action.boo`).
pub const HELP: &str = include_str!("boo_help.txt");

/// The compressed frame data (Zig `@import("framedata").compressed`).
const FRAMEDATA: &[u8] = include_bytes!("framedata.compressed");

/// Width of a single frame
const FRAME_WIDTH: u16 = 100;
/// Height of a single frame
const FRAME_HEIGHT: u16 = 41;

struct Boo {
    frame: usize,
    framerate: u32, // 30 fps
    buffer: Vec<vaxis::Cell>,

    ghostty_style: vaxis::Style,
    outline_style: vaxis::Style,

    /// The decompressed frames (Zig: the `frames` global).
    frames: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Normal,
    Span,
    InTag,
    InClosingTag,
}

impl Boo {
    /// Updates our internal buffer with the current frame, then advances
    /// the frame index
    fn update_frame(&mut self) {
        let frame = &self.frames[self.frame];
        // A frame is characters with html spans. When we encounter a span, we use the outline style
        // until the span ends. That is, when we find a '<', we parse until '>'. Then we use the
        // outline styule until the next '<', and skip until the next '>'

        let mut cell_idx: usize = 0;

        for line in frame.split('\n') {
            let mut state = State::Normal;
            let mut style = self.ghostty_style;
            for c in line.chars() {
                match state {
                    State::Normal => {
                        if c == '<' {
                            state = State::InTag;
                            // We will be entering a span
                            style = self.outline_style;
                            continue;
                        }
                    }
                    State::Span => {
                        if c == '<' {
                            state = State::InTag;
                            style = self.ghostty_style;
                            continue;
                        }
                    }
                    State::InTag => {
                        // If we encounter a '/', we are a closing tag
                        // If we parse all the way to a '>' we are an opening tag: we are now in a span
                        if c == '/' {
                            state = State::InClosingTag;
                        } else if c == '>' {
                            state = State::Span;
                        }
                        continue;
                    }
                    State::InClosingTag => {
                        // If we are closing a tag, we will enter the normal state
                        if c == '>' {
                            state = State::Normal;
                        }
                        continue;
                    }
                }
                self.buffer[cell_idx] = vaxis::Cell {
                    char: vaxis::Character {
                        grapheme: c.to_string(),
                        width: 1,
                    },
                    style,
                    ..Default::default()
                };
                cell_idx += 1;
            }
        }
        debug_assert_eq!(cell_idx, self.buffer.len());

        // Lastly, update the frame index
        self.frame += 1;
        if self.frame == self.frames.len() {
            self.frame = 0;
        }
    }

    fn draw(&self, win: vaxis::Window<'_>) {
        win.clear();

        // Warn for screen size
        if win.width < FRAME_WIDTH || win.height < FRAME_HEIGHT {
            let text = "Screen must be at least 100w x 41h";
            // vxfw.Center of a vxfw.Text
            let text_width = vaxis::gwidth(text).min(win.width);
            let col = (win.width - text_width) / 2;
            let row = win.height.saturating_sub(1) / 2;
            win.print_segment(
                vaxis::Segment {
                    text,
                    ..Default::default()
                },
                vaxis::PrintOptions {
                    row_offset: row,
                    col_offset: col,
                    ..Default::default()
                },
            );
            return;
        }

        // Calculate x and y offsets to center the animation frame
        let offset_y = (win.height - FRAME_HEIGHT) / 2;
        let offset_x = (win.width - FRAME_WIDTH) / 2;

        let child = win.child(vaxis::ChildOptions {
            x_off: offset_x as i32,
            y_off: offset_y as i32,
            width: Some(FRAME_WIDTH),
            height: Some(FRAME_HEIGHT),
            ..Default::default()
        });
        for (i, cell) in self.buffer.iter().enumerate() {
            let col = (i % FRAME_WIDTH as usize) as u16;
            let row = (i / FRAME_WIDTH as usize) as u16;
            child.write_cell(col, row, cell.clone());
        }
    }
}

/// The `boo` command is used to display the animation from the Ghostty website in the terminal
pub fn run() -> RunResult {
    // Disable on non-desktop systems.
    if !cfg!(any(windows, target_os = "macos", target_os = "linux", target_os = "freebsd")) {
        return Ok(1);
    }

    let mut opts = Options::default();

    {
        let mut iter = args::args_iterator(global_args());
        args::parse(&mut opts, &mut iter)?;
    }

    let frames = decompress_frames()?;

    let mut boo = Boo {
        frame: 0,
        framerate: 1000 / 30,
        buffer: vec![vaxis::Cell::default(); FRAME_WIDTH as usize * FRAME_HEIGHT as usize],
        ghostty_style: vaxis::Style::default(),
        outline_style: vaxis::Style {
            fg: vaxis::Color::Index(4),
            ..Default::default()
        },
        frames,
    };

    let mut tty = vaxis::Tty::init()?;
    let mut vx = vaxis::Vaxis::init();
    let mut event_loop = vaxis::Loop::start(&mut tty)?;
    let result = run_app(&mut boo, &mut tty, &mut vx, &mut event_loop);
    vx.deinit(tty.writer());
    event_loop.stop(&mut tty);
    result?;

    Ok(0)
}

/// The vxfw App run loop: `init`/`tick` events advance the animation,
/// ctrl+c or escape quits.
fn run_app(
    boo: &mut Boo,
    tty: &mut vaxis::Tty,
    vx: &mut vaxis::Vaxis,
    event_loop: &mut vaxis::Loop,
) -> std::io::Result<()> {
    vx.enter_alt_screen(tty.writer())?;
    let winsize = tty.get_winsize()?;
    vx.resize(tty.writer(), winsize)?;

    // The `.init` event.
    boo.update_frame();
    let mut next_tick = Instant::now() + Duration::from_millis(boo.framerate as u64);
    loop {
        boo.draw(vx.window());
        vx.render(tty.writer())?;
        tty.writer().flush()?;

        let now = Instant::now();
        let timeout = next_tick.saturating_duration_since(now);
        match event_loop.next_event(vx, Some(timeout))? {
            None => {
                // `.tick`
                boo.update_frame();
                next_tick = Instant::now() + Duration::from_millis(boo.framerate as u64);
            }
            Some(vaxis::Event::KeyPress(key)) => {
                if key.matches('c' as u32, vaxis::Modifiers::CTRL)
                    || key.matches(vaxis::Key::ESCAPE, vaxis::Modifiers::NONE)
                {
                    return Ok(());
                }
            }
            Some(vaxis::Event::Winsize(ws)) => {
                vx.resize(tty.writer(), ws)?;
            }
            Some(_) => {}
        }
    }
}

/// Decompress the frames into a list of individual frames
fn decompress_frames() -> std::io::Result<Vec<String>> {
    let mut decompress = flate2::read::DeflateDecoder::new(FRAMEDATA);
    let mut decompressed_data = String::new();
    decompress.read_to_string(&mut decompressed_data)?;

    let mut frame_list: Vec<String> = Vec::with_capacity(235);
    for frame in decompressed_data.split('\x01') {
        frame_list.push(frame.to_owned());
    }
    Ok(frame_list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_decompress_and_fill_the_buffer() {
        let frames = decompress_frames().unwrap();
        assert_eq!(235, frames.len());
        let mut boo = Boo {
            frame: 0,
            framerate: 1000 / 30,
            buffer: vec![vaxis::Cell::default(); FRAME_WIDTH as usize * FRAME_HEIGHT as usize],
            ghostty_style: vaxis::Style::default(),
            outline_style: vaxis::Style {
                fg: vaxis::Color::Index(4),
                ..Default::default()
            },
            frames,
        };
        // Every frame must fill exactly the frame buffer (asserted in
        // update_frame).
        for _ in 0..boo.frames.len() {
            boo.update_frame();
        }
        assert_eq!(0, boo.frame);
        assert!(boo
            .buffer
            .iter()
            .any(|c| c.style == boo.outline_style));
    }
}
