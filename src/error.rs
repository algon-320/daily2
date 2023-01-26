use x11rb::errors::ReplyOrIdError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    X11(ReplyOrIdError),
}

impl<T: Into<ReplyOrIdError>> From<T> for Error {
    fn from(x: T) -> Error {
        Error::X11(Into::<ReplyOrIdError>::into(x))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
