use crate::Error;

#[cfg(not(target_family = "wasm"))]
pub fn check_key(der: &[u8]) -> Result<(), Error> {
    graviola::signing::rsa::SigningKey::from_pkcs8_der(der)
        .map(drop)
        .map_err(|error| {
            Error::new(format!(
                "the private_key is not a PKCS#8 RSA key: {error:?}"
            ))
        })
}

#[cfg(target_family = "wasm")]
pub fn check_key(_der: &[u8]) -> Result<(), Error> {
    Err(Error::new(
        "service_account keys are signed natively and are not available in WASM; pass an access token instead",
    ))
}

#[cfg(not(target_family = "wasm"))]
pub fn rs256(der: &[u8], message: &[u8]) -> Result<Vec<u8>, Error> {
    let key = graviola::signing::rsa::SigningKey::from_pkcs8_der(der).map_err(|error| {
        Error::new(format!(
            "the private_key is not a PKCS#8 RSA key: {error:?}"
        ))
    })?;
    let mut signature = vec![0; key.modulus_len_bytes()];
    let length = key
        .sign_pkcs1_sha256(&mut signature, message)
        .map_err(|error| Error::new(format!("signing the JWT assertion failed: {error:?}")))?
        .len();
    signature.truncate(length);
    Ok(signature)
}

#[cfg(target_family = "wasm")]
pub fn rs256(der: &[u8], _message: &[u8]) -> Result<Vec<u8>, Error> {
    check_key(der).map(|()| Vec::new())
}
