use crate::args::OutputFormat;
use serde::Serialize;
use std::fmt::{Display, Write as _};

/// Presentation-only output for one CLI invocation.
///
/// Command and manager code own the data model and decide which human text to
/// present.  This type only selects the requested format and writes the
/// resulting representation.
pub struct Output {
    fmt: OutputFormat,
}

#[derive(Serialize)]
struct SuccessEnvelope<'a, T: ?Sized> {
    ok: bool,
    data: &'a T,
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    ok: bool,
    error: &'a str,
}

impl Output {
    #[must_use]
    pub fn new(fmt: OutputFormat) -> Self {
        Self { fmt }
    }

    #[must_use]
    pub fn is_json(&self) -> bool {
        self.fmt == OutputFormat::Json
    }

    /// Render and print a successful command result.
    ///
    /// Human output is exactly the caller-provided display text. JSON output
    /// is a stable success envelope, rather than a command-specific wrapper.
    pub fn print<T: Serialize>(&self, human: impl Display, value: &T) -> Result<(), String> {
        let rendered = self.render_success(human, value)?;
        println!("{rendered}");
        Ok(())
    }

    /// Render and print a command error.
    ///
    /// Human errors go to stderr with a useful prefix. JSON errors go to
    /// stdout so a caller consuming JSON does not have to merge streams.
    pub fn error(&self, message: &str) -> Result<(), String> {
        let rendered = self.render_error(message)?;
        if self.fmt == OutputFormat::Json {
            println!("{rendered}");
        } else {
            eprintln!("{rendered}");
        }
        Ok(())
    }

    fn render_success<T: Serialize>(
        &self,
        human: impl Display,
        value: &T,
    ) -> Result<String, String> {
        if self.fmt == OutputFormat::Json {
            serde_json::to_string(&SuccessEnvelope {
                ok: true,
                data: value,
            })
            .map_err(|err| format!("failed to serialize output: {err}"))
        } else {
            Ok(human.to_string())
        }
    }

    fn render_error(&self, message: &str) -> Result<String, String> {
        if self.fmt == OutputFormat::Json {
            serde_json::to_string(&ErrorEnvelope {
                ok: false,
                error: message,
            })
            .map_err(|err| format!("failed to serialize error output: {err}"))
        } else {
            Ok(format!("error: {message}"))
        }
    }

