use snow::params::{CipherChoice, DHChoice, HashChoice};
use snow::resolvers::{CryptoResolver, DefaultResolver};
use snow::types::{Cipher, Dh, Hash, Random};
use snow::{Builder, Error};
use subtle::ConstantTimeEq;

pub(crate) fn builder(params: &str) -> Result<Builder<'_>, Error> {
    Ok(Builder::with_resolver(
        params.parse()?,
        Box::new(CheckedResolver),
    ))
}

struct CheckedResolver;

impl CryptoResolver for CheckedResolver {
    fn resolve_rng(&self) -> Option<Box<dyn Random>> {
        DefaultResolver.resolve_rng()
    }

    fn resolve_dh(&self, choice: &DHChoice) -> Option<Box<dyn Dh>> {
        let dh = DefaultResolver.resolve_dh(choice)?;
        Some(Box::new(CheckedDh(dh)))
    }

    fn resolve_hash(&self, choice: &HashChoice) -> Option<Box<dyn Hash>> {
        DefaultResolver.resolve_hash(choice)
    }

    fn resolve_cipher(&self, choice: &CipherChoice) -> Option<Box<dyn Cipher>> {
        DefaultResolver.resolve_cipher(choice)
    }
}

struct CheckedDh(Box<dyn Dh>);

impl Dh for CheckedDh {
    fn name(&self) -> &'static str {
        self.0.name()
    }

    fn pub_len(&self) -> usize {
        self.0.pub_len()
    }

    fn priv_len(&self) -> usize {
        self.0.priv_len()
    }

    fn set(&mut self, private_key: &[u8]) {
        self.0.set(private_key);
    }

    fn generate(&mut self, rng: &mut dyn Random) -> Result<(), Error> {
        self.0.generate(rng)
    }

    fn pubkey(&self) -> &[u8] {
        self.0.pubkey()
    }

    fn privkey(&self) -> &[u8] {
        self.0.privkey()
    }

    fn dh(&self, public_key: &[u8], output: &mut [u8]) -> Result<(), Error> {
        let secret = output.get_mut(..32).ok_or(Error::Dh)?;
        self.0.dh(public_key, secret)?;
        // Reject low-order inputs following RFC 7748's constant-time all-zero
        // check. Snow 0.10's default X25519 resolver leaves this to callers.
        if bool::from(secret.ct_eq(&[0; 32])) {
            return Err(Error::Dh);
        }
        Ok(())
    }
}
