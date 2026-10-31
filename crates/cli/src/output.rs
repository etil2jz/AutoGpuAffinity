use std::fmt::Display;

pub fn info(message: impl Display) {
    println!("info: {message}");
}

pub fn warn(message: impl Display) {
    eprintln!("warning: {message}");
}

pub fn error(message: impl Display) {
    eprintln!("error: {message}");
}

pub fn error_chain(error: impl Into<anyhow::Error>) -> String {
    format!("{:#}", error.into())
}
