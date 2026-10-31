//! Port of `src/termio/shell_integration.zig`.
//!
//! STATUS: ported

use crate::cli::args::{PackedStruct, StringZ};
use crate::command::EnvMap;
use crate::config;
use crate::config::config::ShellIntegrationFeatures;
use crate::global;
use crate::os::homedir;
use crate::os::shell::ShellCommandBuilder;

/// Shell types we support
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shell {
    Bash,
    Elvish,
    Fish,
    Nushell,
    Zsh,
}

impl Shell {
    /// All shells, in declaration order (Zig `@typeInfo(Shell).enum.fields`).
    pub const ALL: [Shell; 5] = [
        Shell::Bash,
        Shell::Elvish,
        Shell::Fish,
        Shell::Nushell,
        Shell::Zsh,
    ];

    /// The Zig tag name (`@tagName`).
    pub fn as_str(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Elvish => "elvish",
            Shell::Fish => "fish",
            Shell::Nushell => "nushell",
            Shell::Zsh => "zsh",
        }
    }
}

/// The result of setting up a shell integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellIntegration {
    /// The successfully-integrated shell.
    pub shell: Shell,

    /// The command to use to start the shell with the integration.
    /// In most cases this is identical to the command given but for
    /// bash in particular it may be different.
    pub command: config::Command,
}

/// Errors that can occur during setup (Zig: allocation / formatting errors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The inject flags buffer (32 bytes) or features buffer overflowed.
    WriteFailed,
    /// The home directory buffer was too small.
    BufferTooSmall,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::WriteFailed => f.write_str("WriteFailed"),
            Error::BufferTooSmall => f.write_str("BufferTooSmall"),
        }
    }
}

impl std::error::Error for Error {}

/// Set up the command execution environment for automatic
/// integrated shell integration and return a ShellIntegration
/// struct describing the integration.  If integration fails
/// (shell type couldn't be detected, etc.), this will return None.
pub fn setup(
    resource_dir: &str,
    command: &config::Command,
    env: &mut EnvMap,
    force_shell: Option<Shell>,
) -> Result<Option<ShellIntegration>, Error> {
    let shell: Shell = match force_shell.or_else(|| detect_shell(command)) {
        Some(v) => v,
        None => return Ok(None),
    };

    let new_command: Option<config::Command> = match shell {
        Shell::Bash => setup_bash(command, resource_dir, env)?,

        Shell::Nushell => setup_nushell(command, resource_dir, env),

        Shell::Zsh => setup_zsh(command, resource_dir, env),

        Shell::Elvish | Shell::Fish => {
            if !setup_xdg_data_dirs(resource_dir, env) {
                return Ok(None);
            }
            Some(command.clone())
        }
    };

    let Some(new_command) = new_command else {
        return Ok(None);
    };

    Ok(Some(ShellIntegration {
        shell,
        command: new_command,
    }))
}

fn detect_shell(command: &config::Command) -> Option<Shell> {
    let mut arg_iter = command.arg_iterator();

    let arg0 = arg_iter.next()?;
    let exe = basename(&arg0);

    if exe == "bash" {
        // Apple distributes their own patched version of Bash 3.2
        // on macOS that disables the ENV-based POSIX startup path.
        // This means we're unable to perform our automatic shell
        // integration sequence in this specific environment.
        //
        // If we're running "/bin/bash" on Darwin, we can assume
        // we're using Apple's Bash because /bin is non-writable
        // on modern macOS due to System Integrity Protection.
        if cfg!(target_vendor = "apple") && arg0 == "/bin/bash" {
            return None;
        }
        return Some(Shell::Bash);
    }

    match exe {
        "elvish" => Some(Shell::Elvish),
        "fish" => Some(Shell::Fish),
        "nu" => Some(Shell::Nushell),
        "zsh" => Some(Shell::Zsh),
        _ => None,
    }
}

/// Zig `std.fs.path.basename` (POSIX): the last component, ignoring
/// trailing separators.
fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "";
    }
    match trimmed.rfind('/') {
        Some(idx) => &trimmed[idx + 1..],
        None => trimmed,
    }
}

