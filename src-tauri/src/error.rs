use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    Message(String),
    #[error("Error de archivos: {0}")]
    Io(#[from] std::io::Error),
    #[error("Error de base de datos: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("Datos inválidos: {0}")]
    Json(#[from] serde_json::Error),
}
pub type Result<T> = std::result::Result<T, AppError>;
impl serde::Serialize for AppError {
    fn serialize<S>(&self, s: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        s.serialize_str(&self.to_string())
    }
}
