use std::fmt;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use curve25519_dalek::{edwards::CompressedEdwardsY, traits::IsIdentity};
use snow::TransportState;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::channel::handshake;
use crate::noise;
use crate::{ChannelError, DeviceId, Role};

const CODE_SPACE: u32 = 1_000_000;
const PROLOGUE: &[u8] = b"Velmira device enrollment v1";
const PAKE_INITIATOR: &[u8] = b"Velmira pairing initiator v1";
const PAKE_RESPONDER: &[u8] = b"Velmira pairing responder v1";
const NOISE_PARAMS: &str = "Noise_XXpsk0_25519_ChaChaPoly_SHA256";
const PAKE_BYTES: usize = 33;
const MAX_TIMEOUT: Duration = Duration::from_secs(30);
const READY: u8 = 0x01;
const COMPLETE: u8 = 0x02;
const CONFIRMATION_BYTES: usize = 17;

/// One six-digit enrollment invitation. Consumed by a single pairing attempt.
pub struct PairingCode(Zeroizing<[u8; 6]>);

impl PairingCode {
    /// Select from the full code space uniformly, including leading zeroes.
    pub fn generate() -> Result<Self, PairingError> {
        let limit = u32::MAX - u32::MAX % CODE_SPACE;
        loop {
            let sample = getrandom::u32().map_err(PairingError::Random)?;
            if sample < limit {
                let mut value = sample % CODE_SPACE;
                let mut digits = [b'0'; 6];
                for digit in digits.iter_mut().rev() {
                    *digit += (value % 10) as u8;
                    value /= 10;
                }
                return Ok(Self(Zeroizing::new(digits)));
            }
        }
    }

    pub fn parse(input: &str) -> Result<Self, PairingError> {
        let digits: [u8; 6] = input
            .as_bytes()
            .try_into()
            .map_err(|_| PairingError::InvalidCode)?;
        if !digits.iter().all(u8::is_ascii_digit) {
            return Err(PairingError::InvalidCode);
        }
        Ok(Self(Zeroizing::new(digits)))
    }

    /// Use only for displaying the invitation. Avoid logging these digits.
    pub fn digits(&self) -> &str {
        std::str::from_utf8(self.0.as_ref()).expect("pairing codes contain ASCII digits")
    }
}

impl fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingCode([redacted])")
    }
}

