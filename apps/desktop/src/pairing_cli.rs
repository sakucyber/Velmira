use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use vm_sync::{generate_keypair, pair, DeviceId, PairedPeer, PairingCode, Role};
use zeroize::Zeroizing;

const INVITATION_LIFETIME: Duration = Duration::from_secs(120);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub enum Command {
    Listen(SocketAddr),
    Connect(SocketAddr),
    List,
    Revoke(String),
}

#[cfg(windows)]
pub fn attach_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};

    // GUI-subsystem release builds are detached from the invoking terminal.
    // SAFETY: AttachConsole accepts this reserved constant for the parent.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[cfg(not(windows))]
pub fn attach_console() {}

pub fn run(command: Command, data_dir: &Path) -> Result<()> {
    match command {
        Command::Listen(address) => {
            validate_address(address)?;
            let mut store = TrustStore::open(data_dir)?;
            let identity = store.identity()?;
            let listener = TcpListener::bind(address)?;
            let code = PairingCode::generate()?;
            let deadline = Instant::now() + INVITATION_LIFETIME;
            println!("Listening on {}", listener.local_addr()?);
            println!("Pairing code: {}", code.digits());
            println!("This invitation is valid for one attempt within two minutes.");
            io::stdout().flush()?;
            let stream = accept_until(listener, deadline)?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(
                    io::Error::new(io::ErrorKind::TimedOut, "pairing invitation expired").into(),
                );
            }
            let peer = pair(
                stream,
                Role::Responder,
                &identity.private,
                code,
                remaining.min(HANDSHAKE_TIMEOUT),
            )?;
            store.enroll(&identity, peer)?;
        }
        Command::Connect(address) => {
            validate_address(address)?;
            if address.port() == 0 {
                return Err("the peer port must be nonzero".into());
            }
            let code = read_code()?;
            let mut store = TrustStore::open(data_dir)?;
            let identity = store.identity()?;
            let stream = TcpStream::connect_timeout(&address, CONNECTION_TIMEOUT)?;
            let peer = pair(
                stream,
                Role::Initiator,
                &identity.private,
                code,
                HANDSHAKE_TIMEOUT,
            )?;
            store.enroll(&identity, peer)?;
        }
        Command::List => TrustStore::open(data_dir)?.list()?,
        Command::Revoke(text) => TrustStore::open(data_dir)?.revoke(parse_id(&text)?)?,
    }
    Ok(())
}

fn validate_address(address: SocketAddr) -> Result<()> {
    let ip = address.ip();
    if ip.is_unspecified() || ip.is_multicast() || matches!(ip, IpAddr::V4(ip) if ip.is_broadcast())
    {
        return Err("use the numeric address of a specific local or peer interface".into());
    }
    Ok(())
}

fn read_code() -> Result<PairingCode> {
    eprintln!("Enter the six-digit pairing code:");
    let mut input = Zeroizing::new(Vec::with_capacity(9));
    io::stdin().lock().take(9).read_until(b'\n', &mut input)?;
    let input = std::str::from_utf8(input.as_slice())?;
    let digits = input.strip_suffix('\n').unwrap_or(input);
    let digits = digits.strip_suffix('\r').unwrap_or(digits);
    Ok(PairingCode::parse(digits)?)
}

fn accept_until(listener: TcpListener, deadline: Instant) -> io::Result<TcpStream> {
    listener.set_nonblocking(true)?;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "pairing invitation expired",
            ));
        }
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false)?;
                return Ok(stream);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(remaining.min(Duration::from_millis(25)));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

struct Identity {
    private: Zeroizing<[u8; 32]>,
    public: [u8; 32],
}

struct TrustStore(Connection);

