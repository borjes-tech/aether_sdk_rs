//! Writing to the server's log, tagged with the plugin by the kernel.

#[cfg(target_arch = "wasm32")]
mod imp {
    pub fn debug(message: &str) {
        extism_pdk::debug!("{}", message);
    }
    pub fn info(message: &str) {
        extism_pdk::info!("{}", message);
    }
    pub fn warn(message: &str) {
        extism_pdk::warn!("{}", message);
    }
    pub fn error(message: &str) {
        extism_pdk::error!("{}", message);
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    pub fn debug(message: &str) {
        eprintln!("[debug] {message}");
    }
    pub fn info(message: &str) {
        eprintln!("[info] {message}");
    }
    pub fn warn(message: &str) {
        eprintln!("[warn] {message}");
    }
    pub fn error(message: &str) {
        eprintln!("[error] {message}");
    }
}

pub fn debug(message: impl AsRef<str>) {
    imp::debug(message.as_ref());
}

pub fn info(message: impl AsRef<str>) {
    imp::info(message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    imp::warn(message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    imp::error(message.as_ref());
}
