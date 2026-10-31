use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IniError {
    #[error("line {line}: section header is missing its closing `]`")]
    UnclosedSection { line: usize },
    #[error("line {line}: empty section name")]
    EmptySectionName { line: usize },
    #[error("line {line}: section [{name}] is already defined on line {previous}")]
    DuplicateSection { line: usize, name: String, previous: usize },
    #[error("line {line}: expected `key=value`, found `{content}`")]
    NotAKeyValuePair { line: usize, content: String },
    #[error("line {line}: missing key before `=`")]
    EmptyKey { line: usize },
    #[error("line {line}: `{key}` is not inside a [section]")]
    KeyOutsideSection { line: usize, key: String },
    #[error("line {line}: `{key}` is already defined in section [{section}] on line {previous}")]
    DuplicateKey {
        line: usize,
        key: String,
        section: String,
        previous: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Always lowercase: keys are case-insensitive.
    pub key: String,
    pub value: String,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub line: usize,
    pub entries: Vec<Entry>,
}

impl Section {
    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.key.eq_ignore_ascii_case(key))
    }
}

/// INI subset accepted by Python's `configparser` with `=` as the only delimiter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ini {
    pub sections: Vec<Section>,
}

impl Ini {
    pub fn parse(text: &str) -> Result<Self, IniError> {
        let mut ini = Self::default();

        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let content = raw.trim();

            if content.is_empty() || content.starts_with('#') || content.starts_with(';') {
                continue;
            }

            if let Some(header) = content.strip_prefix('[') {
                let name = header
                    .strip_suffix(']')
                    .ok_or(IniError::UnclosedSection { line })?
                    .trim();
                if name.is_empty() {
                    return Err(IniError::EmptySectionName { line });
                }
                if let Some(previous) = ini.section(name) {
                    return Err(IniError::DuplicateSection {
                        line,
                        name: name.to_owned(),
                        previous: previous.line,
                    });
                }
                ini.sections.push(Section {
                    name: name.to_owned(),
                    line,
                    entries: Vec::new(),
                });
                continue;
            }

            let (key, value) = content.split_once('=').ok_or_else(|| IniError::NotAKeyValuePair {
                line,
                content: content.to_owned(),
            })?;
            let key = key.trim().to_ascii_lowercase();
            if key.is_empty() {
                return Err(IniError::EmptyKey { line });
            }
            let section = ini
                .sections
                .last_mut()
                .ok_or_else(|| IniError::KeyOutsideSection { line, key: key.clone() })?;
            if let Some(previous) = section.get(&key) {
                return Err(IniError::DuplicateKey {
                    line,
                    previous: previous.line,
                    key,
                    section: section.name.clone(),
                });
            }
            section.entries.push(Entry {
                key,
                value: value.trim().to_owned(),
                line,
            });
        }

        Ok(ini)
    }

    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections
            .iter()
            .find(|section| section.name.eq_ignore_ascii_case(name))
    }
}

/// Encodings Notepad can produce: UTF-8 (with or without BOM), UTF-16 or ANSI.
pub fn decode_text(bytes: Vec<u8>) -> String {
    fn utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|&pair| unit(pair)).collect();
        String::from_utf16_lossy(&units)
    }

    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, u16::from_be_bytes);
    }
    let bytes = match bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        Some(rest) => rest.to_vec(),
        None => bytes,
    };
    match String::from_utf8(bytes) {
        Ok(text) => text,
        // Latin-1 fallback, close to Windows-1252.
        Err(error) => error.into_bytes().into_iter().map(char::from).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_comments_and_values() {
        let ini = Ini::parse(
            "# comment\n; other comment\n[settings]\nCache_Duration = 5 \n\n[MSI Afterburner]\nlocation=C:\\a b\\c.exe\n",
        )
        .unwrap();
        let settings = ini.section("SETTINGS").unwrap();
        assert_eq!(settings.get("cache_duration").unwrap().value, "5");
        assert_eq!(settings.get("cache_duration").unwrap().line, 4);
        assert_eq!(
            ini.section("msi afterburner").unwrap().get("location").unwrap().value,
            "C:\\a b\\c.exe"
        );
    }

    #[test]
    fn values_may_contain_equal_signs() {
        let ini = Ini::parse("[a]\nkey=x=y\n").unwrap();
        assert_eq!(ini.section("a").unwrap().get("key").unwrap().value, "x=y");
    }

    #[test]
    fn rejects_malformed_input_with_line_numbers() {
        assert_eq!(
            Ini::parse("key=1\n"),
            Err(IniError::KeyOutsideSection {
                line: 1,
                key: "key".into()
            })
        );
        assert_eq!(
            Ini::parse("[a]\nnot a pair\n"),
            Err(IniError::NotAKeyValuePair {
                line: 2,
                content: "not a pair".into()
            })
        );
        assert_eq!(Ini::parse("[a\n"), Err(IniError::UnclosedSection { line: 1 }));
        assert_eq!(
            Ini::parse("[a]\nk=1\nK=2\n"),
            Err(IniError::DuplicateKey {
                line: 3,
                key: "k".into(),
                section: "a".into(),
                previous: 2
            })
        );
        assert_eq!(
            Ini::parse("[a]\n[A]\n"),
            Err(IniError::DuplicateSection {
                line: 2,
                name: "A".into(),
                previous: 1
            })
        );
    }

    #[test]
    fn decodes_common_notepad_encodings() {
        assert_eq!(decode_text(b"\xEF\xBB\xBF[a]".to_vec()), "[a]");
        assert_eq!(decode_text(vec![0xFF, 0xFE, b'[', 0, b'a', 0, b']', 0]), "[a]");
        assert_eq!(decode_text(b"caf\xE9".to_vec()), "café");
    }
}
