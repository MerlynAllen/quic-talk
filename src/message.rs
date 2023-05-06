#[derive(Debug, Clone)]
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
    // This do not consume the message
    pub(crate) fn as_string(&self) -> Option<String> {
        match self {
            Self::String(text) => Some(text.clone()),
            Self::Bytes(bin) => {
                // Convert binary to text
                String::from_utf8(bin.clone()).ok()
            },
        }
    }
    // This do not consume the message
    pub(crate) fn as_bytes(&self) -> Option<Vec<u8>> {
        match self {
            Self::String(text) => {
                // Convert text to binary
                Some(text.clone().into_bytes())
            },
            Self::Bytes(bin) => Some(bin.clone()),
        }
    }

}