/// Set up the shell integration features environment variable.
pub fn setup_features(
    env: &mut EnvMap,
    features: ShellIntegrationFeatures,
    cursor_blink: bool,
) -> Result<(), Error> {
    let field_names = <ShellIntegrationFeatures as PackedStruct>::FIELD_NAMES;

    // Sort the fields so that the output is deterministic.
    let mut fields_sorted: Vec<(usize, &str)> = field_names.iter().copied().enumerate().collect();
    fields_sorted.sort_unstable_by(|a, b| {
        a.1.to_ascii_lowercase().cmp(&b.1.to_ascii_lowercase())
    });

    let mut buf = String::new();
    for (index, name) in fields_sorted {
        if features.get_field(index) {
            if !buf.is_empty() {
                buf.push(',');
            }
            buf.push_str(name);

            if name == "cursor" {
                buf.push_str(if cursor_blink { ":blink" } else { ":steady" });
            }
        }
    }

    if !buf.is_empty() {
        env.insert("GHOSTTY_SHELL_FEATURES".to_string(), buf);
    }

    Ok(())
}

/// Setup the bash automatic shell integration. This works by
/// starting bash in POSIX mode and using the ENV environment
/// variable to load our bash integration script. This prevents
/// bash from loading its normal startup files, which becomes
/// our script's responsibility (along with disabling POSIX
/// mode).
///
/// This returns a new shell command string that
/// enables the integration or None if integration failed.
fn setup_bash(
    command: &config::Command,
    resource_dir: &str,
    env: &mut EnvMap,
) -> Result<Option<config::Command>, Error> {
    let mut cmd = ShellCommandBuilder::init();

    // Iterator that yields each argument in the original command line.
    let mut iter = command.arg_iterator();

    // Start accumulating arguments with the executable and initial flags.
    match iter.next() {
        Some(exe) => cmd.append_arg(&exe),
        None => return Ok(None),
    }
    cmd.append_arg("--posix");

    // Stores the list of intercepted command line flags that will be passed
    // to our shell integration script: --norc --noprofile
    // We always include at least "1" so the script can differentiate between
    // being manually sourced or automatically injected (from here).
    const INJECT_CAPACITY: usize = 32;
    let mut inject = String::with_capacity(INJECT_CAPACITY);
    let mut write_inject = |s: &str| -> Result<(), Error> {
        if inject.len() + s.len() > INJECT_CAPACITY {
            return Err(Error::WriteFailed);
        }
        inject.push_str(s);
        Ok(())
    };
    write_inject("1")?;

    // Walk through the rest of the given arguments. If we see an option that
    // would require complex or unsupported integration behavior, we bail out
    // and skip loading our shell integration. Users can still manually source
    // the shell integration script.
    //
    // Unsupported options:
    //  -c          -c is always non-interactive
    //  --posix     POSIX mode (a la /bin/sh)
    let mut rcfile: Option<String> = None;
    while let Some(arg) = iter.next() {
        let bytes = arg.as_bytes();
        if arg == "--posix" {
            return Ok(None);
        } else if arg == "--norc" {
            write_inject(" --norc")?;
        } else if arg == "--noprofile" {
            write_inject(" --noprofile")?;
        } else if arg == "--rcfile" || arg == "--init-file" {
            rcfile = iter.next();
        } else if bytes.len() > 1 && bytes[0] == b'-' && bytes[1] != b'-' {
            // '-c command' is always non-interactive
            if bytes.contains(&b'c') {
                return Ok(None);
            }
            cmd.append_arg(&arg);
        } else if arg == "-" || arg == "--" {
            // All remaining arguments should be passed directly to the shell
            // command. We shouldn't perform any further option processing.
            cmd.append_arg(&arg);
            while let Some(remaining_arg) = iter.next() {
                cmd.append_arg(&remaining_arg);
            }
            break;
        } else {
            cmd.append_arg(&arg);
        }
    }

    // Preserve an existing ENV value. We're about to overwrite it.
    if let Some(v) = env.get("ENV").cloned() {
        env.insert("GHOSTTY_BASH_ENV".to_string(), v);
    }

    // Set our new ENV to point to our integration script.
    let script_path = format!("{resource_dir}/shell-integration/bash/ghostty.bash");
    match std::fs::File::open(&script_path) {
        Ok(_file) => {
            env.insert("ENV".to_string(), script_path);
        }
        Err(err) => {
            log::warn!(target: "shell_integration", "unable to open {script_path}: {err}");
            env.remove("GHOSTTY_BASH_ENV");
            return Ok(None);
        }
    }

    env.insert("GHOSTTY_BASH_INJECT".to_string(), inject);
    if let Some(v) = rcfile {
        env.insert("GHOSTTY_BASH_RCFILE".to_string(), v);
    }

    // In POSIX mode, HISTFILE defaults to ~/.sh_history, so unless we're
    // staying in POSIX mode (--posix), change it back to ~/.bash_history.
    if !env.contains_key("HISTFILE") {
        let environ_map = global::environ_map();
        let mut home_buf = [0u8; 1024];
        let home = homedir::home(&environ_map, &mut home_buf).map_err(|_| Error::BufferTooSmall)?;
        if let Some(home) = home {
            let histfile = format!("{home}/.bash_history");
            env.insert("HISTFILE".to_string(), histfile);
            env.insert("GHOSTTY_BASH_UNEXPORT_HISTFILE".to_string(), "1".to_string());
        }
    }

    // Return a copy of our modified command line to use as the shell command.
    Ok(Some(config::Command::Shell(StringZ::from(cmd.written()))))
}

