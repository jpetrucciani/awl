use std::io::Read;

use crate::error::Result;

/// Return `value`, or all of stdin when `value` is `-`.
pub fn stdin_or_bytes(value: &str) -> Result<Vec<u8>> {
    if value != "-" {
        return Ok(value.as_bytes().to_vec());
    }
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Return `value`, or stdin without its trailing newline when `value` is `-`.
pub fn stdin_or_string(value: &str) -> Result<String> {
    if value != "-" {
        return Ok(value.to_owned());
    }
    let mut out = String::new();
    std::io::stdin().read_to_string(&mut out)?;
    Ok(out.trim_end_matches(['\r', '\n']).to_owned())
}
