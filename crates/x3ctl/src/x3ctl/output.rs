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
}
