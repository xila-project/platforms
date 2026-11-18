use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct Selector {
    pub role: Option<String>,
    pub name: Option<String>,
    pub text: Option<String>,
    pub button: Option<String>,
    value: Option<i64>,
    checked: Option<bool>,
    focused: Option<bool>,
    enabled: Option<bool>,
    layer: Option<String>,
}
impl Selector {
    pub fn parse(args: &mut impl Iterator<Item = String>) -> Result<Self, String> {
        let mut selector = Self::default();
        while let Some(flag) = args.next() {
            selector.set(&flag, args.next().ok_or("selector flag requires a value")?)?;
        }
        selector.validate()?;
        Ok(selector)
    }
    pub fn validate(&self) -> Result<(), String> {
        if [
            &self.role,
            &self.name,
            &self.text,
            &self.button,
            &self.layer,
        ]
        .iter()
        .any(|s| s.as_ref().is_some_and(|s| s.is_empty() || s.len() > 512))
        {
            return Err("selectors must contain 1..512 bytes".into());
        }
        if self.role.is_none()
            && self.name.is_none()
            && self.text.is_none()
            && self.button.is_none()
        {
            return Err("provide --role, --name, --text or --button".into());
        }
        Ok(())
    }
    pub fn set(&mut self, flag: &str, value: String) -> Result<(), String> {
        let duplicate = match flag {
            "--role" => self.role.is_some(),
            "--name" => self.name.is_some(),
            "--text" => self.text.is_some(),
            "--button" => self.button.is_some(),
            "--layer" => self.layer.is_some(),
            "--value" => self.value.is_some(),
            "--checked" => self.checked.is_some(),
            "--focused" => self.focused.is_some(),
            "--enabled" => self.enabled.is_some(),
            _ => false,
        };
        if duplicate {
            return Err(format!("duplicate selector flag {flag}"));
        }
        if flag == "--layer" && !matches!(value.as_str(), "screen" | "top" | "system") {
            return Err("layer must be screen, top or system".into());
        }
        match flag {
            "--role" => self.role = Some(value),
            "--name" => self.name = Some(value),
            "--text" => self.text = Some(value),
            "--button" => self.button = Some(value),
            "--layer" => self.layer = Some(value),
            "--value" => self.value = Some(value.parse().map_err(|_| "invalid value")?),
            "--checked" => {
                self.checked = Some(value.parse().map_err(|_| "expected true or false")?)
            }
            "--focused" => {
                self.focused = Some(value.parse().map_err(|_| "expected true or false")?)
            }
            "--enabled" => {
                self.enabled = Some(value.parse().map_err(|_| "expected true or false")?)
            }
            _ => return Err(format!("unsupported selector flag {flag}")),
        }
        Ok(())
    }
    pub fn click_request(self) -> Result<debug_protocol::Request, String> {
        if self.value.is_some()
            || self.checked.is_some()
            || self.focused.is_some()
            || self.enabled.is_some()
            || self.layer.is_some()
        {
            return Err("click supports only --role, --name, --text and --button".into());
        }
        Ok(debug_protocol::Request::Click {
            role: self.role,
            name: self.name,
            text: self.text,
            button: self.button,
        })
    }
    pub fn matches(&self, e: &Value, substring: bool) -> bool {
        if e["hidden"] == true
            || e["visible"] == false
            || (self.text.is_some() && e["redacted"] == true)
        {
            return false;
        }
        if self.role.as_ref().is_some_and(|r| {
            e["role"].as_str() != Some(r) && e["semantic_role"].as_str() != Some(r)
        }) || self
            .name
            .as_ref()
            .is_some_and(|n| e["name"].as_str() != Some(n) || e["name_truncated"] == true)
        {
            return false;
        }
        if !substring && self.text.is_some() && e["text_truncated"] == true {
            return false;
        }
        if self.text.as_ref().is_some_and(|t| {
            !e["text"]
                .as_str()
                .is_some_and(|s| if substring { s.contains(t) } else { s == t })
        }) {
            return false;
        }
        if self
            .layer
            .as_ref()
            .is_some_and(|l| e["layer"].as_str() != Some(l))
            || self.value.is_some_and(|v| e["value"].as_i64() != Some(v))
            || self
                .checked
                .is_some_and(|v| e["checked"].as_bool() != Some(v))
            || self
                .focused
                .is_some_and(|v| e["focused"].as_bool() != Some(v))
            || self
                .enabled
                .is_some_and(|v| e["disabled"].as_bool() != Some(!v))
        {
            return false;
        }
        if self.button.as_ref().is_some_and(|label| {
            !e["buttons"].as_array().is_some_and(|bs| {
                bs.iter().any(|b| {
                    b["text"].as_str() == Some(label)
                        && b["hidden"] != true
                        && b["disabled"] != true
                        && b["text_truncated"] != true
                })
            })
        }) {
            return false;
        }
        true
    }
}

