//! Port of `src/terminal/osc/parsers/osc9.zig`.
//!
//! STATUS: ported

use crate::lib::parse_int;
use crate::terminal::osc::parsers::semantic_prompt;
use crate::terminal::osc::{
    Command, ConemuChangeTabTitle, ConemuSleep, ConemuXtermEmulation, Parser, ProgressReport,
    ProgressReportState, ReportPwd, ShowDesktopNotification, State,
};

/// What the ConEmu check decided.
enum ConEmu {
    /// Not a ConEmu OSC: treat it as an iTerm2 notification.
    NotConEmu,
    /// A command that doesn't reference the capture.
    Done(Command<'static>),
    /// A string command: the payload starts at this offset of the
    /// NUL-terminated capture.
    Str(StrKind, usize),
}

enum StrKind {
    Comment,
    ShowMessageBox,
    ChangeTabTitle,
    Guimacro,
    RunProcess,
    OutputEnvironmentVariable,
    ReportPwd,
}

/// Check first to see if this is a ConEmu OSC
/// https://conemu.github.io/en/AnsiEscapeCodes.html#ConEmu_specific_OSC
fn conemu(data: &[u8]) -> ConEmu {
    use ConEmu::*;
    if data.is_empty() {
        return NotConEmu;
    }
    match data[0] {
        // Check for OSC 9;1 9;10 9;11 9;12
        b'1' => {
            if data.len() < 2 {
                return NotConEmu;
            }
            match data[1] {
                // OSC 9;1 sleep
                b';' => Done(Command::ConemuSleep(ConemuSleep {
                    duration_ms: match parse_int::<u16>(&data[2..], 10) {
                        Ok(num) => num.min(10_000),
                        Err(_) => 100,
                    },
                })),
                // OSC 9;10 xterm keyboard and output emulation
                b'0' => {
                    let emu = |keyboard, output| {
                        Done(Command::ConemuXtermEmulation(ConemuXtermEmulation {
                            keyboard,
                            output,
                        }))
                    };
                    if data.len() == 2 {
                        return emu(Some(true), Some(true));
                    }
                    if data.len() < 4 {
                        return NotConEmu;
                    }
                    if data[2] != b';' {
                        return NotConEmu;
                    }
                    match data[3] {
                        b'0' => emu(Some(false), Some(false)),
                        b'1' => emu(Some(true), Some(true)),
                        b'2' => emu(None, Some(false)),
                        b'3' => emu(None, Some(true)),
                        _ => NotConEmu,
                    }
                }
                // OSC 9;11 comment
                b'1' => {
                    if data.len() < 3 {
                        return NotConEmu;
                    }
                    if data[2] != b';' {
                        return NotConEmu;
                    }
                    Str(StrKind::Comment, 3)
                }
                // OSC 9;12 mark prompt start
                b'2' => Done(Command::SemanticPrompt(semantic_prompt::Command::init(
                    semantic_prompt::Action::FreshLineNewPrompt,
                ))),
                _ => NotConEmu,
            }
        }
        // OSC 9;2 show message box
        b'2' => {
            if data.len() < 2 || data[1] != b';' {
                return NotConEmu;
            }
            Str(StrKind::ShowMessageBox, 2)
        }
        // OSC 9;3 change tab title
        b'3' => {
            if data.len() < 2 || data[1] != b';' {
                return NotConEmu;
            }
            if data.len() == 2 {
                return Done(Command::ConemuChangeTabTitle(ConemuChangeTabTitle::Reset));
            }
            Str(StrKind::ChangeTabTitle, 2)
        }
        // OSC 9;4 progress report
        b'4' => {
            if data.len() < 2 || data[1] != b';' {
                return NotConEmu;
            }
            if data.len() < 3 {
                return NotConEmu;
            }
            let mut report = match data[2] {
                b'0' => ProgressReport::new(ProgressReportState::Remove),
                b'1' => ProgressReport {
                    state: ProgressReportState::Set,
                    progress: Some(0),
                },
                b'2' => ProgressReport::new(ProgressReportState::Error),
                b'3' => ProgressReport::new(ProgressReportState::Indeterminate),
                b'4' => ProgressReport::new(ProgressReportState::Pause),
                _ => return NotConEmu,
            };
            match report.state {
                ProgressReportState::Remove | ProgressReportState::Indeterminate => {}
                ProgressReportState::Set
                | ProgressReportState::Error
                | ProgressReportState::Pause => {
                    if data.len() >= 4 && data[3] == b';' {
                        // parse the progress value
                        report.progress = parse_int::<usize>(&data[4..], 10)
                            .ok()
                            .map(|v| v.clamp(0, 100) as u8);
                    }
                }
            }
            Done(Command::ConemuProgressReport(report))
        }
        // OSC 9;5 wait for input
        b'5' => Done(Command::ConemuWaitInput),
        // OSC 9;6 guimacro
        b'6' => {
            if data.len() < 2 || data[1] != b';' {
                return NotConEmu;
            }
            Str(StrKind::Guimacro, 2)
        }
        // OSC 9;7 run process
        b'7' => {
            if data.len() < 2 || data[1] != b';' {
                return NotConEmu;
            }
            Str(StrKind::RunProcess, 2)
        }
        // OSC 9;8 output environment variable
        b'8' => {
            if data.len() < 2 || data[1] != b';' {
                return NotConEmu;
            }
            Str(StrKind::OutputEnvironmentVariable, 2)
        }
        // OSC 9;9 current working directory
        b'9' => {
            if data.len() < 2 || data[1] != b';' {
                return NotConEmu;
            }
            Str(StrKind::ReportPwd, 2)
        }
        _ => NotConEmu,
    }
}

/// Parse OSC 9, which could be an iTerm2 notification or a ConEmu extension.
pub fn parse(parser: &mut Parser, _: Option<u8>) -> Option<Command<'_>> {
    let Some(cap) = parser.capture.as_mut() else {
        parser.state = State::Invalid;
        return None;
    };

