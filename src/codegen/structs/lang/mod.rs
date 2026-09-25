//! One backend per language. `backend_for` is the only way in.

use super::{Backend, Lang};

pub mod c;
pub mod c3;
pub mod cpp;
pub mod go;
pub mod lua;
pub mod rust;

pub fn backend_for(lang: Lang) -> Box<dyn Backend> {
    match lang {
        Lang::C => Box::new(c::C),
        Lang::Cpp => Box::new(cpp::Cpp),
        Lang::Go => Box::new(go::Go),
        Lang::Rust => Box::new(rust::Rust),
        Lang::C3 => Box::new(c3::C3),
        Lang::Lua => Box::new(lua::Lua),
    }
}
