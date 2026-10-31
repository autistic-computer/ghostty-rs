//! Port of `src/terminal/osc/parsers/rxvt_extension.zig`.
//!
//! STATUS: ported

use crate::terminal::osc::{Command, Parser, ShowDesktopNotification, State};

const LOG_TARGET: &str = "osc_rxvt_extension";

/// Parse OSC 777
pub fn parse(parser: &mut Parser, _: Option<u8>) -> Option<Command<'_>> {
    let Some(cap) = parser.capture.as_mut() else {
        parser.state = State::Invalid;
        return None;
    };
    // ensure that we are sentinel terminated
    if cap.writer_write_byte(0).is_err() {
        parser.state = State::Invalid;
        return None;
    }
    let data = cap.trailing_mut();
    let Some(k) = data.iter().position(|&c| c == b';') else {
        parser.state = State::Invalid;
        return None;
    };
    let ext = &data[..k];
    if ext != b"notify" {
        log::warn!(
            target: LOG_TARGET,
            "unknown rxvt extension: {}",
            String::from_utf8_lossy(ext)
        );
        parser.state = State::Invalid;
        return None;
    }
    let Some(t) = data[k + 1..]
        .iter()
        .position(|&c| c == b';')
        .map(|p| p + k + 1)
    else {
        log::warn!(target: LOG_TARGET, "rxvt notify extension is missing the title");
        parser.state = State::Invalid;
        return None;
    };
    data[t] = 0;
    let data: &[u8] = data;
    let title = &data[k + 1..t];
    let body = &data[t + 1..data.len() - 1];
    Some(Command::ShowDesktopNotification(ShowDesktopNotification {
        title,
        body,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc_osc_777_show_desktop_notification_with_title() {
        let mut p = Parser::init(None);

        for &ch in b"777;notify;Title;Body" {
            p.next(ch);
        }

        let cmd = p.end(Some(0x1b)).unwrap();
        let Command::ShowDesktopNotification(n) = cmd else {
            panic!("expected show_desktop_notification");
        };
        assert_eq!(n.title, b"Title");
        assert_eq!(n.body, b"Body");
    }
}
