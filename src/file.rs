use std::fs::OpenOptions;
use std::io::{self, Write};

pub fn write_to_file(name: String, content: String) -> io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(name)?;

    file.write_all(content.as_bytes())?;

    Ok(())
}