impl TrustStore {
    fn open(data_dir: &Path) -> Result<Self> {
        fs::create_dir_all(data_dir)?;
        let folder = data_dir.join("sync");
        let path = folder.join("devices.sqlite3");
        prepare_store(&folder, &path)?;
        let db = Connection::open_with_flags(
            path,
            OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS local_identity (
                 singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                 private_key BLOB NOT NULL,
                 public_key BLOB NOT NULL CHECK(length(public_key) = 32)
             );
             CREATE TABLE IF NOT EXISTS trusted_peer (
                 device_id BLOB PRIMARY KEY NOT NULL CHECK(length(device_id) = 16),
                 public_key BLOB UNIQUE NOT NULL CHECK(length(public_key) = 32)
             );",
        )?;
        Ok(Self(db))
    }

    fn identity(&mut self) -> Result<Identity> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT private_key, public_key FROM local_identity WHERE singleton = 1",
                [],
                |row| {
                    Ok((
                        Zeroizing::new(row.get::<_, Vec<u8>>(0)?),
                        row.get::<_, Vec<u8>>(1)?,
                    ))
                },
            )
            .optional()?;
        let identity = if let Some((sealed, public)) = existing {
            Identity {
                private: open_identity(sealed)?,
                public: public.try_into().map_err(|_| "invalid stored public key")?,
            }
        } else {
            let keypair = generate_keypair()?;
            let private = Zeroizing::new(keypair.private);
            let mut identity = Identity {
                private: private_key(&private)?,
                public: keypair
                    .public
                    .try_into()
                    .map_err(|_| "invalid generated public key")?,
            };
            let sealed = seal_identity(&mut identity.private)?;
            tx.execute(
                "INSERT INTO local_identity (singleton, private_key, public_key) VALUES (1, ?1, ?2)",
                params![sealed.as_slice(), identity.public.as_slice()],
            )?;
            identity
        };
        tx.commit()?;
        Ok(identity)
    }

    fn enroll(&self, ours: &Identity, peer: PairedPeer) -> Result<()> {
        if ours.public == peer.public_key {
            return Err("a device cannot pair with itself".into());
        }
        let changed = self.0.execute(
            "INSERT INTO trusted_peer(device_id, public_key) VALUES (?1, ?2)
             ON CONFLICT(device_id) DO UPDATE SET public_key = excluded.public_key
             WHERE trusted_peer.public_key = excluded.public_key",
            params![peer.device_id.0.as_slice(), peer.public_key.as_slice()],
        )?;
        if changed == 0 {
            return Err("the paired device ID belongs to a different key".into());
        }
        println!("Paired device {}", display_id(peer.device_id));
        Ok(())
    }

    fn list(&self) -> Result<()> {
        let mut query = self
            .0
            .prepare("SELECT device_id FROM trusted_peer ORDER BY device_id")?;
        let ids = query.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
        for id in ids {
            let id = id?.try_into().map_err(|_| "invalid stored device ID")?;
            println!("{}", display_id(DeviceId(id)));
        }
        Ok(())
    }

    fn revoke(&self, peer: DeviceId) -> Result<()> {
        let changed = self.0.execute(
            "DELETE FROM trusted_peer WHERE device_id = ?1",
            [peer.0.as_slice()],
        )?;
        if changed == 0 {
            return Err("paired device not found".into());
        }
        println!("Revoked device {}", display_id(peer));
        Ok(())
    }
}

fn display_id(id: DeviceId) -> String {
    let mut text = String::with_capacity(32);
    for byte in id.0 {
        use std::fmt::Write as _;
        write!(&mut text, "{byte:02x}").expect("writing to a string is infallible");
    }
    text
}

fn parse_id(text: &str) -> Result<DeviceId> {
    if text.len() != 32 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("a device ID must have 32 hexadecimal digits".into());
    }
    let mut bytes = [0; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * index..2 * index + 2], 16)?;
    }
    Ok(DeviceId(bytes))
}

fn private_key(bytes: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    let mut key = Zeroizing::new([0; 32]);
    if bytes.len() != key.len() {
        return Err("invalid stored Noise private key".into());
    }
    key.copy_from_slice(bytes);
    Ok(key)
}

