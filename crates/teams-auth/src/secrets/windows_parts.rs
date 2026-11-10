//! Credential Manager blobs are limited to 2,560 bytes. Store bounded pieces in the same native namespace and
//! commit their generation with one atomic root write. A journal makes interrupted staging recoverable without
//! enumeration. The caller holds the per-key cross-process lock for every operation. Legacy raw items still read.
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use super::super::SecretError;

pub(super) const PIECE: usize = 2_560;
const MAX_SECRET: usize = 4 * 1024 * 1024;
const MAGIC: &[u8] = b"\0ngteams-credential-parts-v2\0";
const RECORD: usize = 16 + 4 + 32 + 16 + 4;

type Result<T> = std::result::Result<T, SecretError>;

pub(super) trait Storage {
    fn read(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>>;
    fn write(&self, name: &str, bytes: &[u8]) -> Result<()>;
    fn remove(&self, name: &str) -> Result<()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Generation {
    id: [u8; 16],
    pieces: usize,
}

#[derive(Clone, Copy, Debug)]
struct Manifest {
    current: Generation,
    length: usize,
    digest: [u8; 32],
    previous: Option<Generation>,
}

fn invalid() -> SecretError {
    SecretError::WindowsKeyring("invalid or incomplete multipart credential")
}

impl Manifest {
    fn parse(bytes: &[u8]) -> Result<Option<Self>> {
        if !bytes.starts_with(MAGIC) {
            return Ok(None);
        }
        let b = &bytes[MAGIC.len()..];
        if b.len() != RECORD {
            return Err(invalid());
        }
        let length = u32::from_le_bytes(b[16..20].try_into().map_err(|_| invalid())?) as usize;
        let previous = u32::from_le_bytes(b[68..72].try_into().map_err(|_| invalid())?) as usize;
        if length > MAX_SECRET || previous > MAX_SECRET.div_ceil(PIECE) {
            return Err(invalid());
        }
        Ok(Some(Self {
            current: Generation {
                id: b[..16].try_into().map_err(|_| invalid())?,
                pieces: length.div_ceil(PIECE),
            },
            length,
            digest: b[20..52].try_into().map_err(|_| invalid())?,
            previous: (previous > 0).then(|| Generation {
                id: b[52..68].try_into().unwrap_or([0; 16]),
                pieces: previous,
            }),
        }))
    }

    fn encode(self) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&self.current.id);
        bytes.extend_from_slice(&u32::try_from(self.length).unwrap_or(u32::MAX).to_le_bytes());
        bytes.extend_from_slice(&self.digest);
        bytes.extend_from_slice(&self.previous.map_or([0; 16], |g| g.id));
        bytes.extend_from_slice(
            &u32::try_from(self.previous.map_or(0, |g| g.pieces))
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        bytes
    }
}

fn piece_name(root: &str, generation: Generation, index: usize) -> String {
    use std::fmt::Write as _;
    let mut name = format!("{root}:part:");
    for byte in generation.id {
        let _ = write!(&mut name, "{byte:02x}");
    }
    let _ = write!(&mut name, ":{index}");
    name
}

fn cleanup(store: &impl Storage, root: &str, generation: Generation) -> Result<()> {
    for index in 0..generation.pieces {
        store.remove(&piece_name(root, generation, index))?;
    }
    Ok(())
}

fn recover(store: &impl Storage, root: &str, current: Option<Manifest>) -> Result<()> {
    let journal = format!("{root}:pending");
    if let Some(bytes) = store.read(&journal)? {
        let pending = Manifest::parse(&bytes)?.ok_or_else(invalid)?;
        if !current.is_some_and(|m| m.current == pending.current) {
            cleanup(store, root, pending.current)?;
        }
        store.remove(&journal)?;
    }
    Ok(())
}

pub(super) fn read(store: &impl Storage, root: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
    let bytes = store.read(root)?;
    let manifest = bytes
        .as_deref()
        .map(|bytes| Manifest::parse(bytes))
        .transpose()?
        .flatten();
    recover(store, root, manifest)?;
    let Some(manifest) = manifest else {
        return Ok(bytes);
    };
    let mut value = Zeroizing::new(Vec::with_capacity(manifest.length));
    for index in 0..manifest.current.pieces {
        let bytes = store
            .read(&piece_name(root, manifest.current, index))?
            .ok_or_else(invalid)?;
        let expected = (manifest.length - value.len()).min(PIECE);
        if bytes.len() != expected {
            return Err(invalid());
        }
        value.extend_from_slice(&bytes);
    }
    if <[u8; 32]>::from(Sha256::digest(value.as_slice())) != manifest.digest {
        return Err(invalid());
    }
    Ok(Some(value))
}

pub(super) fn write(store: &impl Storage, root: &str, value: &[u8]) -> Result<()> {
    if value.len() > MAX_SECRET {
        return Err(SecretError::WindowsKeyring(
            "credential exceeds the bounded token record size",
        ));
    }
    let old = store
        .read(root)?
        .as_deref()
        .map(|bytes| Manifest::parse(bytes))
        .transpose()?
        .flatten();
    recover(store, root, old)?;
    if let Some(previous) = old.and_then(|m| m.previous) {
        cleanup(store, root, previous)?;
    }
    let mut id = [0; 16];
    getrandom::fill(&mut id).map_err(|_| SecretError::WindowsKeyring("credential generation failed"))?;
    let manifest = Manifest {
        current: Generation {
            id,
            pieces: value.len().div_ceil(PIECE),
        },
        length: value.len(),
        digest: Sha256::digest(value).into(),
        previous: old.map(|m| m.current),
    };
    let journal = format!("{root}:pending");
    store.write(&journal, &manifest.encode())?;
    // If staging or commit fails, keep the journal. Recovery removes only this uncommitted generation.
    for (index, bytes) in value.chunks(PIECE).enumerate() {
        store.write(&piece_name(root, manifest.current, index), bytes)?;
    }
    store.write(root, &manifest.encode())?;
    if let Some(previous) = manifest.previous {
        cleanup(store, root, previous)?;
    }
    store.remove(&journal)
}

pub(super) fn delete(store: &impl Storage, root: &str) -> Result<()> {
    let old = store
        .read(root)?
        .as_deref()
        .map(|bytes| Manifest::parse(bytes))
        .transpose()?
        .flatten();
    recover(store, root, old)?;
    if let Some(manifest) = old {
        if let Some(previous) = manifest.previous {
            cleanup(store, root, previous)?;
        }
        cleanup(store, root, manifest.current)?;
    }
    store.remove(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Memory {
        values: RefCell<BTreeMap<String, Vec<u8>>>,
        fail_after: Cell<Option<usize>>,
    }
    impl Memory {
        fn mutation(&self) -> Result<()> {
            if let Some(left) = self.fail_after.get() {
                if left == 0 {
                    return Err(SecretError::WindowsKeyring("synthetic storage interruption"));
                }
                self.fail_after.set(Some(left - 1));
            }
            Ok(())
        }
    }
    impl Storage for Memory {
        fn read(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
            Ok(self.values.borrow().get(name).cloned().map(Zeroizing::new))
        }
        fn write(&self, name: &str, bytes: &[u8]) -> Result<()> {
            assert!(bytes.len() <= PIECE);
            self.mutation()?;
            self.values.borrow_mut().insert(name.into(), bytes.into());
            Ok(())
        }
        fn remove(&self, name: &str) -> Result<()> {
            self.mutation()?;
            self.values.borrow_mut().remove(name);
            Ok(())
        }
    }

    #[test]
    fn every_interrupted_update_recovers_one_complete_value_and_deletes_all_owned_pieces() {
        for point in 0..20 {
            let store = Memory::default();
            let old = b"synthetic-old";
            write(&store, "root", old).unwrap();
            let new = "synthetic-new-".repeat(PIECE);
            store.fail_after.set(Some(point));
            let _outcome = write(&store, "root", new.as_bytes());
            store.fail_after.set(None);
            let actual = read(&store, "root").unwrap().unwrap();
            assert!(actual.as_slice() == old || actual.as_slice() == new.as_bytes());
            delete(&store, "root").unwrap();
            assert!(store.values.borrow().is_empty());
            delete(&store, "root").unwrap();
        }
    }

    #[test]
    fn legacy_items_empty_and_multibyte_values_roundtrip_without_partial_reads() {
        let store = Memory::default();
        store.write("root", b"synthetic-legacy").unwrap();
        assert_eq!(read(&store, "root").unwrap().unwrap().as_slice(), b"synthetic-legacy");
        for value in [String::new(), "s".repeat(PIECE), "é".repeat(10_000)] {
            write(&store, "root", value.as_bytes()).unwrap();
            assert_eq!(read(&store, "root").unwrap().unwrap().as_slice(), value.as_bytes());
        }
        let manifest = Manifest::parse(store.read("root").unwrap().as_deref().unwrap())
            .unwrap()
            .unwrap();
        store
            .write(&piece_name("root", manifest.current, 0), &[0; PIECE])
            .unwrap();
        assert!(read(&store, "root").is_err());
        store.remove(&piece_name("root", manifest.current, 0)).unwrap();
        assert!(read(&store, "root").is_err());
        delete(&store, "root").unwrap();
        assert!(store.values.borrow().is_empty());
        assert!(Manifest::parse(MAGIC).is_err());
        let mut malformed = manifest;
        malformed.length = MAX_SECRET + 1;
        assert!(Manifest::parse(&malformed.encode()).is_err());
        assert!(write(&store, "root", &vec![0; MAX_SECRET + 1]).is_err());
    }
}
