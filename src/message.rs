pub(crate) enum Message {
    String(String),
    Bytes(Vec<u8>),
}
impl Message {
    pub(crate) fn from_string(text: String) -> Self {
        Self::String(text)
    }
    pub(crate) fn from_bytes(data: Vec<u8>) -> Self {
        Self::Bytes(data)
    }
    pub(crate) fn as_string(&self) -> Option<String> {
        match self {
            Self::String(mut text) => Some(text),
            Self::Bytes(mut bin) => {
                // Convert binary to text
                String::from_utf8(bin).ok()
            },
        }
    }
    pub(crate) fn as_bytes(&self) -> Option<Vec<u8>> {
        match self {
            Self::String(mut text) => {
                // Convert text to binary
                Some(text.into_bytes())
            },
            Self::Bytes(mut bin) => Some(bin),
        }
    }
}