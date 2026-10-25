//! Settings the resolver reads from its environment, filled in from the settings files when the process started without them.
//!
//! The boot script exports `~/.streams-env` (`~/.pstream-env` before the rename of 2026-10-12) and starts the resolver. A phone set up
//! before the rename can run the old and the new script side by side; only one resolver holds the port, and after an update either may
//! win. On 2026-10-25 the one that won had been started without the settings file, and subtitle search reported its key missing.
//! Reading both files here makes the settings the same whichever script started the process. Only unset or empty names are filled;
//! names are logged, values never.

use std::path::Path;

/// The settings files, newest name first.
const FILES: [&str; 2] = [".streams-env", ".pstream-env"];

pub fn fill_from_files() {
    let Some(home) = std::env::var_os("HOME") else { return };
    for name in FILES {
        let path = Path::new(&home).join(name);
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let mut filled = Vec::new();
        for (key, value) in parse(&text) {
            if std::env::var(&key).map(|v| v.is_empty()).unwrap_or(true) && !value.is_empty() {
                std::env::set_var(&key, value);
                filled.push(key);
            }
        }
        if !filled.is_empty() {
            println!("[settings] from ~/{name}: {}", filled.join(", "));
        }
    }
}

/// `export NAME='value'` and `NAME=value` lines of a shell settings file; comments and anything else are skipped.
fn parse(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line).trim();
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || key.chars().next()?.is_ascii_digit() {
                return None;
            }
            let value = value.trim();
            let value = value
                .strip_prefix('\'').and_then(|v| v.strip_suffix('\''))
                .or_else(|| value.strip_prefix('"').and_then(|v| v.strip_suffix('"')))
                .unwrap_or(value);
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn reads_shell_settings_lines() {
        let text = "# a comment\nexport SUBDL_API_KEY='abc123'\nDEPLOY_SECRET=\"s e\"\nexport EMPTY=''\nnot a setting\n1BAD=x\n";
        assert_eq!(parse(text), vec![
            ("SUBDL_API_KEY".to_string(), "abc123".to_string()),
            ("DEPLOY_SECRET".to_string(), "s e".to_string()),
            ("EMPTY".to_string(), String::new()),
        ]);
    }
}
