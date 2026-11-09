//! The ini files REAPER writes (`reaper-mouse.ini`, `reaper-menu.ini`, …): sections of
//! `key=value` lines kept in their order, a blank line after each section.

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Section {
    pub name: String,
    pub entries: Vec<(String, String)>,
}

impl Section {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_owned(), entries: Vec::new() }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn set(&mut self, key: &str, value: &str) {
        match self.entries.iter_mut().find(|(k, _)| k == key) {
            Some(e) => e.1 = value.to_owned(),
            None => self.entries.push((key.to_owned(), value.to_owned())),
        }
    }

    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(k, _)| k != key);
        self.entries.len() != before
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ini {
    pub sections: Vec<Section>,
}

impl Ini {
    pub fn parse(text: &str) -> Self {
        let mut sections: Vec<Section> = Vec::new();
        for line in text.lines().map(|l| l.trim_end_matches('\r')) {
            if let Some(name) = line.trim().strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                sections.push(Section::new(name));
            } else if let Some((k, v)) = line.split_once('=')
                && let Some(s) = sections.last_mut()
            {
                s.entries.push((k.to_owned(), v.to_owned()));
            }
        }
        Self { sections }
    }

    pub fn write(&self) -> String {
        let mut out = String::new();
        for s in &self.sections {
            out.push_str(&format!("[{}]\n", s.name));
            for (k, v) in &s.entries {
                out.push_str(&format!("{k}={v}\n"));
            }
            out.push('\n');
        }
        out
    }

    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.name.eq_ignore_ascii_case(name))
    }

    /// The section `name`, added at the end when missing.
    pub fn section_mut(&mut self, name: &str) -> &mut Section {
        match self.sections.iter().position(|s| s.name.eq_ignore_ascii_case(name)) {
            Some(i) => &mut self.sections[i],
            None => {
                self.sections.push(Section::new(name));
                self.sections.last_mut().unwrap()
            }
        }
    }

    pub fn remove_section(&mut self, name: &str) -> bool {
        let before = self.sections.len();
        self.sections.retain(|s| !s.name.eq_ignore_ascii_case(name));
        self.sections.len() != before
    }
}