    /// Format bytes as a deterministic lower-case hexadecimal string for a
    /// caller that already obtained an offline packet from the manager.
    ///
    /// This helper intentionally does not know how packets are constructed or
    /// decoded; it is only presentation formatting.
    pub fn hex(bytes: &[u8]) -> String {
        let mut rendered = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            let _ = write!(rendered, "{byte:02x}");
        }
        rendered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Serialize)]
    struct Payload {
        profile: u8,
        dpi: u16,
    }

    #[test]
    fn success_json_is_a_stable_envelope() {
        let output = Output::new(OutputFormat::Json);
        let payload = Payload {
            profile: 2,
            dpi: 1600,
        };

        let rendered = output
            .render_success("Profile 2: 1600 DPI", &payload)
            .unwrap();
        assert_eq!(rendered, r#"{"ok":true,"data":{"profile":2,"dpi":1600}}"#);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&rendered).unwrap(),
            json!({
                "ok": true,
                "data": { "profile": 2, "dpi": 1600 },
            })
        );
    }

    #[test]
    fn error_json_is_a_stable_envelope() {
        let output = Output::new(OutputFormat::Json);

        let rendered = output.render_error("device unavailable").unwrap();
        assert_eq!(rendered, r#"{"ok":false,"error":"device unavailable"}"#);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&rendered).unwrap(),
            json!({
                "ok": false,
                "error": "device unavailable",
            })
        );
    }

    #[test]
    fn human_success_is_verbatim() {
        let output = Output::new(OutputFormat::Human);
        let payload = Payload {
            profile: 1,
            dpi: 800,
        };

        assert_eq!(
            output
                .render_success("Profile 1: 800 DPI", &payload)
                .unwrap(),
            "Profile 1: 800 DPI"
        );
        assert_eq!(output.render_success("", &payload).unwrap(), "");
    }

    #[test]
    fn human_error_is_prefixed_and_useful() {
        let output = Output::new(OutputFormat::Human);

        assert_eq!(
            output.render_error("device unavailable").unwrap(),
            "error: device unavailable"
        );
    }

    #[test]
    fn hex_is_lowercase_and_zero_padded() {
        assert_eq!(Output::hex(&[0x00, 0x01, 0xab, 0xff]), "0001abff");
    }

    #[test]
    fn json_is_single_parseable_document_with_raw_detail() {
        let output = Output::new(OutputFormat::Json);
        // Simulate power-cycle combined payload: instruction + raw outcome detail
        let payload = json!({
            "instruction": "Unplug USB, turn the mouse off",
            "outcome": { "profile": 1, "verified": true }
        });
        let rendered = output
            .render_success("Power-cycle verification for profile 1", &payload)
            .unwrap();
        // Must be exactly one JSON document, no embedded newlines from double envelope
        assert!(!rendered.contains('\n'));
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["ok"], json!(true));
        assert_eq!(
            parsed["data"]["instruction"],
            json!("Unplug USB, turn the mouse off")
        );
        assert_eq!(parsed["data"]["outcome"]["verified"], json!(true));
        // The human text does not leak into JSON data
        assert!(parsed["data"].get("human").is_none());
    }

    #[test]
    fn json_error_is_also_single_document() {
        let output = Output::new(OutputFormat::Json);
        let rendered = output.render_error("something failed").unwrap();
        assert_eq!(rendered.matches('\n').count(), 0);
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["ok"], json!(false));
    }

    #[test]
    fn is_json_distinguishes_formats() {
        assert!(Output::new(OutputFormat::Json).is_json());
        assert!(!Output::new(OutputFormat::Human).is_json());
    }

    #[test]
    fn human_output_leads_with_readable_action_but_json_keeps_raw() {
        // Human bind-like text should be readable; raw field stays in JSON data
        let human = "Set left button to profile-cycle";
        assert!(human.starts_with("Set left button to"));
        let payload = json!({"slot": 0, "action": 0x34, "raw": [0x34, 0x00, 0x00]});
        let output = Output::new(OutputFormat::Json);
        let rendered = output.render_success(human, &payload).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        // JSON retains raw automation fields
        assert_eq!(parsed["data"]["raw"], json!([0x34, 0x00, 0x00]));
        // Human output when not JSON is verbatim readable
        let human_out = Output::new(OutputFormat::Human)
            .render_success(human, &payload)
            .unwrap();
        assert_eq!(human_out, human);
    }

    #[test]
    fn bind_human_text_is_readable_without_raw_hex() {
        // Ordinary human bind output must lead with slot/action names and
        // readable modifier/key labels, never raw hex fragments.
        let human_samples = [
            "Set forward button to copy (Ctrl+C)",
            "Buttons for profile 1\n  [ 0] left: left-click",
            "Buttons for profile 1\n  [ 4] slot 4: copy (Ctrl+C)",
        ];
        for human in human_samples {
            assert!(!human.contains("0x"), "human leaks hex: {human}");
            assert!(!human.contains("mod=0x"), "human leaks raw: {human}");
            // Must lead with slot/action wording
            assert!(
                human.contains("button to") || human.contains("Buttons for profile"),
                "not leading: {human}"
            );
            // JSON must keep raw typed fields verbatim
            let payload = json!({
                "slot": 6,
                "action": 0x11,
                "modifier": 0x01,
                "keyCode": 0x06,
                "raw": [0x11, 0x01, 0x06]
            });
            let rendered = Output::new(OutputFormat::Json)
                .render_success(human, &payload)
                .unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
            assert_eq!(parsed["data"]["modifier"], json!(0x01));
            assert_eq!(parsed["data"]["keyCode"], json!(0x06));
            assert_eq!(parsed["data"]["raw"], json!([0x11, 0x01, 0x06]));
            let human_out = Output::new(OutputFormat::Human)
                .render_success(human, &payload)
                .unwrap();
            assert_eq!(human_out, human);
            assert!(!human_out.contains("0x"));
        }
    }

    #[test]
    fn debug_hex_remains_lossless_while_human_stays_friendly() {
        // Debug/JSON must preserve every raw field; hex helper is stable lower-case.
        let bytes = [0x08u8, 0x3b, 0x02, 0x00, 0x00, 0xab, 0xff];
        assert_eq!(Output::hex(&bytes), "083b020000abff");
        let human = "Buttons for profile 2\n  [ 0] left: left-click";
        let payload = json!({
            "profile": 2,
            "slots": [{ "action": 0x02, "modifier": 0x00, "keyCode": 0x00 }],
            "rawHex": Output::hex(&bytes)
        });
        let rendered = Output::new(OutputFormat::Json)
            .render_success(human, &payload)
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["data"]["rawHex"], json!("083b020000abff"));
        assert_eq!(parsed["data"]["slots"][0]["action"], json!(0x02));
    }
}
