#[derive(Default)]
pub struct FrameBuffer {
    buf: Vec<u8>,
}

impl FrameBuffer {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(idx) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=idx).collect();
            line.pop(); // 去掉 \n
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            out.push(String::from_utf8_lossy(&line).into_owned());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_only_on_lf() {
        let mut fb = FrameBuffer::default();
        let lines = fb.push(b"{\"a\":1}\n{\"b\":2}\n");
        assert_eq!(lines, vec![r#"{"a":1}"#, r#"{"b":2}"#]);
    }

    #[test]
    fn strips_trailing_cr() {
        let mut fb = FrameBuffer::default();
        let lines = fb.push(b"hello\r\nworld\r\n");
        assert_eq!(lines, vec!["hello", "world"]);
    }

    #[test]
    fn buffers_partial_line_across_pushes() {
        let mut fb = FrameBuffer::default();
        assert_eq!(fb.push(b"{\"a\":"), Vec::<String>::new());
        assert_eq!(fb.push(b"1}\n"), vec![r#"{"a":1}"#]);
    }

    #[test]
    fn does_not_split_on_unicode_separators() {
        // U+2028 (E2 80 A8) 在 JSON 字符串内合法，绝不能当作换行
        let mut fb = FrameBuffer::default();
        let input = "{\"t\":\"a\u{2028}b\"}\n".as_bytes();
        let lines = fb.push(input);
        assert_eq!(lines, vec!["{\"t\":\"a\u{2028}b\"}"]);
    }
}
