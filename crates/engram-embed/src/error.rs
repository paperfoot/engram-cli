use thiserror::Error;

#[derive(Debug, Error)]
pub enum EmbedError {
    #[error("missing API key for {provider}")]
    MissingKey { provider: &'static str },

    #[error("HTTP error from {provider}: {source}")]
    Http {
        provider: &'static str,
        #[source]
        source: reqwest::Error,
    },

    #[error("API error from {provider}: {message}")]
    Api { provider: &'static str, message: String },

    #[error("rate limited by {provider}")]
    RateLimited { provider: &'static str },

    #[error("invalid response from {provider}: {message}")]
    InvalidResponse {
        provider: &'static str,
        message: String,
    },
}
