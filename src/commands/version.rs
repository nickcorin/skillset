use std::io::{self, Write};

pub struct VersionPrinter<'a, W> {
    version: &'a str,
    writer: W,
}

impl<'a, W: Write> VersionPrinter<'a, W> {
    pub fn new(version: &'a str, writer: W) -> Self {
        Self { version, writer }
    }

    pub fn print(&mut self) -> io::Result<()> {
        writeln!(self.writer, "{}", self.version)
    }
}

#[cfg(test)]
mod tests {
    use super::VersionPrinter;

    #[test]
    fn prints_version_followed_by_newline() {
        let mut output = Vec::new();
        VersionPrinter::new("1.2.3", &mut output)
            .print()
            .expect("write to Vec<u8> cannot fail");

        assert_eq!(output, b"1.2.3\n");
    }
}