    let (kind, start) = match conemu(cap.trailing()) {
        ConEmu::Done(cmd) => return Some(cmd),
        ConEmu::Str(kind, start) => (Some(kind), start),
        // If it's not a ConEmu OSC, it's an iTerm2 notification
        ConEmu::NotConEmu => (None, 0),
    };

    if cap.writer_write_byte(0).is_err() {
        parser.state = State::Invalid;
        return None;
    }
    let data = cap.trailing();
    let value = &data[start..data.len() - 1];
    Some(match kind {
        Some(StrKind::Comment) => Command::ConemuComment(value),
        Some(StrKind::ShowMessageBox) => Command::ConemuShowMessageBox(value),
        Some(StrKind::ChangeTabTitle) => {
            Command::ConemuChangeTabTitle(ConemuChangeTabTitle::Value(value))
        }
        Some(StrKind::Guimacro) => Command::ConemuGuimacro(value),
        Some(StrKind::RunProcess) => Command::ConemuRunProcess(value),
        Some(StrKind::OutputEnvironmentVariable) => {
            Command::ConemuOutputEnvironmentVariable(value)
        }
        Some(StrKind::ReportPwd) => Command::ReportPwd(ReportPwd { value }),
        None => Command::ShowDesktopNotification(ShowDesktopNotification {
            title: b"",
            body: value,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::osc::Allocator;

    fn run(alloc: Option<Allocator>, input: &[u8]) -> Parser {
        let mut p = Parser::init(alloc);
        for &ch in input {
            p.next(ch);
        }
        p
    }

    fn notification_body<'a>(cmd: Option<Command<'a>>) -> &'a [u8] {
        match cmd {
            Some(Command::ShowDesktopNotification(n)) => n.body,
            other => panic!("expected show_desktop_notification, got {:?}", other),
        }
    }

    fn progress(cmd: Option<Command<'_>>) -> ProgressReport {
        match cmd {
            Some(Command::ConemuProgressReport(r)) => r,
            other => panic!("expected conemu_progress_report, got {:?}", other),
        }
    }

    fn xterm(cmd: Option<Command<'_>>) -> ConemuXtermEmulation {
        match cmd {
            Some(Command::ConemuXtermEmulation(x)) => x,
            other => panic!("expected conemu_xterm_emulation, got {:?}", other),
        }
    }

    #[test]
    fn osc_9_numeric_fields_reject_digit_separators() {
        let mut sleep = run(None, b"9;1;4_2");
        assert_eq!(
            Some(Command::ConemuSleep(ConemuSleep { duration_ms: 100 })),
            sleep.end(None)
        );

        let mut prog = run(None, b"9;4;1;4_2");
        assert_eq!(None, progress(prog.end(None)).progress);
    }

    #[test]
    fn osc_9_show_desktop_notification() {
        let mut p = run(None, b"9;Hello world");
        let cmd = p.end(Some(0x1b)).unwrap();
        assert_eq!(
            Command::ShowDesktopNotification(ShowDesktopNotification {
                title: b"",
                body: b"Hello world"
            }),
            cmd
        );
    }

    #[test]
    fn osc_9_show_single_character_desktop_notification() {
        let mut p = run(None, b"9;H");
        let cmd = p.end(Some(0x1b)).unwrap();
        assert_eq!(
            Command::ShowDesktopNotification(ShowDesktopNotification {
                title: b"",
                body: b"H"
            }),
            cmd
        );
    }

    #[test]
    fn osc_9_1_conemu_sleep() {
        let mut p = run(None, b"9;1;420");
        assert_eq!(
            Some(Command::ConemuSleep(ConemuSleep { duration_ms: 420 })),
            p.end(Some(0x1b))
        );
    }

    #[test]
    fn osc_9_1_conemu_sleep_with_no_value_default_to_100ms() {
        let mut p = run(None, b"9;1;");
        assert_eq!(
            Some(Command::ConemuSleep(ConemuSleep { duration_ms: 100 })),
            p.end(Some(0x1b))
        );
    }

    #[test]
    fn osc_9_1_conemu_sleep_cannot_exceed_10000ms() {
        let mut p = run(None, b"9;1;12345");
        assert_eq!(
            Some(Command::ConemuSleep(ConemuSleep { duration_ms: 10000 })),
            p.end(Some(0x1b))
        );
    }

    #[test]
    fn osc_9_1_conemu_sleep_invalid_input() {
        let mut p = run(None, b"9;1;foo");
        assert_eq!(
            Some(Command::ConemuSleep(ConemuSleep { duration_ms: 100 })),
            p.end(Some(0x1b))
        );
    }

    #[test]
    fn osc_9_1_conemu_sleep_to_desktop_notification_1() {
        let mut p = run(None, b"9;1");
        assert_eq!(b"1", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_1_conemu_sleep_to_desktop_notification_2() {
        let mut p = run(None, b"9;1a");
        assert_eq!(b"1a", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_2_conemu_message_box() {
        let mut p = run(None, b"9;2;hello world");
        assert_eq!(
            Some(Command::ConemuShowMessageBox(b"hello world")),
            p.end(Some(0x1b))
        );
    }

    #[test]
    fn osc_9_2_conemu_message_box_invalid_input() {
        let mut p = run(None, b"9;2");
        assert_eq!(b"2", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_2_conemu_message_box_empty_message() {
        let mut p = run(None, b"9;2;");
        assert_eq!(Some(Command::ConemuShowMessageBox(b"")), p.end(Some(0x1b)));
    }

    #[test]
    fn osc_9_2_conemu_message_box_spaces_only_message() {
        let mut p = run(None, b"9;2;   ");
        assert_eq!(Some(Command::ConemuShowMessageBox(b"   ")), p.end(Some(0x1b)));
    }

    #[test]
    fn osc_9_2_message_box_to_desktop_notification_1() {
        let mut p = run(None, b"9;2");
        assert_eq!(b"2", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_2_message_box_to_desktop_notification_2() {
        let mut p = run(None, b"9;2a");
        assert_eq!(b"2a", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_3_conemu_change_tab_title() {
        let mut p = run(None, b"9;3;foo bar");
        assert_eq!(
            Some(Command::ConemuChangeTabTitle(ConemuChangeTabTitle::Value(
                b"foo bar"
            ))),
            p.end(Some(0x1b))
        );
    }

    #[test]
    fn osc_9_3_conemu_change_tab_title_reset() {
        let mut p = run(None, b"9;3;");
        let expected_command = Command::ConemuChangeTabTitle(ConemuChangeTabTitle::Reset);
        assert_eq!(Some(expected_command), p.end(Some(0x1b)));
    }

    #[test]
    fn osc_9_3_conemu_change_tab_title_spaces_only() {
        let mut p = run(None, b"9;3;   ");
        assert_eq!(
            Some(Command::ConemuChangeTabTitle(ConemuChangeTabTitle::Value(
                b"   "
            ))),
            p.end(Some(0x1b))
        );
    }

    #[test]
    fn osc_9_3_change_tab_title_to_desktop_notification_1() {
        let mut p = run(None, b"9;3");
        assert_eq!(b"3", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_3_message_box_to_desktop_notification_2() {
        let mut p = run(None, b"9;3a");
        assert_eq!(b"3a", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_4_conemu_progress_set() {
        let mut p = run(None, b"9;4;1;100");
        let r = progress(p.end(Some(0x1b)));
        assert_eq!(ProgressReportState::Set, r.state);
        assert_eq!(Some(100), r.progress);
    }

    #[test]
    fn osc_9_4_conemu_progress_set_overflow() {
        let mut p = run(None, b"9;4;1;900");
        let r = progress(p.end(Some(0x1b)));
        assert_eq!(ProgressReportState::Set, r.state);
        assert_eq!(Some(100), r.progress);
    }

    #[test]
    fn osc_9_4_conemu_progress_set_single_digit() {
        let mut p = run(None, b"9;4;1;9");
        let r = progress(p.end(Some(0x1b)));
        assert_eq!(ProgressReportState::Set, r.state);
        assert_eq!(Some(9), r.progress);
    }

    #[test]
    fn osc_9_4_conemu_progress_set_double_digit() {
        let mut p = run(None, b"9;4;1;94");
        let r = progress(p.end(Some(0x1b)));
        assert_eq!(ProgressReportState::Set, r.state);
        assert_eq!(Some(94), r.progress);
    }

    #[test]
    fn osc_9_4_conemu_progress_set_extra_semicolon_ignored() {
        let mut p = run(None, b"9;4;1;100");
        let r = progress(p.end(Some(0x1b)));
        assert_eq!(ProgressReportState::Set, r.state);
        assert_eq!(Some(100), r.progress);
    }

    #[test]
    fn osc_9_4_conemu_progress_remove_with_no_progress() {
        let mut p = run(None, b"9;4;0;");
        let r = progress(p.end(Some(0x1b)));
        assert_eq!(ProgressReportState::Remove, r.state);
        assert_eq!(None, r.progress);
    }

    #[test]
    fn osc_9_4_conemu_progress_remove_with_double_semicolon() {
        let mut p = run(None, b"9;4;0;;");
        let r = progress(p.end(Some(0x1b)));
        assert_eq!(ProgressReportState::Remove, r.state);
        assert_eq!(None, r.progress);
    }

    #[test]
    fn osc_9_4_conemu_progress_remove_ignores_progress() {
        let mut p = run(None, b"9;4;0;100");
        let r = progress(p.end(Some(0x1b)));
        assert_eq!(ProgressReportState::Remove, r.state);
        assert_eq!(None, r.progress);
    }

    #[test]
    fn osc_9_4_conemu_progress_remove_extra_semicolon() {
        let mut p = run(None, b"9;4;0;100;");
        let r = progress(p.end(Some(0x1b)));
        assert_eq!(ProgressReportState::Remove, r.state);
    }

    #[test]
    fn osc_9_4_conemu_progress_error() {
        let mut p = run(None, b"9;4;2");
        let r = progress(p.end(Some(0x1b)));
        assert_eq!(ProgressReportState::Error, r.state);
        assert_eq!(None, r.progress);
    }

    #[test]
    fn osc_9_4_conemu_progress_error_with_progress() {
        let mut p = run(None, b"9;4;2;100");
        let r = progress(p.end(Some(0x1b)));
        assert_eq!(ProgressReportState::Error, r.state);
        assert_eq!(Some(100), r.progress);
    }

    #[test]
    fn osc_9_4_progress_pause() {
        let mut p = run(None, b"9;4;4");
        let r = progress(p.end(Some(0x1b)));
        assert_eq!(ProgressReportState::Pause, r.state);
        assert_eq!(None, r.progress);
    }

    #[test]
    fn osc_9_4_conemu_progress_pause_with_progress() {
        let mut p = run(None, b"9;4;4;100");
        let r = progress(p.end(Some(0x1b)));
        assert_eq!(ProgressReportState::Pause, r.state);
        assert_eq!(Some(100), r.progress);
    }

    #[test]
    fn osc_9_4_progress_to_desktop_notification_1() {
        let mut p = run(None, b"9;4");
        assert_eq!(b"4", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_4_progress_to_desktop_notification_2() {
        let mut p = run(None, b"9;4;");
        assert_eq!(b"4;", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_4_progress_to_desktop_notification_3() {
        let mut p = run(None, b"9;4;5");
        assert_eq!(b"4;5", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_4_progress_to_desktop_notification_4() {
        let mut p = run(None, b"9;4;5a");
        assert_eq!(b"4;5a", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_5_conemu_wait_input() {
        let mut p = run(None, b"9;5");
        assert_eq!(Some(Command::ConemuWaitInput), p.end(Some(0x1b)));
    }

    #[test]
    fn osc_9_5_conemu_wait_ignores_trailing_characters() {
        let mut p = run(None, b"9;5;foo");
        assert_eq!(Some(Command::ConemuWaitInput), p.end(Some(0x1b)));
    }

    #[test]
    fn osc_9_6_conemu_guimacro_1() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;6;a");
        assert_eq!(Some(Command::ConemuGuimacro(b"a")), p.end(Some(0x1b)));
    }

    #[test]
    fn osc_9_6_conemu_guimacro_2() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;6;ab");
        assert_eq!(Some(Command::ConemuGuimacro(b"ab")), p.end(Some(0x1b)));
    }

    #[test]
    fn osc_9_6_conemu_guimacro_3_incomplete_to_desktop_notification() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;6");
        assert_eq!(b"6", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_7_conemu_run_process_1() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;7;ab");
        assert_eq!(Some(Command::ConemuRunProcess(b"ab")), p.end(Some(0x1b)));
    }

    #[test]
    fn osc_9_7_conemu_run_process_2() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;7;");
        assert_eq!(Some(Command::ConemuRunProcess(b"")), p.end(Some(0x1b)));
    }

    #[test]
    fn osc_9_7_conemu_run_process_incomplete_to_desktop_notification() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;7");
        assert_eq!(b"7", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_8_conemu_output_environment_variable_1() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;8;ab");
        assert_eq!(
            Some(Command::ConemuOutputEnvironmentVariable(b"ab")),
            p.end(Some(0x1b))
        );
    }

    #[test]
    fn osc_9_8_conemu_output_environment_variable_2() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;8;");
        assert_eq!(
            Some(Command::ConemuOutputEnvironmentVariable(b"")),
            p.end(Some(0x1b))
        );
    }

    #[test]
    fn osc_9_8_conemu_output_environment_variable_incomplete_to_desktop_notification() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;8");
        assert_eq!(b"8", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_9_conemu_set_current_working_directory() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;9;ab");
        assert_eq!(
            Some(Command::ReportPwd(ReportPwd { value: b"ab" })),
            p.end(Some(0x1b))
        );
    }

    #[test]
    fn osc_9_9_conemu_set_current_working_directory_incomplete_to_desktop_notification() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;9");
        assert_eq!(b"9", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_10_conemu_xterm_keyboard_and_output_emulation_1() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;10");
        let x = xterm(p.end(Some(0x1b)));
        assert_eq!(Some(true), x.keyboard);
        assert_eq!(Some(true), x.output);
    }

    #[test]
    fn osc_9_10_conemu_xterm_keyboard_and_output_emulation_2() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;10;0");
        let x = xterm(p.end(Some(0x1b)));
        assert_eq!(Some(false), x.keyboard);
        assert_eq!(Some(false), x.output);
    }

    #[test]
    fn osc_9_10_conemu_xterm_keyboard_and_output_emulation_3() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;10;1");
        let x = xterm(p.end(Some(0x1b)));
        assert_eq!(Some(true), x.keyboard);
        assert_eq!(Some(true), x.output);
    }

    #[test]
    fn osc_9_10_conemu_xterm_keyboard_and_output_emulation_4() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;10;2");
        let x = xterm(p.end(Some(0x1b)));
        assert_eq!(None, x.keyboard);
        assert_eq!(Some(false), x.output);
    }

    #[test]
    fn osc_9_10_conemu_xterm_keyboard_and_output_emulation_5() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;10;3");
        let x = xterm(p.end(Some(0x1b)));
        assert_eq!(None, x.keyboard);
        assert_eq!(Some(true), x.output);
    }

    #[test]
    fn osc_9_10_conemu_xterm_keyboard_and_output_emulation_6() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;10;4");
        assert_eq!(b"10;4", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_10_conemu_xterm_keyboard_and_output_emulation_7() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;10;");
        assert_eq!(b"10;", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_10_conemu_xterm_keyboard_and_output_emulation_8() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;10;abc");
        assert_eq!(b"10;abc", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_11_conemu_comment() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;11;ab");
        assert_eq!(Some(Command::ConemuComment(b"ab")), p.end(Some(0x1b)));
    }

    #[test]
    fn osc_9_11_conemu_comment_incomplete_to_desktop_notification() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;11");
        assert_eq!(b"11", notification_body(p.end(Some(0x1b))));
    }

    #[test]
    fn osc_9_12_conemu_mark_prompt_start_1() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;12");
        assert!(matches!(p.end(Some(0x1b)), Some(Command::SemanticPrompt(_))));
    }

    #[test]
    fn osc_9_12_conemu_mark_prompt_start_2() {
        let mut p = run(Some(Allocator::GLOBAL), b"9;12;abc");
        assert!(matches!(p.end(Some(0x1b)), Some(Command::SemanticPrompt(_))));
    }
}