/// Setup automatic shell integration for shells that include
/// their modules from paths in `XDG_DATA_DIRS` env variable.
///
/// The shell-integration path is prepended to `XDG_DATA_DIRS`.
/// It is also saved in the `GHOSTTY_SHELL_INTEGRATION_XDG_DIR` variable
/// so that the shell can refer to it and safely remove this directory
/// from `XDG_DATA_DIRS` when integration is complete.
fn setup_xdg_data_dirs(resource_dir: &str, env: &mut EnvMap) -> bool {
    // Get our path to the shell integration directory.
    let integ_path = format!("{resource_dir}/shell-integration");
    if let Err(err) = open_dir(&integ_path) {
        log::warn!(target: "shell_integration", "unable to open {integ_path}: {err}");
        return false;
    }

    // Set an env var so we can remove this from XDG_DATA_DIRS later.
    // This happens in the shell integration config itself. We do this
    // so that our modifications don't interfere with other commands.
    env.insert(
        "GHOSTTY_SHELL_INTEGRATION_XDG_DIR".to_string(),
        integ_path.clone(),
    );

    // If no XDG_DATA_DIRS set use the default value as specified.
    // This ensures that the default directories aren't lost by setting
    // our desired integration dir directly. See #2711.
    // <https://specifications.freedesktop.org/basedir-spec/0.6/#variables>
    const XDG_DATA_DIRS_KEY: &str = "XDG_DATA_DIRS";
    let value = prepend_env(
        env.get(XDG_DATA_DIRS_KEY)
            .map(String::as_str)
            .unwrap_or("/usr/local/share:/usr/share"),
        &integ_path,
    );
    env.insert(XDG_DATA_DIRS_KEY.to_string(), value);

    true
}

/// Zig `std.Io.Dir.openDirAbsolute` + close: succeeds only if the path is
/// an openable directory.
fn open_dir(path: &str) -> std::io::Result<()> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_dir() {
        return Err(std::io::Error::from(std::io::ErrorKind::NotADirectory));
    }
    std::fs::read_dir(path).map(|_| ())
}

/// Prepend a value to an environment variable such as PATH.
fn prepend_env(current: &str, value: &str) -> String {
    // If there is no prior value, we return it as-is
    if current.is_empty() {
        return value.to_string();
    }

    let delimiter = if cfg!(windows) { ';' } else { ':' };
    format!("{value}{delimiter}{current}")
}

