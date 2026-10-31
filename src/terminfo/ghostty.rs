//! Port of `src/terminfo/ghostty.zig`.
//!
//! STATUS: ported

use std::sync::OnceLock;

use super::source::{Capability, Source, Value};
use crate::terminal::hash_map::Wyhash;

/// Ghostty's terminfo entry.
pub const GHOSTTY: Source = Source {
    names: &[
        // We support the "xterm-" prefix because some poorly behaved programs
        // use this to detect if the terminal supports 256 colors and other
        // features.
        // HACK: This is a hack on a hack...we use "xterm-ghostty" to prevent
        // vim from breaking, and when we do this as the default we break
        // tcell-based applications (lazygit, aerc, etc). tcell has a bug where
        // the primary terminfo name must be the value of TERM.
        // https://github.com/gdamore/tcell/pull/639 fixes the issue but is not
        // merged yet. Consider switching these in the future.
        "xterm-ghostty",

        // The preferred name
        "ghostty",

        // Our "formal" name
        "Ghostty",
    ],

    // NOTE: These capabilities are super underdocumented and I'm not 100%
    // I've got the list or my understanding of any in this list fully correct.
    // As we learn more, please update the comments to better explain what
    // anything means.
    //
    // I've marked some capabilities as "???" if I don't understand what they
    // mean but I just assume I support since other modern terminals do. In
    // this case, I'd love if anyone could help explain what this means and
    // verify that Ghostty does indeed support it and if not we can fix it.
    capabilities: &[
        // automatic right margin -- when reaching the end of a line, text is
        // wrapped to the next line.
        Capability { name: "am", value: Value::Boolean },

        // background color erase -- screen is erased with the background color
        Capability { name: "bce", value: Value::Boolean },

        // terminal can change color definitions, i.e. we can change the color
        // palette.
        Capability { name: "ccc", value: Value::Boolean },

        // supports changing the window title.
        Capability { name: "hs", value: Value::Boolean },

        // terminal has a meta key
        Capability { name: "km", value: Value::Boolean },

        // terminal will not echo input on the screen on its own
        Capability { name: "mc5i", value: Value::Boolean },

        // safe to move (move what?) while in insert/standout mode. (???)
        Capability { name: "mir", value: Value::Boolean },
        Capability { name: "msgr", value: Value::Boolean },

        // no pad character (???)
        Capability { name: "npc", value: Value::Boolean },

        // newline ignored after 80 cols (???)
        Capability { name: "xenl", value: Value::Boolean },

        // Terminal supports default colors
        Capability { name: "AX", value: Value::Boolean },

        // Tmux "truecolor" mode. Other programs also use this to detect
        // if the terminal supports "truecolor". This means that the terminal
        // can display 24-bit RGB colors.
        Capability { name: "Tc", value: Value::Boolean },

        // Colored underlines. https://sw.kovidgoyal.net/kitty/underlines/
        Capability { name: "Su", value: Value::Boolean },

        // Terminal supports a number of xterm extensions
        Capability { name: "XT", value: Value::Boolean },

        // Full keyboard support using Kitty's keyboard protocol:
        // https://sw.kovidgoyal.net/kitty/keyboard-protocol/
        Capability { name: "fullkbd", value: Value::Boolean },

        // Number of colors in the color palette.
        Capability { name: "colors", value: Value::Numeric(256) },

        // Number of columns in a line. Our terminal is variable width on
        // Window resize but this appears to just be the value set by most
        // terminals.
        Capability { name: "cols", value: Value::Numeric(80) },

        // Initial tabstop interval.
        Capability { name: "it", value: Value::Numeric(8) },

        // Number of lines on a page. Similar to cols this is variable width
        // but this appears to be the value set by most terminals.
        Capability { name: "lines", value: Value::Numeric(24) },

        // Number of color pairs on the screen.
        Capability { name: "pairs", value: Value::Numeric(32767) },

        // Alternate character set. This is the VT100 alternate character set.
        // I don't know what the value means, I copied this from Kitty and
        // verified with some other terminals (looks similar).
        Capability { name: "acsc", value: Value::String("++\\,\\,--..00``aaffgghhiijjkkllmmnnooppqqrrssttuuvvwwxxyyzz{{||}}~~") },

        // Curly, dashed, etc underlines
        Capability { name: "Smulx", value: Value::String("\\E[4:%p1%dm") },

        // Overline
        Capability { name: "Smol", value: Value::String("\\E[53m") },
        Capability { name: "Rmol", value: Value::String("\\E[55m") },

        // Colored underlines
        Capability { name: "Setulc", value: Value::String("\\E[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%d%;m") },

        // Cursor styles
        Capability { name: "Ss", value: Value::String("\\E[%p1%d q") },

        // Cursor style reset (to user configured default)
        Capability { name: "Se", value: Value::String("\\E[0 q") },

        // OSC 52 Clipboard
        Capability { name: "Ms", value: Value::String("\\E]52;%p1%s;%p2%s\\007") },

        // Synchronized output
        Capability { name: "Sync", value: Value::String("\\E[?2026%?%p1%{1}%-%tl%eh%;") },

        // Bracketed paste mode
        Capability { name: "BD", value: Value::String("\\E[?2004l") },
        Capability { name: "BE", value: Value::String("\\E[?2004h") },
        // Bracketed paste start/end
        Capability { name: "PS", value: Value::String("\\E[200~") },
        Capability { name: "PE", value: Value::String("\\E[201~") },

        // Mouse
        Capability { name: "XM", value: Value::String("\\E[?1006;1000%?%p1%{1}%=%th%el%;") },
        Capability { name: "xm", value: Value::String("\\E[<%i%p3%d;%p1%d;%p2%d;%?%p4%tM%em%;") },

        // Secondary device attributes request / response
        Capability { name: "RV", value: Value::String("\\E[>c") },
        Capability { name: "rv", value: Value::String("\\E\\\\[[0-9]+;[0-9]+;[0-9]+c") },

        // XTVERSION
        Capability { name: "XR", value: Value::String("\\E[>0q") },
        Capability { name: "xr", value: Value::String("\\EP>\\\\|[ -~]+a\\E\\\\") },

        // DECSLRM (Left/Right Margins)
        Capability { name: "Enmg", value: Value::String("\\E[?69h") },
        Capability { name: "Dsmg", value: Value::String("\\E[?69l") },
        Capability { name: "Clmg", value: Value::String("\\E[s") },
        Capability { name: "Cmg", value: Value::String("\\E[%i%p1%d;%p2%ds") },

        // Clear screen. E3 is the extension to clear scrollback
        Capability { name: "clear", value: Value::String("\\E[H\\E[2J") },
        Capability { name: "E3", value: Value::String("\\E[3J") },

        // Focus reporting. Introduced in ncurses 6.4-20231028
        Capability { name: "fe", value: Value::String("\\E[?1004h") },
        Capability { name: "fd", value: Value::String("\\E[?1004l") },
        Capability { name: "kxIN", value: Value::String("\\E[I") },
        Capability { name: "kxOUT", value: Value::String("\\E[O") },

        // These are all capabilities that should be pretty straightforward
        // and map to input sequences.
        Capability { name: "bel", value: Value::String("^G") },
        Capability { name: "blink", value: Value::String("\\E[5m") },
        Capability { name: "bold", value: Value::String("\\E[1m") },
        Capability { name: "cbt", value: Value::String("\\E[Z") },
        Capability { name: "civis", value: Value::String("\\E[?25l") },
        Capability { name: "cnorm", value: Value::String("\\E[?12l\\E[?25h") },
        Capability { name: "cr", value: Value::String("\\r") },
        Capability { name: "csr", value: Value::String("\\E[%i%p1%d;%p2%dr") },
        Capability { name: "cub", value: Value::String("\\E[%p1%dD") },
        Capability { name: "cub1", value: Value::String("^H") },
        Capability { name: "cud", value: Value::String("\\E[%p1%dB") },
        Capability { name: "cud1", value: Value::String("^J") },
        Capability { name: "cuf", value: Value::String("\\E[%p1%dC") },
        Capability { name: "cuf1", value: Value::String("\\E[C") },
        Capability { name: "cup", value: Value::String("\\E[%i%p1%d;%p2%dH") },
        Capability { name: "cuu", value: Value::String("\\E[%p1%dA") },
        Capability { name: "cuu1", value: Value::String("\\E[A") },
        Capability { name: "cvvis", value: Value::String("\\E[?12;25h") },
        Capability { name: "dch", value: Value::String("\\E[%p1%dP") },
        Capability { name: "dch1", value: Value::String("\\E[P") },
        Capability { name: "dim", value: Value::String("\\E[2m") },
        Capability { name: "dl", value: Value::String("\\E[%p1%dM") },
        Capability { name: "dl1", value: Value::String("\\E[M") },
        Capability { name: "dsl", value: Value::String("\\E]2;\\007") },
        Capability { name: "ech", value: Value::String("\\E[%p1%dX") },
        Capability { name: "ed", value: Value::String("\\E[J") },
        Capability { name: "el", value: Value::String("\\E[K") },
        Capability { name: "el1", value: Value::String("\\E[1K") },
        Capability { name: "flash", value: Value::String("\\E[?5h$<100/>\\E[?5l") },
        Capability { name: "fsl", value: Value::String("^G") },
        Capability { name: "home", value: Value::String("\\E[H") },
        Capability { name: "hpa", value: Value::String("\\E[%i%p1%dG") },
        Capability { name: "ht", value: Value::String("^I") },
        Capability { name: "hts", value: Value::String("\\EH") },
        Capability { name: "ich", value: Value::String("\\E[%p1%d@") },
        Capability { name: "ich1", value: Value::String("\\E[@") },
        Capability { name: "il", value: Value::String("\\E[%p1%dL") },
        Capability { name: "il1", value: Value::String("\\E[L") },
        Capability { name: "ind", value: Value::String("\\n") },
        Capability { name: "indn", value: Value::String("\\E[%p1%dS") },
        Capability { name: "initc", value: Value::String("\\E]4;%p1%d;rgb\\:%p2%{255}%*%{1000}%/%2.2X/%p3%{255}%*%{1000}%/%2.2X/%p4%{255}%*%{1000}%/%2.2X\\E\\\\") },
        Capability { name: "invis", value: Value::String("\\E[8m") },
        Capability { name: "oc", value: Value::String("\\E]104\\007") },
        Capability { name: "op", value: Value::String("\\E[39;49m") },
        Capability { name: "rc", value: Value::String("\\E8") },
        Capability { name: "rep", value: Value::String("%p1%c\\E[%p2%{1}%-%db") },
        Capability { name: "rev", value: Value::String("\\E[7m") },
        Capability { name: "ri", value: Value::String("\\EM") },
        Capability { name: "rin", value: Value::String("\\E[%p1%dT") },
        Capability { name: "ritm", value: Value::String("\\E[23m") },
        Capability { name: "rmacs", value: Value::String("\\E(B") },
        Capability { name: "rmam", value: Value::String("\\E[?7l") },
        Capability { name: "rmcup", value: Value::String("\\E[?1049l") },
        Capability { name: "rmir", value: Value::String("\\E[4l") },
        Capability { name: "rmkx", value: Value::String("\\E[?1l\\E>") },
        Capability { name: "rmso", value: Value::String("\\E[27m") },
        Capability { name: "rmul", value: Value::String("\\E[24m") },
        Capability { name: "rmxx", value: Value::String("\\E[29m") },
        Capability { name: "setab", value: Value::String("\\E[%?%p1%{8}%<%t4%p1%d%e%p1%{16}%<%t10%p1%{8}%-%d%e48;5;%p1%d%;m") },
        Capability { name: "setaf", value: Value::String("\\E[%?%p1%{8}%<%t3%p1%d%e%p1%{16}%<%t9%p1%{8}%-%d%e38;5;%p1%d%;m") },
        Capability { name: "setrgbb", value: Value::String("\\E[48:2:%p1%d:%p2%d:%p3%dm") },
        Capability { name: "setrgbf", value: Value::String("\\E[38:2:%p1%d:%p2%d:%p3%dm") },
        Capability { name: "sgr", value: Value::String("%?%p9%t\\E(0%e\\E(B%;\\E[0%?%p6%t;1%;%?%p5%t;2%;%?%p2%t;4%;%?%p1%p3%|%t;7%;%?%p4%t;5%;%?%p7%t;8%;m") },
        Capability { name: "sgr0", value: Value::String("\\E(B\\E[m") },
        Capability { name: "sitm", value: Value::String("\\E[3m") },
        Capability { name: "smacs", value: Value::String("\\E(0") },
        Capability { name: "smam", value: Value::String("\\E[?7h") },
        Capability { name: "smcup", value: Value::String("\\E[?1049h") },
        Capability { name: "smir", value: Value::String("\\E[4h") },
        Capability { name: "smkx", value: Value::String("\\E[?1h\\E=") },
        Capability { name: "smso", value: Value::String("\\E[7m") },
        Capability { name: "smul", value: Value::String("\\E[4m") },
        Capability { name: "smxx", value: Value::String("\\E[9m") },
        Capability { name: "tbc", value: Value::String("\\E[3g") },
        Capability { name: "tsl", value: Value::String("\\E]2;") },
        Capability { name: "u6", value: Value::String("\\E[%i%d;%dR") },
        Capability { name: "u7", value: Value::String("\\E[6n") },
        Capability { name: "u8", value: Value::String("\\E[?%[;0123456789]c") },
        Capability { name: "u9", value: Value::String("\\E[c") },
        Capability { name: "vpa", value: Value::String("\\E[%i%p1%dd") },

        //-----------------------------------------------------------
        // Completely unvalidated entries that are blindly copied from
        // other terminals (Kitty, Wezterm, Alacritty) and may or may not
        // actually work with Ghostty. todo is to validate these!

        Capability { name: "kDC", value: Value::String("\\E[3;2~") },
        Capability { name: "kDC3", value: Value::String("\\E[3;3~") },
        Capability { name: "kDC4", value: Value::String("\\E[3;4~") },
        Capability { name: "kDC5", value: Value::String("\\E[3;5~") },
        Capability { name: "kDC6", value: Value::String("\\E[3;6~") },
        Capability { name: "kDC7", value: Value::String("\\E[3;7~") },
        Capability { name: "kDN", value: Value::String("\\E[1;2B") },
        Capability { name: "kDN3", value: Value::String("\\E[1;3B") },
        Capability { name: "kDN4", value: Value::String("\\E[1;4B") },
        Capability { name: "kDN5", value: Value::String("\\E[1;5B") },
        Capability { name: "kDN6", value: Value::String("\\E[1;6B") },
        Capability { name: "kDN7", value: Value::String("\\E[1;7B") },
        Capability { name: "kEND", value: Value::String("\\E[1;2F") },
        Capability { name: "kEND3", value: Value::String("\\E[1;3F") },
        Capability { name: "kEND4", value: Value::String("\\E[1;4F") },
        Capability { name: "kEND5", value: Value::String("\\E[1;5F") },
        Capability { name: "kEND6", value: Value::String("\\E[1;6F") },
        Capability { name: "kEND7", value: Value::String("\\E[1;7F") },
        Capability { name: "kHOM", value: Value::String("\\E[1;2H") },
        Capability { name: "kHOM3", value: Value::String("\\E[1;3H") },
        Capability { name: "kHOM4", value: Value::String("\\E[1;4H") },
        Capability { name: "kHOM5", value: Value::String("\\E[1;5H") },
        Capability { name: "kHOM6", value: Value::String("\\E[1;6H") },
        Capability { name: "kHOM7", value: Value::String("\\E[1;7H") },
        Capability { name: "kIC", value: Value::String("\\E[2;2~") },
        Capability { name: "kIC3", value: Value::String("\\E[2;3~") },
        Capability { name: "kIC4", value: Value::String("\\E[2;4~") },
        Capability { name: "kIC5", value: Value::String("\\E[2;5~") },
        Capability { name: "kIC6", value: Value::String("\\E[2;6~") },
        Capability { name: "kIC7", value: Value::String("\\E[2;7~") },
        Capability { name: "kLFT", value: Value::String("\\E[1;2D") },
        Capability { name: "kLFT3", value: Value::String("\\E[1;3D") },
        Capability { name: "kLFT4", value: Value::String("\\E[1;4D") },
        Capability { name: "kLFT5", value: Value::String("\\E[1;5D") },
        Capability { name: "kLFT6", value: Value::String("\\E[1;6D") },
        Capability { name: "kLFT7", value: Value::String("\\E[1;7D") },
        Capability { name: "kNXT", value: Value::String("\\E[6;2~") },
        Capability { name: "kNXT3", value: Value::String("\\E[6;3~") },
        Capability { name: "kNXT4", value: Value::String("\\E[6;4~") },
        Capability { name: "kNXT5", value: Value::String("\\E[6;5~") },
        Capability { name: "kNXT6", value: Value::String("\\E[6;6~") },
        Capability { name: "kNXT7", value: Value::String("\\E[6;7~") },
        Capability { name: "kPRV", value: Value::String("\\E[5;2~") },
        Capability { name: "kPRV3", value: Value::String("\\E[5;3~") },
        Capability { name: "kPRV4", value: Value::String("\\E[5;4~") },
        Capability { name: "kPRV5", value: Value::String("\\E[5;5~") },
        Capability { name: "kPRV6", value: Value::String("\\E[5;6~") },
        Capability { name: "kPRV7", value: Value::String("\\E[5;7~") },
        Capability { name: "kRIT", value: Value::String("\\E[1;2C") },
        Capability { name: "kRIT3", value: Value::String("\\E[1;3C") },
        Capability { name: "kRIT4", value: Value::String("\\E[1;4C") },
        Capability { name: "kRIT5", value: Value::String("\\E[1;5C") },
        Capability { name: "kRIT6", value: Value::String("\\E[1;6C") },
        Capability { name: "kRIT7", value: Value::String("\\E[1;7C") },
        Capability { name: "kUP", value: Value::String("\\E[1;2A") },
        Capability { name: "kUP3", value: Value::String("\\E[1;3A") },
        Capability { name: "kUP4", value: Value::String("\\E[1;4A") },
        Capability { name: "kUP5", value: Value::String("\\E[1;5A") },
        Capability { name: "kUP6", value: Value::String("\\E[1;6A") },
        Capability { name: "kUP7", value: Value::String("\\E[1;7A") },
        Capability { name: "kbs", value: Value::String("^?") },
        Capability { name: "kcbt", value: Value::String("\\E[Z") },
        Capability { name: "kcub1", value: Value::String("\\EOD") },
        Capability { name: "kcud1", value: Value::String("\\EOB") },
        Capability { name: "kcuf1", value: Value::String("\\EOC") },
        Capability { name: "kcuu1", value: Value::String("\\EOA") },
        Capability { name: "kdch1", value: Value::String("\\E[3~") },
        Capability { name: "kend", value: Value::String("\\EOF") },
        Capability { name: "kent", value: Value::String("\\EOM") },
        Capability { name: "kf1", value: Value::String("\\EOP") },
        Capability { name: "kf10", value: Value::String("\\E[21~") },
        Capability { name: "kf11", value: Value::String("\\E[23~") },
        Capability { name: "kf12", value: Value::String("\\E[24~") },
        Capability { name: "kf13", value: Value::String("\\E[1;2P") },
        Capability { name: "kf14", value: Value::String("\\E[1;2Q") },
        Capability { name: "kf15", value: Value::String("\\E[1;2R") },
        Capability { name: "kf16", value: Value::String("\\E[1;2S") },
        Capability { name: "kf17", value: Value::String("\\E[15;2~") },
        Capability { name: "kf18", value: Value::String("\\E[17;2~") },
        Capability { name: "kf19", value: Value::String("\\E[18;2~") },
        Capability { name: "kf2", value: Value::String("\\EOQ") },
        Capability { name: "kf20", value: Value::String("\\E[19;2~") },
        Capability { name: "kf21", value: Value::String("\\E[20;2~") },
        Capability { name: "kf22", value: Value::String("\\E[21;2~") },
        Capability { name: "kf23", value: Value::String("\\E[23;2~") },
        Capability { name: "kf24", value: Value::String("\\E[24;2~") },
        Capability { name: "kf25", value: Value::String("\\E[1;5P") },
        Capability { name: "kf26", value: Value::String("\\E[1;5Q") },
        Capability { name: "kf27", value: Value::String("\\E[1;5R") },
        Capability { name: "kf28", value: Value::String("\\E[1;5S") },
        Capability { name: "kf29", value: Value::String("\\E[15;5~") },
        Capability { name: "kf3", value: Value::String("\\EOR") },
        Capability { name: "kf30", value: Value::String("\\E[17;5~") },
        Capability { name: "kf31", value: Value::String("\\E[18;5~") },
        Capability { name: "kf32", value: Value::String("\\E[19;5~") },
        Capability { name: "kf33", value: Value::String("\\E[20;5~") },
        Capability { name: "kf34", value: Value::String("\\E[21;5~") },
        Capability { name: "kf35", value: Value::String("\\E[23;5~") },
        Capability { name: "kf36", value: Value::String("\\E[24;5~") },
        Capability { name: "kf37", value: Value::String("\\E[1;6P") },
        Capability { name: "kf38", value: Value::String("\\E[1;6Q") },
        Capability { name: "kf39", value: Value::String("\\E[1;6R") },
        Capability { name: "kf4", value: Value::String("\\EOS") },
        Capability { name: "kf40", value: Value::String("\\E[1;6S") },
        Capability { name: "kf41", value: Value::String("\\E[15;6~") },
        Capability { name: "kf42", value: Value::String("\\E[17;6~") },
        Capability { name: "kf43", value: Value::String("\\E[18;6~") },
        Capability { name: "kf44", value: Value::String("\\E[19;6~") },
        Capability { name: "kf45", value: Value::String("\\E[20;6~") },
        Capability { name: "kf46", value: Value::String("\\E[21;6~") },
        Capability { name: "kf47", value: Value::String("\\E[23;6~") },
        Capability { name: "kf48", value: Value::String("\\E[24;6~") },
        Capability { name: "kf49", value: Value::String("\\E[1;3P") },
        Capability { name: "kf5", value: Value::String("\\E[15~") },
        Capability { name: "kf50", value: Value::String("\\E[1;3Q") },
        Capability { name: "kf51", value: Value::String("\\E[1;3R") },
        Capability { name: "kf52", value: Value::String("\\E[1;3S") },
        Capability { name: "kf53", value: Value::String("\\E[15;3~") },
        Capability { name: "kf54", value: Value::String("\\E[17;3~") },
        Capability { name: "kf55", value: Value::String("\\E[18;3~") },
        Capability { name: "kf56", value: Value::String("\\E[19;3~") },
        Capability { name: "kf57", value: Value::String("\\E[20;3~") },
        Capability { name: "kf58", value: Value::String("\\E[21;3~") },
        Capability { name: "kf59", value: Value::String("\\E[23;3~") },
        Capability { name: "kf6", value: Value::String("\\E[17~") },
        Capability { name: "kf60", value: Value::String("\\E[24;3~") },
        Capability { name: "kf61", value: Value::String("\\E[1;4P") },
        Capability { name: "kf62", value: Value::String("\\E[1;4Q") },
        Capability { name: "kf63", value: Value::String("\\E[1;4R") },
        Capability { name: "kf7", value: Value::String("\\E[18~") },
        Capability { name: "kf8", value: Value::String("\\E[19~") },
        Capability { name: "kf9", value: Value::String("\\E[20~") },
        Capability { name: "khome", value: Value::String("\\EOH") },
        Capability { name: "kich1", value: Value::String("\\E[2~") },
        Capability { name: "kind", value: Value::String("\\E[1;2B") },
        Capability { name: "kmous", value: Value::String("\\E[<") },
        Capability { name: "knp", value: Value::String("\\E[6~") },
        Capability { name: "kpp", value: Value::String("\\E[5~") },
        Capability { name: "kri", value: Value::String("\\E[1;2A") },
        Capability { name: "rs1", value: Value::String("\\E]\\E\\\\\\Ec") },
        Capability { name: "sc", value: Value::String("\\E7") },
    ],
};

