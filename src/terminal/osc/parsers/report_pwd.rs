//! Port of `src/terminal/osc/parsers/report_pwd.zig`.
//!
//! STATUS: ported

use crate::terminal::osc::{Command, Parser, ReportPwd, State};

/// Parse OSC 7
pub fn parse(parser: &mut Parser, _: Option<u8>) -> Option<Command<'_>> {
    let Some(cap) = parser.capture.as_mut() else {
        parser.state = State::Invalid;
        return None;
    };
    if cap.writer_write_byte(0).is_err() {
        parser.state = State::Invalid;
        return None;
    }
    let data = cap.trailing();
    Some(Command::ReportPwd(ReportPwd {
        value: &data[..data.len() - 1],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc_7_report_pwd() {
        let mut p = Parser::init(None);

        for &ch in b"7;file:///tmp/example" {
            p.next(ch);
        }

        let cmd = p.end(None).unwrap();
        assert_eq!(
            Command::ReportPwd(ReportPwd {
                value: b"file:///tmp/example"
            }),
            cmd
        );
    }

    #[test]
    fn osc_7_report_pwd_empty() {
        let mut p = Parser::init(None);

        for &ch in b"7;" {
            p.next(ch);
        }
        let cmd = p.end(None).unwrap();
        assert_eq!(Command::ReportPwd(ReportPwd { value: b"" }), cmd);
    }
}