/// Set up automatic Nushell shell integration. This works by adding our
/// shell resource directory to the `XDG_DATA_DIRS` environment variable,
/// which Nushell will use to load `nushell/vendor/autoload/ghostty.nu`.
///
/// We then add `--execute 'use ghostty ...'` to the nu command line to
/// automatically enable our shelll features.
fn setup_nushell(
    command: &config::Command,
    resource_dir: &str,
    env: &mut EnvMap,
) -> Option<config::Command> {
    // Add our XDG_DATA_DIRS entry (for nushell/vendor/autoload/). This
    // makes our 'ghostty' module automatically available, even if any
    // of the later checks abort the rest of our automatic integration.
    if !setup_xdg_data_dirs(resource_dir, env) {
        return None;
    }

    let mut cmd = ShellCommandBuilder::init();

    // Iterator that yields each argument in the original command line.
    let mut iter = command.arg_iterator();

    // Start accumulating arguments with the executable and initial flags.
    cmd.append_arg(&iter.next()?);

    // Tell nu to immediately "use" all of the exported functions in our
    // 'ghostty' module.
    //
    // We can consider making this more specific based on the set of
    // enabled shell features (e.g. `use ghostty sudo`). At the moment,
    // shell features are all runtime-guarded in the nushell script.
    cmd.append_arg("--execute 'use ghostty *'");

    // Walk through the rest of the given arguments. If we see an option that
    // would require complex or unsupported integration behavior, we bail out
    // and skip loading our shell integration. Users can still manually source
    // the shell integration module.
    //
    // Unsupported options:
    //  -c / --command      -c is always non-interactive
    //  --lsp               --lsp starts the language server
    while let Some(arg) = iter.next() {
        let bytes = arg.as_bytes();
        if arg == "--command" || arg == "--lsp" {
            return None;
        } else if bytes.len() > 1 && bytes[0] == b'-' && bytes[1] != b'-' {
            if bytes.contains(&b'c') {
                return None;
            }
            cmd.append_arg(&arg);
        } else if arg == "-" || arg == "--" {
            // All remaining arguments should be passed directly to the shell
            // command. We shouldn't perform any further option processing.
            cmd.append_arg(&arg);
            while let Some(remaining_arg) = iter.next() {
                cmd.append_arg(&remaining_arg);
            }
            break;
        } else {
            cmd.append_arg(&arg);
        }
    }

    // Return a copy of our modified command line to use as the shell command.
    Some(config::Command::Shell(StringZ::from(cmd.written())))
}

