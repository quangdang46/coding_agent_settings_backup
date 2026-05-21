//! Output formatting and printing for CLI commands.
//!
//! Wraps the `--json`, `--format toon`, and default text modes behind a
//! [`Printer`] type that all command implementations share. Text output uses
//! basic ANSI styling but stays readable on terminals without colour.

use crate::cli::OutputFormat;
use crate::error::Result;
use crate::toon;
use serde::Serialize;
use std::io::{self, Write};

/// Centralised CLI output sink.
///
/// Carries the chosen format plus quiet/verbose flags and writes through a
/// boxed [`Write`] so tests can capture output.
pub struct Printer {
    format: OutputFormat,
    quiet: bool,
    verbose: bool,
    writer: Box<dyn Write + Send>,
}

impl Printer {
    /// Construct a new printer that writes to stdout.
    pub fn new(format: OutputFormat, quiet: bool, verbose: bool) -> Self {
        Self {
            format,
            quiet,
            verbose,
            writer: Box::new(io::stdout()),
        }
    }

    /// Construct a new printer that writes to an arbitrary sink.
    pub fn with_writer(
        format: OutputFormat,
        quiet: bool,
        verbose: bool,
        writer: Box<dyn Write + Send>,
    ) -> Self {
        Self {
            format,
            quiet,
            verbose,
            writer,
        }
    }

    /// Whether the configured format is JSON.
    pub fn is_json(&self) -> bool {
        matches!(self.format, OutputFormat::Json)
    }

    /// Whether the configured format is TOON.
    pub fn is_toon(&self) -> bool {
        matches!(self.format, OutputFormat::Toon)
    }

    /// Whether the configured format is plain text.
    pub fn is_text(&self) -> bool {
        matches!(self.format, OutputFormat::Text)
    }

    /// Whether quiet mode is enabled.
    pub fn quiet(&self) -> bool {
        self.quiet
    }

    /// Whether verbose mode is enabled.
    pub fn verbose(&self) -> bool {
        self.verbose
    }

    /// Print a heading line in text mode (suppressed in JSON / TOON / quiet).
    pub fn heading(&mut self, line: &str) -> Result<()> {
        if self.quiet || !self.is_text() {
            return Ok(());
        }
        writeln!(self.writer, "{line}")?;
        Ok(())
    }

    /// Print a plain text line. Suppressed in non-text formats.
    pub fn text_line(&mut self, line: &str) -> Result<()> {
        if !self.is_text() || self.quiet {
            return Ok(());
        }
        writeln!(self.writer, "{line}")?;
        Ok(())
    }

    /// Print a verbose-only line (only when `--verbose` and text mode).
    pub fn verbose_line(&mut self, line: &str) -> Result<()> {
        if !self.verbose || !self.is_text() || self.quiet {
            return Ok(());
        }
        writeln!(self.writer, "{line}")?;
        Ok(())
    }

    /// Print a single arbitrary line regardless of format. Used for
    /// generated content (export to stdout, completion scripts, etc.).
    pub fn raw(&mut self, line: &str) -> Result<()> {
        writeln!(self.writer, "{line}")?;
        Ok(())
    }

    /// Print arbitrary bytes regardless of format. Used for binary streams
    /// such as tar.gz output to stdout.
    pub fn raw_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        Ok(())
    }

    /// Emit the supplied serialisable value in the active format.
    ///
    /// For text mode the caller should call [`Printer::text_line`] or
    /// [`Printer::heading`] themselves; in that case the value is silently
    /// ignored. For JSON/TOON it is serialised to the writer.
    pub fn emit<T: Serialize>(&mut self, value: &T) -> Result<()> {
        match self.format {
            OutputFormat::Text => Ok(()),
            OutputFormat::Json => {
                let text = serde_json::to_string_pretty(value)?;
                writeln!(self.writer, "{text}")?;
                Ok(())
            }
            OutputFormat::Toon => {
                let json = serde_json::to_value(value)?;
                let toon_text = toon::to_toon(&json);
                writeln!(self.writer, "{toon_text}")?;
                Ok(())
            }
        }
    }

    /// Force-flush the underlying writer.
    pub fn flush(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    /// Writer adapter that captures output in an `Arc<Mutex<Vec<u8>>>` so
    /// tests can read what was written.
    #[derive(Clone)]
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn make_printer(format: OutputFormat) -> (Printer, Arc<Mutex<Vec<u8>>>) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer = Box::new(CaptureWriter(buf.clone()));
        let p = Printer::with_writer(format, false, false, writer);
        (p, buf)
    }

    #[test]
    fn text_heading_prints_line() {
        let (mut p, buf) = make_printer(OutputFormat::Text);
        p.heading("hello").unwrap();
        let s = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert_eq!(s, "hello\n");
    }

    #[test]
    fn json_emit_produces_valid_json() {
        let (mut p, buf) = make_printer(OutputFormat::Json);
        p.emit(&json!({"a":1,"b":"x"})).unwrap();
        let s = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
        assert_eq!(parsed["a"], 1);
        assert_eq!(parsed["b"], "x");
    }

    #[test]
    fn quiet_suppresses_text_lines() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer = Box::new(CaptureWriter(buf.clone()));
        let mut p = Printer::with_writer(OutputFormat::Text, true, false, writer);
        p.text_line("hidden").unwrap();
        assert!(buf.lock().unwrap().is_empty());
    }

    #[test]
    fn json_mode_does_not_print_text_lines() {
        let (mut p, buf) = make_printer(OutputFormat::Json);
        p.text_line("ignored").unwrap();
        assert!(buf.lock().unwrap().is_empty());
    }
}
