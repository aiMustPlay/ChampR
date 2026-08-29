#[derive(Debug, Clone)]
pub enum LcuError {
    APIError(String),
}

impl From<reqwest::Error> for LcuError {
    fn from(error: reqwest::Error) -> LcuError {
        LcuError::APIError(error.to_string())
    }
}

impl From<anyhow::Error> for LcuError {
    fn from(error: anyhow::Error) -> LcuError {
        LcuError::APIError(error.to_string())
    }
}