pub(super) fn find(
    endpoint: Option<PathBuf>,
    mut args: impl Iterator<Item = String>,
    timeout_ms: u64,
) -> Result<bool, String> {
    let selector = Selector::parse(&mut args)?;
    let endpoint = endpoint
        .map(Ok)
        .unwrap_or_else(drivers_native::window_screen::control::transport::endpoint_path)?;
    let response = super::wait::query(
        &endpoint,
        Instant::now() + Duration::from_millis(timeout_ms),
    )
    .map_err(|e| e.to_string())?;
    if response["ok"] != true {
        println!("{response}");
        return Ok(false);
    }
    let result = &response["result"];
    let elements = result["elements"].as_array().ok_or("missing elements")?;
    let matches: Vec<_> = elements
        .iter()
        .filter(|e| selector.matches(e, false))
        .collect();
    let ok = matches.len() == 1 && result["truncated"] == false;
    println!(
        "{}",
        json!({"ok":ok,"result":{"matches":matches,"truncated":result["truncated"]},"error":if ok {Value::Null}else{json!({"code":if result["truncated"]==true{"tree_truncated"}else if matches.is_empty(){"target_not_found"}else{"ambiguous_target"}})}})
    );
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selector_flags_reject_duplicates_and_unknown_layers() {
        let mut selector = Selector::default();
        selector.set("--text", "A".into()).unwrap();
        assert!(selector.set("--text", "B".into()).is_err());
        assert!(selector.set("--layer", "invalid".into()).is_err());
    }
    #[test]
    fn named_state_waits_exclude_hidden_clipped_and_wrong_value() {
        let mut selector = Selector::default();
        selector.set("--role", "slider".into()).unwrap();
        selector.set("--name", "Volume".into()).unwrap();
        selector.set("--value", "42".into()).unwrap();
        let widget = json!({"semantic_role":"slider","name":"Volume","value":42,"visible":true});
        assert!(selector.matches(&widget, true));
        for field in ["hidden", "visible", "value"] {
            let mut e = widget.clone();
            e[field] = match field {
                "hidden" => json!(true),
                "visible" => json!(false),
                _ => json!(41),
            };
            assert!(!selector.matches(&e, true));
        }
        assert!(
            selector.click_request().is_err(),
            "click must not silently drop a value predicate"
        );
    }
    #[test]
    fn matrix_virtual_buttons_and_sensitive_names_can_be_found() {
        let mut selector = Selector::default();
        selector.set("--button", "7".into()).unwrap();
        assert!(selector.matches(
            &json!({"visible":true,"buttons":[{"text":"7","disabled":false}]}),
            false
        ));
        assert!(!selector.matches(
            &json!({"visible":true,"buttons":[{"text":"7","disabled":true}]}),
            false
        ));
        let mut password = Selector::default();
        password.set("--name", "Password".into()).unwrap();
        assert!(password.matches(
            &json!({"name":"Password","redacted":true,"visible":true}),
            false
        ));
    }
}