#[derive(Debug, Error)]
pub enum PairingError {
    #[error("the pairing code must contain six ASCII digits")]
    InvalidCode,
    #[error("system random number generator failed: {0}")]
    Random(getrandom::Error),
    #[error("invalid pairing timeout")]
    InvalidTimeout,
    #[error("pairing I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid SPAKE2 pairing message")]
    Pake,
    #[error("pairing authentication or confirmation failed")]
    Authentication,
    #[error("the pairing peer did not present a Noise static key")]
    MissingKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairedPeer {
    pub public_key: [u8; 32],
    pub device_id: DeviceId,
}

/// Enroll a peer on one TCP connection. SPAKE2 resists offline guesses for the
/// short code, then Noise authenticates the peer's actual static X25519 key
/// using the SPAKE2 session secret. The connection closes after enrollment.
///
/// Generate a new invitation for each attempt and expose the pairing endpoint
/// only while the user has invited another device. Store the returned public
/// key in the caller's trust store. Future sync connects via the pinned key.
pub fn pair(
    stream: TcpStream,
    role: Role,
    private_key: &[u8; 32],
    code: PairingCode,
    timeout: Duration,
) -> Result<PairedPeer, PairingError> {
    if timeout.is_zero() || timeout > MAX_TIMEOUT {
        return Err(PairingError::InvalidTimeout);
    }
    let mut stream = DeadlineStream {
        stream,
        deadline: Instant::now() + timeout,
    };
    let first = Identity::new(PAKE_INITIATOR);
    let second = Identity::new(PAKE_RESPONDER);
    let (state, ours) = {
        let password = Password::new(code.0.as_ref());
        drop(code);
        match role {
            Role::Initiator => Spake2::<Ed25519Group>::start_a(&password, &first, &second),
            Role::Responder => Spake2::<Ed25519Group>::start_b(&password, &first, &second),
        }
    };

    let mut theirs = [0; PAKE_BYTES];
    match role {
        Role::Initiator => {
            stream.write_all(&ours)?;
            stream.read_exact(&mut theirs)?;
        }
        Role::Responder => {
            stream.read_exact(&mut theirs)?;
            stream.write_all(&ours)?;
        }
    }
    validate_pake_point(&theirs)?;
    let secret = Zeroizing::new(state.finish(&theirs).map_err(|_| PairingError::Pake)?);
    let psk: &[u8; 32] = secret
        .as_slice()
        .try_into()
        .map_err(|_| PairingError::Pake)?;
    let setup = || -> Result<_, snow::Error> {
        noise::builder(NOISE_PARAMS)?
            .prologue(PROLOGUE)?
            .psk(0, psk)
    };
    let builder = setup().map_err(|_| PairingError::Authentication)?;
    let state = handshake(&mut stream, role, builder, private_key, None).map_err(map_channel)?;
    drop(secret);
    let public_key = state
        .get_remote_static()
        .and_then(|key| <[u8; 32]>::try_from(key).ok())
        .ok_or(PairingError::MissingKey)?;
    let mut channel = state
        .into_transport_mode()
        .map_err(|_| PairingError::Authentication)?;
    match role {
        Role::Initiator => {
            read_confirmation(&mut stream, &mut channel, READY)?;
            write_confirmation(&mut stream, &mut channel, COMPLETE)?;
        }
        Role::Responder => {
            write_confirmation(&mut stream, &mut channel, READY)?;
            read_confirmation(&mut stream, &mut channel, COMPLETE)?;
        }
    }
    Ok(PairedPeer {
        public_key,
        device_id: DeviceId::from_public_key(&public_key),
    })
}

fn map_channel(error: ChannelError) -> PairingError {
    match error {
        ChannelError::Io(error) => PairingError::Io(error),
        _ => PairingError::Authentication,
    }
}

fn validate_pake_point(message: &[u8; PAKE_BYTES]) -> Result<(), PairingError> {
    // spake2 0.4 decodes the Edwards point; check its prime-order subgroup
    // and canonical encoding before passing it into the password exchange.
    let compressed = CompressedEdwardsY(message[1..].try_into().map_err(|_| PairingError::Pake)?);
    let point = compressed.decompress().ok_or(PairingError::Pake)?;
    if point.is_identity() || !point.is_torsion_free() || point.compress() != compressed {
        return Err(PairingError::Pake);
    }
    Ok(())
}

fn write_confirmation(
    stream: &mut DeadlineStream,
    channel: &mut TransportState,
    value: u8,
) -> Result<(), PairingError> {
    let mut encrypted = [0; CONFIRMATION_BYTES];
    let size = channel
        .write_message(&[value], &mut encrypted)
        .map_err(|_| PairingError::Authentication)?;
    if size != encrypted.len() {
        return Err(PairingError::Authentication);
    }
    stream.write_all(&encrypted)?;
    Ok(())
}

fn read_confirmation(
    stream: &mut DeadlineStream,
    channel: &mut TransportState,
    expected: u8,
) -> Result<(), PairingError> {
    let mut encrypted = [0; CONFIRMATION_BYTES];
    stream.read_exact(&mut encrypted)?;
    let mut confirmation = [0; 1];
    let size = channel
        .read_message(&encrypted, &mut confirmation)
        .map_err(|_| PairingError::Authentication)?;
    if size != 1 || confirmation[0] != expected {
        return Err(PairingError::Authentication);
    }
    Ok(())
}

struct DeadlineStream {
    stream: TcpStream,
    deadline: Instant,
}

impl DeadlineStream {
    fn remaining(&self) -> io::Result<Duration> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "pairing deadline elapsed",
            ));
        }
        Ok(remaining)
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(bytes)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