/// A content-derived version of the encoded terminfo source.
///
/// This is the lowercase hex Wyhash (seed 0) of [`GHOSTTY`] encoded as
/// terminfo source. Zig computes it at comptime; we compute it once lazily.
pub fn version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        let mut hashing = HashingWriter {
            hasher: Wyhash::init(0),
        };
        GHOSTTY.encode(&mut hashing).unwrap();
        format!("{:x}", hashing.hasher.final_())
    })
}

/// `std.Io.Writer.Hashing(std.hash.Wyhash)` with an empty buffer: every
/// write goes straight into the hasher.
struct HashingWriter {
    hasher: Wyhash,
}

impl std::io::Write for HashingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode() {
        // Encode
        let mut buf: Vec<u8> = Vec::with_capacity(1024 * 16);
        GHOSTTY.encode(&mut buf).unwrap();
        assert!(!buf.is_empty());
    }

    // The Zig code builds this map at comptime (stream_terminal.zig), which
    // fails the build on unsupported values; check it builds at runtime.
    #[test]
    fn xtgettcap_map_builds() {
        let map = GHOSTTY.xtgettcap_map();
        assert_eq!(GHOSTTY.capabilities.len() + 3, map.len());
        assert_eq!(
            b"\x1bP1+r544E=787465726D2D67686F73747479\x1b\\",
            map.get(b"544E").unwrap().as_bytes()
        );
        assert!(!version().is_empty());
    }
}
