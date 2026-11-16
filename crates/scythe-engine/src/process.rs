//! Child processes: ffmpeg, encoders, Python, the plugin-scan helper.

/// A command for `program`. On Windows the child gets no console window of its own: Scythe is a
/// GUI program there, so a console child would otherwise open one. Every child process goes
/// through here (`clippy.toml` disallows `Command::new` in the crates that start them).
#[allow(clippy::disallowed_methods)]
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    #[allow(unused_mut)]
    let mut command = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    command
}
