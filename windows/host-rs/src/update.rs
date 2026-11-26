use std::ffi::OsString;
use std::io;
use std::ptr::{null, null_mut};

use base64::{engine::general_purpose::STANDARD, Engine};
use windows_sys::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptCreateHash, BCryptDestroyHash, BCryptDestroyKey,
    BCryptFinishHash, BCryptHash, BCryptHashData, BCryptImportKeyPair,
    BCryptOpenAlgorithmProvider, BCryptVerifySignature, BCRYPT_ALG_HANDLE, BCRYPT_ECCPUBLIC_BLOB,
    BCRYPT_ECDSA_P256_ALGORITHM, BCRYPT_HASH_HANDLE, BCRYPT_KEY_HANDLE, BCRYPT_SHA256_ALGORITHM,
};

fn status(code: i32, context: &'static str) -> io::Result<()> {
    if code == 0 {
        return Ok(());
    }
    Err(io::Error::other(context))
}

struct Algorithm(BCRYPT_ALG_HANDLE);

impl Algorithm {
    fn open(name: *const u16) -> io::Result<Self> {
        let mut handle = null_mut();
        // Algorithm names are the static, nul-terminated Windows CNG constants.
        let code = unsafe { BCryptOpenAlgorithmProvider(&mut handle, name, null(), 0) };
        status(code, "Could not open signature provider")?;
        Ok(Self(handle))
    }
}

impl Drop for Algorithm {
    fn drop(&mut self) {
        // The handle was returned by the provider and is owned by this wrapper.
        unsafe { BCryptCloseAlgorithmProvider(self.0, 0) };
    }
}

struct Key(BCRYPT_KEY_HANDLE);

impl Drop for Key {
    fn drop(&mut self) {
        // The key handle was imported by CNG and is owned by this wrapper.
        unsafe { BCryptDestroyKey(self.0) };
    }
}

pub(crate) fn verify(data: &[u8], signature: &[u8], public_key: &[u8]) -> io::Result<()> {
    if signature.len() != 64 || public_key.len() != 72 || data.len() > 8192 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid update signature size",
        ));
    }
    let hash = Algorithm::open(BCRYPT_SHA256_ALGORITHM)?;
    let mut digest = [0u8; 32];
    // The input and output buffers remain valid for the call, and no HMAC key is used.
    let code = unsafe {
        BCryptHash(
            hash.0,
            null(),
            0,
            data.as_ptr(),
            data.len() as u32,
            digest.as_mut_ptr(),
            digest.len() as u32,
        )
    };
    status(code, "Could not hash update identity")?;

    let algorithm = Algorithm::open(BCRYPT_ECDSA_P256_ALGORITHM)?;
    let mut key = null_mut();
    // The CNG ECCPUBLICBLOB buffer belongs to the caller for the duration of the import.
    let code = unsafe {
        BCryptImportKeyPair(
            algorithm.0,
            null_mut(),
            BCRYPT_ECCPUBLIC_BLOB,
            &mut key,
            public_key.as_ptr(),
            public_key.len() as u32,
            0,
        )
    };
    status(code, "Invalid update verification key")?;
    let key = Key(key);
    // ECDSA uses no external padding info. Digest and signature are fixed-size slices.
    let code = unsafe {
        BCryptVerifySignature(
            key.0,
            null(),
            digest.as_ptr(),
            digest.len() as u32,
            signature.as_ptr(),
            signature.len() as u32,
            0,
        )
    };
    status(code, "Update signature mismatch")
}

pub(crate) struct Sha256 {
    hash: BCRYPT_HASH_HANDLE,
    _algorithm: Algorithm,
}

impl Sha256 {
    pub(crate) fn open() -> io::Result<Self> {
        let algorithm = Algorithm::open(BCRYPT_SHA256_ALGORITHM)?;
        let mut hash = null_mut();
        let code = unsafe { BCryptCreateHash(algorithm.0, &mut hash, null_mut(), 0, null(), 0, 0) };
        status(code, "Could not create update hash")?;
        Ok(Self { hash, _algorithm: algorithm })
    }

    pub(crate) fn update(&mut self, block: &[u8]) -> io::Result<()> {
        status(unsafe { BCryptHashData(self.hash, block.as_ptr(), block.len() as u32, 0) },
               "Could not hash update bytes")
    }

    pub(crate) fn finish(self) -> io::Result<[u8; 32]> {
        let mut digest = [0u8; 32];
        status(unsafe { BCryptFinishHash(self.hash, digest.as_mut_ptr(), digest.len() as u32, 0) },
               "Could not finish update hash")?;
        Ok(digest)
    }
}

impl Drop for Sha256 {
    fn drop(&mut self) {
        unsafe { BCryptDestroyHash(self.hash) };
    }
}

pub fn run(args: &[OsString]) -> io::Result<()> {
    let [key, data, signature] = args else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Missing update verification arguments",
        ));
    };
    let decode = |arg: &OsString| -> io::Result<Vec<u8>> {
        STANDARD
            .decode(arg.to_str().unwrap_or_default())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    };
    verify(&decode(data)?, &decode(signature)?, &decode(key)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_historical_official_update_and_rejects_tampering() {
        let key = STANDARD.decode("RUNTMSAAAAAnoMGiSMDUsphBHF2T4rQMEebXo78VFya7qAvcbS2Xv8SI+nVFwnCyk9rBJWrF+a0AkCNV7SjymkY8kGnmBZJ7").unwrap();
        let signature = STANDARD.decode("Owlkc2kTdwAmmij6niAa4O59y8smF109wS+WzQ1Vo34Mzt5Lt/w0GvufpnDtwjrywhIgFHnAV2ak9Zz37e7kdg==").unwrap();
        let identity = b"Lummix Windows x64 update v1\nv1.0.7\n42737340\nd25f4c5000e99ab5a440f3db8bc957adb24b3a7b8aee68991fd2e6ab3bf0c907\n";
        verify(identity, &signature, &key).unwrap();
        let mut forged = *identity;
        forged[0] = b'X';
        assert!(verify(&forged, &signature, &key).is_err());
        assert!(verify(identity, &[0; 64], &key).is_err());
        assert!(verify(identity, &signature[..63], &key).is_err());
        assert!(verify(identity, &signature, &[0; 72]).is_err());
    }
}
