use libc::termios;
use std::os::fd::AsRawFd;
use tokio::io::{self, Stdin, Stdout};

pub struct Terminal {
    pub stdin: Stdin,
    pub stdout: Stdout,
    settings: termios,
}

impl Terminal {
    pub fn new(stdin: Stdin, stdout: Stdout) -> Self {
        let mut settings = std::mem::MaybeUninit::<termios>::uninit();
        unsafe {
            libc::tcgetattr(stdin.as_raw_fd(), settings.as_mut_ptr());
        }

        Self {
            stdin,
            stdout,
            settings: unsafe { settings.assume_init() },
        }
    }

    pub fn enable_raw_mode(&mut self) {
        let mut raw_settings = self.settings;
        unsafe {
            libc::cfmakeraw(&mut raw_settings);
            libc::tcsetattr(self.stdin.as_raw_fd(), libc::TCSANOW, &raw_settings);
        }
    }

    pub fn size(&self) -> io::Result<(u16, u16)> {
        let mut winsize = libc::winsize::default();
        let result =
            unsafe { libc::ioctl(self.stdout.as_raw_fd(), libc::TIOCGWINSZ, &mut winsize) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok((winsize.ws_row, winsize.ws_col))
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.stdin.as_raw_fd(), libc::TCSANOW, &self.settings);
        }
    }
}
