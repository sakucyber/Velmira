use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// A TCP stream with a shared absolute deadline and per-I/O idle timeout.
/// Repeated short reads or writes never extend the absolute deadline.
pub struct TimedStream {
    stream: TcpStream,
    deadline: Instant,
    idle: Duration,
}

impl TimedStream {
    pub fn new(stream: TcpStream, deadline: Instant, idle: Duration) -> Self {
        Self {
            stream,
            deadline,
            idle,
        }
    }

    fn remaining(&self) -> io::Result<Duration> {
        let remaining = self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(self.idle);
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "connection deadline elapsed",
            ));
        }
        Ok(remaining)
    }
}

impl Read for TimedStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(bytes)
    }
}

impl Write for TimedStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
