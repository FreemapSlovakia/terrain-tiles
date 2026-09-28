use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("gdal: {0}")]
    Gdal(#[from] gdal::errors::GdalError),

    #[error("image: {0}")]
    Image(#[from] image::ImageError),

    #[error("jpeg: {0}")]
    Jpeg(#[from] jpeg_encoder::EncodingError),

    #[error("{0}")]
    Encode(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Config(String),

    #[error("{0}")]
    BadRequest(String),

    #[error("not found")]
    NotFound,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound => StatusCode::NOT_FOUND,
            _ => {
                eprintln!("{self}");

                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        (status, self.to_string()).into_response()
    }
}
