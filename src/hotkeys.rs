use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shortcuts {
    pub screenshot: String,
    pub history: String,
    pub full: String,
    pub gif: String,
}
impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            screenshot: "Alt+Shift+S".into(),
            history: "Alt+Shift+V".into(),
            full: "Alt+Shift+F".into(),
            gif: "Alt+Shift+G".into(),
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct Binding {
    pub modifiers: u32,
    pub key: u32,
    pub label: String,
}

pub fn parse(text: &str) -> Result<Binding> {
    let mut modifiers = 0;
    let mut key = None;
    let mut key_label = String::new();
    for token in text.split('+').map(|s| s.trim().to_ascii_uppercase()) {
        let modifier = match token.as_str() {
            "ALT" | "OPTION" => 1,
            "CTRL" | "CONTROL" => 2,
            "SHIFT" => 4,
            "WIN" | "WINDOWS" | "CMD" | "COMMAND" | "META" | "SUPER" => 8,
            _ => 0,
        };
        if modifier != 0 {
            ensure!(modifiers & modifier == 0, "Repeated shortcut modifier");
            modifiers |= modifier;
            continue;
        }
        ensure!(key.is_none(), "Use one letter, number, or function key");
        let code = if token.len() == 1 && token.as_bytes()[0].is_ascii_alphanumeric() {
            token.as_bytes()[0] as u32
        } else if let Some(number) = token
            .strip_prefix('F')
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|n| (1..=24).contains(n))
        {
            0x70 + number - 1
        } else {
            bail!("Use a letter, number, or F1–F24 with Ctrl, Alt, or Win");
        };
        key = Some(code);
        key_label = token;
    }
    ensure!(
        modifiers & (1 | 2 | 8) != 0,
        "Include Ctrl, Alt, or Win in each shortcut"
    );
    let key = key.ok_or_else(|| anyhow::anyhow!("Choose a shortcut key"))?;
    let mut labels = Vec::new();
    for (flag, label) in [(2, "Ctrl"), (1, "Alt"), (4, "Shift"), (8, "Win")] {
        if modifiers & flag != 0 {
            labels.push(label.to_string());
        }
    }
    labels.push(key_label);
    Ok(Binding {
        modifiers,
        key,
        label: labels.join("+"),
    })
}
impl Shortcuts {
    pub fn bindings(&self) -> Result<Vec<Binding>> {
        let bindings = [&self.screenshot, &self.history, &self.full, &self.gif]
            .map(|s| parse(s))
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        for (index, binding) in bindings.iter().enumerate() {
            ensure!(
                !bindings[..index]
                    .iter()
                    .any(|other| other.key == binding.key && other.modifiers == binding.modifiers),
                "Each action needs a different shortcut"
            );
        }
        Ok(bindings)
    }
    pub fn normalized(&self) -> Result<Self> {
        let b = self.bindings()?;
        Ok(Self {
            screenshot: b[0].label.clone(),
            history: b[1].label.clone(),
            full: b[2].label.clone(),
            gif: b[3].label.clone(),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_custom_shortcuts_are_normalized_and_unique() {
        assert_eq!(
            Shortcuts::default()
                .bindings()
                .unwrap()
                .iter()
                .map(|b| b.key)
                .collect::<Vec<_>>(),
            vec![0x53, 0x56, 0x46, 0x47]
        );
        assert_eq!(
            parse(" shift + control + f12 ").unwrap(),
            Binding {
                modifiers: 6,
                key: 0x7b,
                label: "Ctrl+Shift+F12".into()
            }
        );
        assert_eq!(
            parse("Cmd+Option+9").unwrap(),
            Binding {
                modifiers: 9,
                key: 0x39,
                label: "Alt+Win+9".into()
            }
        );
        for text in [
            "S",
            "Shift+S",
            "Alt+",
            "Ctrl+Alt+X+Y",
            "Alt+F25",
            "Ctrl+Ctrl+V",
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
        let mut shortcuts = Shortcuts::default();
        shortcuts.history = "shift+alt+s".into();
        assert!(shortcuts.bindings().is_err());
    }
}
