//! Case-insensitive Windows INI sections/keys with owned writable values.
#[derive(Default)]
pub struct Profile {
    pub sections: Vec<(Vec<u8>, Vec<(Vec<u8>, Vec<u8>)>)>,
}
fn trim(mut s: &[u8]) -> &[u8] {
    while s.first().is_some_and(u8::is_ascii_whitespace) {
        s = &s[1..];
    }
    while s.last().is_some_and(u8::is_ascii_whitespace) {
        s = &s[..s.len() - 1];
    }
    s
}
impl Profile {
    pub fn parse(bytes: &[u8]) -> Self {
        let mut result = Self::default();
        let mut current = None;
        for line in bytes.split(|b| *b == b'\n') {
            let line = trim(line);
            if line.is_empty() || matches!(line[0], b';' | b'#') {
                continue;
            }
            if line[0] == b'[' {
                if let Some(end) = line.iter().position(|b| *b == b']') {
                    let name = trim(&line[1..end]);
                    let index = result
                        .sections
                        .iter()
                        .position(|(key, _)| key.eq_ignore_ascii_case(name))
                        .unwrap_or_else(|| {
                            result.sections.push((name.to_vec(), Vec::new()));
                            result.sections.len() - 1
                        });
                    current = Some(index);
                }
            } else if let (Some(section), Some(equal)) =
                (current, line.iter().position(|b| *b == b'='))
            {
                let key = trim(&line[..equal]);
                let mut value = trim(&line[equal + 1..]);
                if value.len() >= 2
                    && matches!(value[0], b'\'' | b'"')
                    && value.last() == value.first()
                {
                    value = &value[1..value.len() - 1];
                }
                if !result.sections[section]
                    .1
                    .iter()
                    .any(|(existing, _)| existing.eq_ignore_ascii_case(key))
                {
                    result.sections[section]
                        .1
                        .push((key.to_vec(), value.to_vec()));
                }
            }
        }
        result
    }
    pub fn keys(&self, section: &[u8]) -> Option<&[(Vec<u8>, Vec<u8>)]> {
        self.sections
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(section))
            .map(|(_, items)| items.as_slice())
    }
    pub fn value(&self, section: &[u8], key: &[u8]) -> Option<&[u8]> {
        self.keys(section)?
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.as_slice())
    }
    pub fn set(&mut self, section: &[u8], key: Option<&[u8]>, value: Option<&[u8]>) {
        let index = self
            .sections
            .iter()
            .position(|(name, _)| name.eq_ignore_ascii_case(section));
        if key.is_none() {
            if let Some(index) = index {
                self.sections.remove(index);
            }
            return;
        }
        let key = key.unwrap();
        let index = match index {
            Some(i) => i,
            None => {
                if value.is_none() {
                    return;
                }
                self.sections.push((section.to_vec(), Vec::new()));
                self.sections.len() - 1
            }
        };
        let entries = &mut self.sections[index].1;
        if let Some(index) = entries
            .iter()
            .position(|(name, _)| name.eq_ignore_ascii_case(key))
        {
            if let Some(value) = value {
                entries[index].1 = value.to_vec();
            } else {
                entries.remove(index);
            }
        } else if let Some(value) = value {
            entries.push((key.to_vec(), value.to_vec()));
        }
    }
    pub fn serialize(&self) -> Vec<u8> {
        let mut result = Vec::new();
        for (section, items) in &self.sections {
            result.push(b'[');
            result.extend_from_slice(section);
            result.extend_from_slice(b"]\r\n");
            for (key, value) in items {
                result.extend_from_slice(key);
                result.push(b'=');
                result.extend_from_slice(value);
                result.extend_from_slice(b"\r\n");
            }
        }
        result
    }
}
pub fn path(path: &[u8]) -> String {
    let path = String::from_utf8_lossy(path)
        .replace('\\', "/")
        .to_ascii_lowercase();
    let path = path.strip_prefix("c:/").unwrap_or(&path);
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    if parts.len() == 1 {
        parts.insert(0, "castle");
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_reads_updates_deletes_and_roundtrips_case_insensitive_values() {
        let mut p = Profile::parse(b"; hello\r\n[Castle]\r\nPlayerName=\"John\"\r\nFlag=1\r\n");
        assert_eq!(p.value(b"castle", b"PLAYERNAME"), Some(b"John".as_slice()));
        p.set(b"CASTLE", Some(b"Flag"), Some(b"0"));
        p.set(b"Castle", Some(b"SaveFile"), Some(b"test.sav"));
        p.set(b"Castle", Some(b"PlayerName"), None);
        let p = Profile::parse(&p.serialize());
        assert_eq!(p.value(b"castle", b"flag"), Some(b"0".as_slice()));
        assert_eq!(
            p.value(b"castle", b"savefile"),
            Some(b"test.sav".as_slice())
        );
        assert_eq!(p.value(b"castle", b"PlayerName"), None);
        assert_eq!(path(b"C:\\CASTLE\\CASTLE.INI"), path(b"castle.ini"));
    }
}
