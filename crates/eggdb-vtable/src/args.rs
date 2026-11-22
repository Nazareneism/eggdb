//! Parsing of `USING egg(...)` arguments.
//!
//! ```sql
//! egg('data.egg', 'users')                      -- read-only
//! egg('data.egg', 'users', 'mode=rw')           -- writable
//! egg('data.egg', 'users', 'passphrase=secret') -- encrypted
//! egg('data.egg', 'users', 'secret')            -- legacy positional passphrase
//! ```

use zeroize::Zeroizing;

use crate::error::VtableError;

/// Parsed virtual table arguments.
#[derive(Debug)]
pub struct EggArgs {
    /// Path to the `.egg` file.
    pub path: String,
    /// Table name inside the egg.
    pub table: String,
    /// Passphrase for encrypted eggs.
    pub passphrase: Option<Zeroizing<String>>,
    /// Whether INSERT/UPDATE/DELETE are allowed.
    pub writable: bool,
}

/// Parse the user arguments (rusqlite has already removed module, database,
/// and table names).
pub fn parse_args(args: &[&[u8]]) -> Result<EggArgs, VtableError> {
    if args.len() < 2 {
        return Err(VtableError::WrongArgCount(args.len()));
    }
    let mut parsed = EggArgs {
        path: arg_str(args[0], "file path")?.to_string(),
        table: arg_str(args[1], "table name")?.to_string(),
        passphrase: None,
        writable: false,
    };
    let mut mode_seen = false;
    for (i, raw) in args[2..].iter().enumerate() {
        let arg = arg_str(raw, "option")?;
        match arg.split_once('=') {
            Some((key, value)) if key.trim().eq_ignore_ascii_case("mode") => {
                if std::mem::replace(&mut mode_seen, true) {
                    return Err(option_err("mode given more than once"));
                }
                parsed.writable = parse_mode(value.trim())?;
            }
            Some((key, value)) if key.trim().eq_ignore_ascii_case("passphrase") => {
                set_passphrase(&mut parsed, value)?;
            }
            // A bare first option is the legacy positional passphrase.
            _ if i == 0 => set_passphrase(&mut parsed, arg)?,
            _ => {
                return Err(option_err(format!(
                    "unrecognized option {arg:?} (expected mode=ro|rw or passphrase=...)"
                )))
            }
        }
    }
    Ok(parsed)
}

fn parse_mode(value: &str) -> Result<bool, VtableError> {
    match value.to_ascii_lowercase().as_str() {
        "rw" => Ok(true),
        "ro" => Ok(false),
        other => Err(option_err(format!(
            "unknown mode {other:?} (expected ro or rw)"
        ))),
    }
}

fn set_passphrase(parsed: &mut EggArgs, value: &str) -> Result<(), VtableError> {
    if parsed.passphrase.is_some() {
        return Err(option_err("passphrase given more than once"));
    }
    parsed.passphrase = Some(Zeroizing::new(value.to_string()));
    Ok(())
}

fn arg_str<'a>(raw: &'a [u8], what: &str) -> Result<&'a str, VtableError> {
    std::str::from_utf8(raw)
        .map(|s| dequote(s.trim()))
        .map_err(|e| VtableError::Other(format!("invalid {what}: {e}")))
}

fn option_err(msg: impl Into<String>) -> VtableError {
    VtableError::Other(format!("egg vtable: {}", msg.into()))
}

/// Remove one level of surrounding single or double quotes.
#[must_use]
pub fn dequote(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<EggArgs, VtableError> {
        let bytes: Vec<&[u8]> = args.iter().map(|a| a.as_bytes()).collect();
        parse_args(&bytes)
    }

    #[test]
    fn path_and_table_default_to_read_only() {
        let a = parse(&["'/tmp/d.egg'", "'users'"]).unwrap();
        assert_eq!((a.path.as_str(), a.table.as_str()), ("/tmp/d.egg", "users"));
        assert!(!a.writable);
        assert!(a.passphrase.is_none());
    }

    #[test]
    fn named_options_in_any_order() {
        let a = parse(&["p", "t", "'passphrase=s3cret'", "'mode=RW'"]).unwrap();
        assert!(a.writable);
        assert_eq!(a.passphrase.as_deref().map(String::as_str), Some("s3cret"));
        let a = parse(&["p", "t", "mode = ro"]).unwrap();
        assert!(!a.writable);
    }

    #[test]
    fn legacy_positional_passphrase_still_works() {
        let a = parse(&["p", "t", "'hunter2'"]).unwrap();
        assert_eq!(a.passphrase.as_deref().map(String::as_str), Some("hunter2"));
        let a = parse(&["p", "t", "'pw'", "'mode=rw'"]).unwrap();
        assert!(a.writable);
    }

    #[test]
    fn bad_arguments_are_rejected() {
        assert!(matches!(parse(&["p"]), Err(VtableError::WrongArgCount(1))));
        assert!(parse(&["p", "t", "mode=write"]).is_err());
        assert!(parse(&["p", "t", "mode=rw", "mode=ro"]).is_err());
        assert!(parse(&["p", "t", "a", "b"]).is_err(), "second bare option");
        assert!(
            parse(&["p", "t", "x", "passphrase=y"]).is_err(),
            "two passphrases"
        );
    }
}
