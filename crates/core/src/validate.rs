//! Validation of user input that several services share.

use crate::{Error, Result};

/// Maximum length of a file or folder name, in bytes (common file system limit).
pub const MAX_NAME_BYTES: usize = 255;

/// Checks that `name` is a valid file or folder name and returns it unchanged.
/// Names are taken literally: `" a"` and `"a"` are different names.
///
/// Rejected: empty names, `.` and `..`, names longer than [`MAX_NAME_BYTES`],
/// and names containing `/`, `\` or control characters (including NUL).
pub fn file_name(name: &str) -> Result<&str> {
    let invalid = |reason: &str| Err(Error::bad_request("INVALID_NAME", reason.to_string()));

    if name.is_empty() {
        return invalid("Name must not be empty");
    }
    if name == "." || name == ".." {
        return invalid("Name must not be '.' or '..'");
    }
    if name.len() > MAX_NAME_BYTES {
        return invalid("Name is too long");
    }
    if name.chars().any(|c| c == '/' || c == '\\' || c.is_control()) {
        return invalid("Name must not contain '/', '\\' or control characters");
    }
    if name.chars().any(is_bidi_control) {
        return invalid("Name must not contain characters that change the direction of text");
    }
    Ok(name)
}

/// Characters that reorder how text is shown, so `invoice\u{202E}fdp.exe`
/// would look like `invoiceexe.pdf`. (Other invisible format characters,
/// such as the joiner inside emoji, stay allowed.)
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Checks a username and returns it lower-cased.
///
/// Usernames are 3–32 characters of `a-z`, `0-9`, `.`, `_` and `-`, and start
/// with a letter or digit. Upper-case input is accepted and folded, so `Alice`
/// and `alice` are the same account.
pub fn username(name: &str) -> Result<String> {
    let name = name.trim().to_ascii_lowercase();
    let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-');

    let valid = (3..=32).contains(&name.len())
        && name.chars().all(allowed)
        && name.starts_with(|c: char| c.is_ascii_alphanumeric());
    if !valid {
        return Err(Error::bad_request(
            "INVALID_USERNAME",
            "Username must be 3-32 characters: letters, digits, '.', '_' or '-', starting with a letter or digit",
        ));
    }
    Ok(name)
}

/// Minimum password length in characters.
pub const MIN_PASSWORD_CHARS: usize = 8;

/// Checks that a password is long enough (and not absurdly long, since hashing
/// cost grows with length).
pub fn password(password: &str) -> Result<()> {
    let chars = password.chars().count();
    if chars < MIN_PASSWORD_CHARS {
        return Err(Error::bad_request(
            "WEAK_PASSWORD",
            format!("Password must be at least {MIN_PASSWORD_CHARS} characters"),
        ));
    }
    if chars > 1024 {
        return Err(Error::bad_request("WEAK_PASSWORD", "Password is too long"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_file_names() {
        for name in ["report.pdf", "Фото 2024", " leading space", "a", "emoji 🎉.txt", "family 👨\u{200D}👩\u{200D}👧"]
        {
            assert_eq!(file_name(name).unwrap(), name);
        }
    }

    #[test]
    fn rejects_dangerous_file_names() {
        let too_long = "x".repeat(MAX_NAME_BYTES + 1);
        let reversed = "invoice\u{202E}fdp.exe";
        for name in ["", ".", "..", "a/b", "a\\b", "nul\0byte", "tab\tname", reversed, "x\u{2067}y", too_long.as_str()]
        {
            let err = file_name(name).unwrap_err();
            assert_eq!(err.code, "INVALID_NAME", "{name:?} should be rejected");
        }
    }

    #[test]
    fn usernames_are_folded_to_lower_case() {
        assert_eq!(username("Alice").unwrap(), "alice");
        assert_eq!(username(" bob.smith-2 ").unwrap(), "bob.smith-2");
    }

    #[test]
    fn rejects_bad_usernames() {
        for name in ["ab", "-alice", "al ice", "алиса", "a@b", &"x".repeat(33)] {
            assert!(username(name).is_err(), "{name:?} should be rejected");
        }
    }

    #[test]
    fn password_length_is_checked_in_characters() {
        assert!(password("short").is_err());
        assert!(password("пароль12").is_ok()); // 8 chars, 14 bytes
        assert!(password(&"x".repeat(1025)).is_err());
    }
}