/// Setup the zsh automatic shell integration. This works by setting
/// ZDOTDIR to our resources dir so that zsh will load our config. This
/// config then loads the true user config.
fn setup_zsh(
    command: &config::Command,
    resource_dir: &str,
    env: &mut EnvMap,
) -> Option<config::Command> {
    // Preserve an existing ZDOTDIR value. We're about to overwrite it.
    if let Some(old) = env.get("ZDOTDIR").cloned() {
        env.insert("GHOSTTY_ZSH_ZDOTDIR".to_string(), old);
    }

    // Set our new ZDOTDIR to point to our shell resource directory.
    let integ_path = format!("{resource_dir}/shell-integration/zsh");
    if let Err(err) = open_dir(&integ_path) {
        log::warn!(target: "shell_integration", "unable to open {integ_path}: {err}");
        return None;
    }
    env.insert("ZDOTDIR".to_string(), integ_path);

    Some(command.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn shell(s: &str) -> config::Command {
        config::Command::Shell(StringZ::from(s))
    }

    fn shell_str(c: &config::Command) -> &str {
        match c {
            config::Command::Shell(v) => v.as_str(),
            config::Command::Direct(_) => panic!("expected shell command"),
        }
    }

    /// Equivalent of `std.testing.tmpDir`: a unique temporary directory
    /// removed on drop.
    struct TmpDir {
        path: String,
    }

    impl TmpDir {
        fn new() -> TmpDir {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "ghostty-shell-integration-test-{}-{}",
                std::process::id(),
                n
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let path = std::fs::canonicalize(&dir)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            TmpDir { path }
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Test helper that creates a temporary resources directory with shell integration paths.
    struct TmpResourcesDir {
        tmp_dir: TmpDir,
        shell_path: String,
    }

    impl TmpResourcesDir {
        fn init(shell: Shell) -> TmpResourcesDir {
            let tmp_dir = TmpDir::new();
            let relative_shell_path = format!("shell-integration/{}", shell.as_str());
            std::fs::create_dir_all(Path::new(&tmp_dir.path).join(&relative_shell_path)).unwrap();
            let shell_path = format!("{}/{}", tmp_dir.path, relative_shell_path);

            if shell == Shell::Bash {
                std::fs::write(
                    Path::new(&tmp_dir.path).join("shell-integration/bash/ghostty.bash"),
                    "",
                )
                .unwrap();
            }

            TmpResourcesDir {
                tmp_dir,
                shell_path,
            }
        }

        fn path(&self) -> &str {
            &self.tmp_dir.path
        }
    }

    #[test]
    fn force_shell() {
        let mut env = EnvMap::new();

        for shell_ in Shell::ALL {
            let res = TmpResourcesDir::init(shell_);
            let result = setup(res.path(), &shell("sh"), &mut env, Some(shell_)).unwrap();
            assert_eq!(shell_, result.unwrap().shell);
        }
    }

    #[test]
    fn shell_integration_failure() {
        let mut env = EnvMap::new();
        let result = setup("/nonexistent", &shell("sh"), &mut env, None).unwrap();
        assert!(result.is_none());
        assert_eq!(0, env.len());
    }

    #[test]
    fn detect_shell_test() {
        assert!(detect_shell(&shell("sh")).is_none());
        assert_eq!(Some(Shell::Bash), detect_shell(&shell("bash")));
        assert_eq!(Some(Shell::Elvish), detect_shell(&shell("elvish")));
        assert_eq!(Some(Shell::Fish), detect_shell(&shell("fish")));
        assert_eq!(Some(Shell::Nushell), detect_shell(&shell("nu")));
        assert_eq!(Some(Shell::Zsh), detect_shell(&shell("zsh")));

        if cfg!(target_vendor = "apple") {
            assert!(detect_shell(&shell("/bin/bash")).is_none());
        }

        assert_eq!(Some(Shell::Bash), detect_shell(&shell("bash -c 'command'")));
        assert_eq!(Some(Shell::Bash), detect_shell(&shell("\"/a b/bash\"")));
    }

    fn features(
        cursor: bool,
        sudo: bool,
        title: bool,
        ssh_env: bool,
        ssh_terminfo: bool,
        path: bool,
    ) -> ShellIntegrationFeatures {
        ShellIntegrationFeatures {
            cursor,
            sudo,
            title,
            ssh_env,
            ssh_terminfo,
            path,
        }
    }

    #[test]
    fn setup_features_test() {
        // Test: all features enabled
        {
            let mut env = EnvMap::new();
            setup_features(&mut env, features(true, true, true, true, true, true), true).unwrap();
            assert_eq!(
                "cursor:blink,path,ssh-env,ssh-terminfo,sudo,title",
                env.get("GHOSTTY_SHELL_FEATURES").unwrap()
            );
        }

        // Test: all features disabled
        {
            let mut env = EnvMap::new();
            setup_features(&mut env, ShellIntegrationFeatures::from_bits(0), true).unwrap();
            assert!(env.get("GHOSTTY_SHELL_FEATURES").is_none());
        }

        // Test: mixed features
        {
            let mut env = EnvMap::new();
            setup_features(&mut env, features(false, true, false, true, false, false), true)
                .unwrap();
            assert_eq!("ssh-env,sudo", env.get("GHOSTTY_SHELL_FEATURES").unwrap());
        }

        // Test: blinking cursor
        {
            let mut env = EnvMap::new();
            setup_features(&mut env, features(true, false, false, false, false, false), true)
                .unwrap();
            assert_eq!("cursor:blink", env.get("GHOSTTY_SHELL_FEATURES").unwrap());
        }

        // Test: steady cursor
        {
            let mut env = EnvMap::new();
            setup_features(&mut env, features(true, false, false, false, false, false), false)
                .unwrap();
            assert_eq!("cursor:steady", env.get("GHOSTTY_SHELL_FEATURES").unwrap());
        }
    }

    #[test]
    fn bash() {
        let res = TmpResourcesDir::init(Shell::Bash);
        let mut env = EnvMap::new();

        let command = setup_bash(&shell("bash"), res.path(), &mut env).unwrap();
        assert_eq!("bash --posix", shell_str(&command.unwrap()));
        assert_eq!("1", env.get("GHOSTTY_BASH_INJECT").unwrap());
        assert_eq!(
            &format!("{}/ghostty.bash", res.shell_path),
            env.get("ENV").unwrap()
        );
    }

    #[test]
    fn bash_unsupported_options() {
        let res = TmpResourcesDir::init(Shell::Bash);

        let cmdlines = [
            "bash --posix",
            "bash --rcfile script.sh --posix",
            "bash --init-file script.sh --posix",
            "bash -c script.sh",
            "bash -ic script.sh",
        ];

        for cmdline in cmdlines {
            let mut env = EnvMap::new();
            assert!(setup_bash(&shell(cmdline), res.path(), &mut env)
                .unwrap()
                .is_none());
            assert_eq!(0, env.len());
        }
    }

    #[test]
    fn bash_inject_flags() {
        let res = TmpResourcesDir::init(Shell::Bash);

        // bash --norc
        {
            let mut env = EnvMap::new();
            let command = setup_bash(&shell("bash --norc"), res.path(), &mut env).unwrap();
            assert_eq!("bash --posix", shell_str(&command.unwrap()));
            assert_eq!("1 --norc", env.get("GHOSTTY_BASH_INJECT").unwrap());
        }

        // bash --noprofile
        {
            let mut env = EnvMap::new();
            let command = setup_bash(&shell("bash --noprofile"), res.path(), &mut env).unwrap();
            assert_eq!("bash --posix", shell_str(&command.unwrap()));
            assert_eq!("1 --noprofile", env.get("GHOSTTY_BASH_INJECT").unwrap());
        }
    }

    #[test]
    fn bash_rcfile() {
        let res = TmpResourcesDir::init(Shell::Bash);
        let mut env = EnvMap::new();

        // bash --rcfile
        {
            let command =
                setup_bash(&shell("bash --rcfile profile.sh"), res.path(), &mut env).unwrap();
            assert_eq!("bash --posix", shell_str(&command.unwrap()));
            assert_eq!("profile.sh", env.get("GHOSTTY_BASH_RCFILE").unwrap());
        }

        // bash --init-file
        {
            let command =
                setup_bash(&shell("bash --init-file profile.sh"), res.path(), &mut env).unwrap();
            assert_eq!("bash --posix", shell_str(&command.unwrap()));
            assert_eq!("profile.sh", env.get("GHOSTTY_BASH_RCFILE").unwrap());
        }
    }

    #[test]
    fn bash_histfile() {
        let res = TmpResourcesDir::init(Shell::Bash);

        // HISTFILE unset
        {
            let mut env = EnvMap::new();
            let _ = setup_bash(&shell("bash"), res.path(), &mut env).unwrap();
            assert!(env.get("HISTFILE").unwrap().ends_with(".bash_history"));
            assert_eq!("1", env.get("GHOSTTY_BASH_UNEXPORT_HISTFILE").unwrap());
        }

        // HISTFILE set
        {
            let mut env = EnvMap::new();
            env.insert("HISTFILE".into(), "my_history".into());
            let _ = setup_bash(&shell("bash"), res.path(), &mut env).unwrap();
            assert_eq!("my_history", env.get("HISTFILE").unwrap());
            assert!(env.get("GHOSTTY_BASH_UNEXPORT_HISTFILE").is_none());
        }
    }

    #[test]
    fn bash_env() {
        let res = TmpResourcesDir::init(Shell::Bash);
        let mut env = EnvMap::new();
        env.insert("ENV".into(), "env.sh".into());

        let _ = setup_bash(&shell("bash"), res.path(), &mut env).unwrap();
        assert_eq!("env.sh", env.get("GHOSTTY_BASH_ENV").unwrap());
        assert_eq!(
            &format!("{}/ghostty.bash", res.shell_path),
            env.get("ENV").unwrap()
        );
    }

    #[test]
    fn bash_additional_arguments() {
        let res = TmpResourcesDir::init(Shell::Bash);
        let mut env = EnvMap::new();

        // "-" argument separator
        {
            let command =
                setup_bash(&shell("bash - --arg file1 file2"), res.path(), &mut env).unwrap();
            assert_eq!(
                "bash --posix - --arg file1 file2",
                shell_str(&command.unwrap())
            );
        }

        // "--" argument separator
        {
            let command =
                setup_bash(&shell("bash -- --arg file1 file2"), res.path(), &mut env).unwrap();
            assert_eq!(
                "bash --posix -- --arg file1 file2",
                shell_str(&command.unwrap())
            );
        }
    }

    #[test]
    fn bash_missing_resources() {
        let tmp_dir = TmpDir::new();
        let mut env = EnvMap::new();
        assert!(setup_bash(&shell("bash"), &tmp_dir.path, &mut env)
            .unwrap()
            .is_none());
        assert_eq!(0, env.len());
    }

    #[test]
    fn xdg_empty_xdg_data_dirs() {
        let res = TmpResourcesDir::init(Shell::Fish);
        let mut env = EnvMap::new();

        assert!(setup_xdg_data_dirs(res.path(), &mut env));
        assert_eq!(
            &format!("{}/shell-integration", res.path()),
            env.get("GHOSTTY_SHELL_INTEGRATION_XDG_DIR").unwrap()
        );
        assert_eq!(
            &format!(
                "{}/shell-integration:/usr/local/share:/usr/share",
                res.path()
            ),
            env.get("XDG_DATA_DIRS").unwrap()
        );
    }

    #[test]
    fn xdg_existing_xdg_data_dirs() {
        let res = TmpResourcesDir::init(Shell::Fish);
        let mut env = EnvMap::new();
        env.insert("XDG_DATA_DIRS".into(), "/opt/share".into());

        assert!(setup_xdg_data_dirs(res.path(), &mut env));
        assert_eq!(
            &format!("{}/shell-integration", res.path()),
            env.get("GHOSTTY_SHELL_INTEGRATION_XDG_DIR").unwrap()
        );
        assert_eq!(
            &format!("{}/shell-integration:/opt/share", res.path()),
            env.get("XDG_DATA_DIRS").unwrap()
        );
    }

    #[test]
    fn xdg_missing_resources() {
        let tmp_dir = TmpDir::new();
        let mut env = EnvMap::new();
        assert!(!setup_xdg_data_dirs(&tmp_dir.path, &mut env));
        assert_eq!(0, env.len());
    }

    #[test]
    fn nushell() {
        let res = TmpResourcesDir::init(Shell::Nushell);
        let mut env = EnvMap::new();

        let command = setup_nushell(&shell("nu"), res.path(), &mut env);
        assert_eq!(
            "nu --execute 'use ghostty *'",
            shell_str(&command.unwrap())
        );
        assert_eq!(
            &format!("{}/shell-integration", res.path()),
            env.get("GHOSTTY_SHELL_INTEGRATION_XDG_DIR").unwrap()
        );
        assert!(env
            .get("XDG_DATA_DIRS")
            .unwrap()
            .starts_with(&format!("{}/shell-integration", res.path())));
    }

    #[test]
    fn nushell_unsupported_options() {
        let res = TmpResourcesDir::init(Shell::Nushell);

        let cmdlines = [
            "nu --command exit",
            "nu --lsp",
            "nu -c script.sh",
            "nu -ic script.sh",
        ];

        for cmdline in cmdlines {
            let mut env = EnvMap::new();
            assert!(setup_nushell(&shell(cmdline), res.path(), &mut env).is_none());
            assert!(env.get("XDG_DATA_DIRS").is_some());
            assert!(env.get("GHOSTTY_SHELL_INTEGRATION_XDG_DIR").is_some());
        }
    }

    #[test]
    fn nushell_missing_resources() {
        let tmp_dir = TmpDir::new();
        let mut env = EnvMap::new();
        assert!(setup_nushell(&shell("nu"), &tmp_dir.path, &mut env).is_none());
        assert_eq!(0, env.len());
    }

    #[test]
    fn zsh() {
        let res = TmpResourcesDir::init(Shell::Zsh);
        let mut env = EnvMap::new();

        let command = setup_zsh(&shell("zsh"), res.path(), &mut env);
        assert_eq!("zsh", shell_str(&command.unwrap()));
        assert_eq!(&res.shell_path, env.get("ZDOTDIR").unwrap());
        assert!(env.get("GHOSTTY_ZSH_ZDOTDIR").is_none());
    }

    #[test]
    fn zsh_zdotdir() {
        let res = TmpResourcesDir::init(Shell::Zsh);
        let mut env = EnvMap::new();
        env.insert("ZDOTDIR".into(), "$HOME/.config/zsh".into());

        let command = setup_zsh(&shell("zsh"), res.path(), &mut env);
        assert_eq!("zsh", shell_str(&command.unwrap()));
        assert_eq!(&res.shell_path, env.get("ZDOTDIR").unwrap());
        assert_eq!("$HOME/.config/zsh", env.get("GHOSTTY_ZSH_ZDOTDIR").unwrap());
    }

    #[test]
    fn zsh_missing_resources() {
        let tmp_dir = TmpDir::new();
        let mut env = EnvMap::new();
        assert!(setup_zsh(&shell("zsh"), &tmp_dir.path, &mut env).is_none());
        assert_eq!(0, env.len());
    }
}
