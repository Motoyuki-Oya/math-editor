#[derive(Clone, Debug)]
pub(crate) struct EditBuffer(Vec<String>);

impl EditBuffer {
    pub(crate) fn new(lines: Vec<String>) -> Self {
        Self(lines)
    }

    pub(crate) fn as_slice(&self) -> &[String] {
        &self.0
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    pub(crate) fn split_off(&mut self, at: usize) -> Self {
        Self(self.0.split_off(at))
    }
}
