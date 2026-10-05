use super::super::final_output::read_final_bytes;

#[test]
fn final_read_error_is_reported() {
    struct Broken;
    impl std::io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("injected file read failure"))
        }
    }
    let error = read_final_bytes(Broken, 0, 100).unwrap_err();
    assert!(format!("{error:#}").contains("Failed to read Codex final output"));
}

#[test]
fn growing_file_read_is_capped_at_limit_plus_one() {
    use std::{cell::Cell, io::Read, rc::Rc};
    struct Growing(Rc<Cell<usize>>);
    impl Read for Growing {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            buffer.fill(b'x');
            self.0.set(self.0.get() + buffer.len());
            Ok(buffer.len())
        }
    }
    let consumed = Rc::new(Cell::new(0));
    let error = read_final_bytes(Growing(consumed.clone()), 0, 32).unwrap_err();
    assert_eq!(consumed.get(), 33);
    assert!(error.to_string().contains("exceeds 32-byte limit"));
}

#[test]
fn final_bytes_preserve_empty_unicode_and_exact_boundary() {
    for bytes in ["", " \nλ\t", "01234567"] {
        assert_eq!(read_final_bytes(bytes.as_bytes(), 0, 8).unwrap(), bytes);
    }
}