#[cfg(unix)]
fn prepare_store(folder: &Path, path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    if let Err(error) = builder.create(folder) {
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    let metadata = fs::symlink_metadata(folder)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "sync directory must have mode 0700",
        ));
    }
    let opened = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path);
    if let Err(error) = opened {
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "sync database must have mode 0600",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn prepare_store(folder: &Path, _: &Path) -> io::Result<()> {
    fs::create_dir_all(folder)
}

#[cfg(not(windows))]
fn seal_identity(bytes: &mut [u8; 32]) -> Result<Zeroizing<Vec<u8>>> {
    Ok(Zeroizing::new(bytes.to_vec()))
}

#[cfg(not(windows))]
fn open_identity(sealed: Zeroizing<Vec<u8>>) -> Result<Zeroizing<[u8; 32]>> {
    private_key(&sealed)
}

#[cfg(windows)]
fn seal_identity(bytes: &mut [u8; 32]) -> Result<Zeroizing<Vec<u8>>> {
    dpapi::protect(bytes).map_err(Into::into)
}

#[cfg(windows)]
fn open_identity(mut sealed: Zeroizing<Vec<u8>>) -> Result<Zeroizing<[u8; 32]>> {
    private_key(&dpapi::unprotect(&mut sealed)?)
}

#[cfg(windows)]
mod dpapi {
    use std::{io, ptr, slice};

    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        },
    };
    use zeroize::{Zeroize, Zeroizing};

    const MAX_BLOB_BYTES: usize = 4096;

    pub fn protect(bytes: &mut [u8]) -> io::Result<Zeroizing<Vec<u8>>> {
        let input = input_blob(bytes)?;
        let mut output = Blob::default();
        // SAFETY: `input` borrows a valid buffer and `output` is writable.
        // Null optional arguments select the Windows profile of the current user.
        let ok = unsafe {
            CryptProtectData(
                &input,
                ptr::null(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output.0,
            )
        };
        output.result(ok)
    }

    pub fn unprotect(bytes: &mut [u8]) -> io::Result<Zeroizing<Vec<u8>>> {
        let input = input_blob(bytes)?;
        let mut output = Blob::default();
        // SAFETY: same buffer validity as in protect. No optional description is requested.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output.0,
            )
        };
        output.result(ok)
    }

    fn input_blob(bytes: &mut [u8]) -> io::Result<CRYPT_INTEGER_BLOB> {
        if bytes.is_empty() || bytes.len() > MAX_BLOB_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid DPAPI input size",
            ));
        }
        Ok(CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            pbData: bytes.as_mut_ptr(),
        })
    }

    struct Blob(CRYPT_INTEGER_BLOB);

    impl Default for Blob {
        fn default() -> Self {
            Self(CRYPT_INTEGER_BLOB {
                cbData: 0,
                pbData: ptr::null_mut(),
            })
        }
    }

    impl Blob {
        fn result(&self, ok: i32) -> io::Result<Zeroizing<Vec<u8>>> {
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let size = self.0.cbData as usize;
            if self.0.pbData.is_null() || size == 0 || size > MAX_BLOB_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid DPAPI output size",
                ));
            }
            // SAFETY: successful DPAPI call supplied a live allocation of this size.
            let bytes = unsafe { slice::from_raw_parts(self.0.pbData, size) };
            Ok(Zeroizing::new(bytes.to_vec()))
        }
    }

    impl Drop for Blob {
        fn drop(&mut self) {
            if !self.0.pbData.is_null() {
                // SAFETY: this is the unaliased DPAPI allocation, released exactly once.
                unsafe {
                    if self.0.cbData as usize <= MAX_BLOB_BYTES {
                        slice::from_raw_parts_mut(self.0.pbData, self.0.cbData as usize).zeroize();
                    }
                    LocalFree(self.0.pbData.cast());
                }
            }
        }
    }
}
